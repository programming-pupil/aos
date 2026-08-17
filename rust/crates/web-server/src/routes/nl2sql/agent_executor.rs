//! Multi-datasource NL2SQL agent executor.
//!
//! This module is intentionally kept in `web-server` rather than `nl2sql-core`:
//! it performs tenant/user authorization, LLM planning calls, datasource
//! decryption, network database execution, and SQL self-correction. The core
//! crate owns pure domain rules; this module owns application-service wiring.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

use api::{InputContentBlock, InputMessage};
use serde::{Deserialize, Serialize};
use sqlx::{Column, Row};

use crate::auth::Claims;
use crate::routes::data_sources::decrypt_config;
use crate::state::AppState;

use super::{
    build_agent_planning_prompt, correct_sql, cross_join, decode_mysql_cell, decode_pg_cell,
    discover_knowledge_schema_tables, extract_schema_tables_and_fks, full_outer_join, generate_sql,
    hash_join, load_cross_domain_clusters_summary, matched_metric_names,
    max_operational_retry_attempts, max_self_correct_attempts, parse_metric_aliases,
    parse_multi_step_plan, right_join, should_enable_qu, union_all, union_distinct,
    validate_data_source_access, CrossDatasourceRelation, DatasourceSchemaInfo, ForeignKeyPrompt,
    MetricMatchCandidate, ReferencePromptSnippet, ReferenceUsageDto, SelfCorrectContext,
    SqlExecErrorKind, SqlRepairDecision,
};

fn max_agent_steps() -> usize {
    nl2sql_domain::config::max_agent_steps()
}

fn max_rows_per_step() -> usize {
    nl2sql_domain::config::max_rows_per_step()
}

fn max_agent_response_rows() -> usize {
    std::env::var("NL2SQL_MAX_AGENT_RESPONSE_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(nl2sql_domain::config::DEFAULT_MAX_AGENT_RESPONSE_ROWS)
}

fn federated_trino_enabled() -> bool {
    std::env::var("NL2SQL_FEDERATED_TRINO_ENABLED")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true)
}

fn federated_trino_query_timeout_secs() -> u64 {
    std::env::var("NL2SQL_FEDERATED_TRINO_QUERY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 30)
        .unwrap_or(300)
}

fn agent_trino_query_timeout_secs() -> u64 {
    std::env::var("NL2SQL_AGENT_TRINO_QUERY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 30)
        .unwrap_or_else(federated_trino_query_timeout_secs)
}

fn datasource_visible_to_user(
    is_admin: bool,
    user_id: &str,
    owner_user_id: Option<&str>,
    visibility: &str,
) -> bool {
    is_admin || visibility.eq_ignore_ascii_case("tenant") || owner_user_id == Some(user_id)
}

fn federated_trino_explain_timeout_secs() -> u64 {
    std::env::var("NL2SQL_FEDERATED_TRINO_EXPLAIN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 10)
        // EXPLAIN is only a preflight. A slow or unsupported connector must
        // not consume the same budget as the real execution path, which still
        // provides authoritative validation after this soft failure.
        .unwrap_or(15)
}

fn federated_trino_explain_repair_attempts() -> usize {
    std::env::var("NL2SQL_FEDERATED_TRINO_EXPLAIN_REPAIR_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .min(max_self_correct_attempts().max(1))
}

fn federated_trino_explain_soft_fail() -> bool {
    std::env::var("NL2SQL_FEDERATED_TRINO_EXPLAIN_SOFT_FAIL")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true)
}

fn federated_trino_explain_after_execution_repair() -> bool {
    std::env::var("NL2SQL_FEDERATED_TRINO_EXPLAIN_AFTER_EXECUTION_REPAIR")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(false)
}

const EXPLAIN_PREFLIGHT_SKIPPED_PREFIX: &str = "[explain_preflight_skipped]";

fn federated_trino_explain_cooldown_secs() -> u64 {
    std::env::var("NL2SQL_FEDERATED_TRINO_EXPLAIN_COOLDOWN_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 30)
        .unwrap_or(600)
}

fn trino_explain_circuit_breaker(
) -> &'static tokio::sync::Mutex<HashMap<String, std::time::Instant>> {
    static CIRCUIT_BREAKER: OnceLock<tokio::sync::Mutex<HashMap<String, std::time::Instant>>> =
        OnceLock::new();
    CIRCUIT_BREAKER.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

const TRINO_USER_CONCURRENCY_LIMIT: usize = 3;

fn trino_user_execution_gates(
) -> &'static tokio::sync::Mutex<HashMap<String, Weak<tokio::sync::Semaphore>>> {
    static GATES: OnceLock<tokio::sync::Mutex<HashMap<String, Weak<tokio::sync::Semaphore>>>> =
        OnceLock::new();
    GATES.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

fn trino_user_execution_key(tenant_id: &str, user_id: &str) -> String {
    format!("{tenant_id}\u{1f}{user_id}")
}

pub(crate) async fn acquire_trino_user_permit(
    tenant_id: &str,
    user_id: &str,
) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
    let key = trino_user_execution_key(tenant_id, user_id);
    let gate = {
        let mut gates = trino_user_execution_gates().lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
            gate
        } else {
            let gate = Arc::new(tokio::sync::Semaphore::new(TRINO_USER_CONCURRENCY_LIMIT));
            gates.insert(key, Arc::downgrade(&gate));
            gate
        }
    };
    gate.acquire_owned()
        .await
        .map_err(|_| anyhow::anyhow!("Trino user execution gate is closed"))
}

async fn claim_trino_explain_probe(datasource_id: &str) -> bool {
    let now = std::time::Instant::now();
    let mut failures = trino_explain_circuit_breaker().lock().await;
    failures.retain(|_, until| *until > now);
    if failures.contains_key(datasource_id) {
        return false;
    }
    failures.insert(
        datasource_id.to_string(),
        now + std::time::Duration::from_secs(federated_trino_explain_cooldown_secs()),
    );
    true
}

async fn suppress_trino_explain(datasource_id: &str) {
    let until = std::time::Instant::now()
        + std::time::Duration::from_secs(federated_trino_explain_cooldown_secs());
    trino_explain_circuit_breaker()
        .lock()
        .await
        .insert(datasource_id.to_string(), until);
}

async fn clear_trino_explain_suppression(datasource_id: &str) {
    trino_explain_circuit_breaker()
        .lock()
        .await
        .remove(datasource_id);
}

fn trino_explain_preflight_was_skipped(error: &str) -> bool {
    error.starts_with(EXPLAIN_PREFLIGHT_SKIPPED_PREFIX)
}

fn max_federated_trino_datasources() -> usize {
    std::env::var("NL2SQL_FEDERATED_TRINO_MAX_DATASOURCES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
}

fn max_federated_trino_tables() -> usize {
    std::env::var("NL2SQL_FEDERATED_TRINO_MAX_TABLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(120)
}

#[derive(Debug, Clone, Deserialize)]
struct AgentTrinoConfig {
    host: String,
    port: u16,
    catalog: String,
    #[serde(default)]
    schema: String,
    #[serde(default)]
    schemas: Vec<String>,
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    ssl: Option<bool>,
    #[serde(default)]
    basic_auth: Option<bool>,
}

impl AgentTrinoConfig {
    fn effective_schema_label(&self) -> String {
        let schemas = nl2sql_domain::datasource_config::normalize_trino_schemas(
            &self.schema,
            self.schemas.iter().map(String::as_str),
        );
        schemas.join(",")
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct TrinoClusterKey {
    db_type: String,
    host: String,
    port: u16,
    username: String,
    secure: bool,
    basic_auth: bool,
    password: String,
}

#[derive(Debug, Clone)]
struct FederatedTrinoSource {
    schema: DatasourceSchemaInfo,
    cfg: AgentTrinoConfig,
    key: TrinoClusterKey,
    score: usize,
}

#[derive(Debug, Clone)]
struct FederatedTrinoWorkspace {
    key: TrinoClusterKey,
    sources: Vec<FederatedTrinoSource>,
}

impl FederatedTrinoSource {
    fn from_schema(
        schema: &DatasourceSchemaInfo,
        data_dir: &std::path::Path,
        question_tokens: &std::collections::HashSet<String>,
    ) -> Option<Self> {
        if !matches!(schema.db_type.as_str(), "presto" | "trino") {
            return None;
        }
        let config_val = decrypt_config(&schema.config, data_dir).ok()?;
        let cfg: AgentTrinoConfig = serde_json::from_value(config_val).ok()?;
        let normalized_host = nl2sql_domain::datasource_config::normalize_host_input(&cfg.host);
        let port = normalized_host.port.unwrap_or(cfg.port);
        let secure = cfg.ssl.or(normalized_host.secure).unwrap_or(port == 443);
        let basic_auth = cfg.basic_auth.unwrap_or(!cfg.password.is_empty());
        let key = TrinoClusterKey {
            db_type: "trino_presto".to_string(),
            host: normalized_host.host,
            port,
            username: cfg.username.clone(),
            secure,
            basic_auth,
            password: cfg.password.clone(),
        };
        let score = score_datasource_schema(schema, &cfg, question_tokens);
        Some(Self {
            schema: schema.clone(),
            cfg,
            key,
            score,
        })
    }
}

fn score_text_against_tokens(
    text: &str,
    question_tokens: &std::collections::HashSet<String>,
    weight: usize,
) -> usize {
    if text.trim().is_empty() || question_tokens.is_empty() {
        return 0;
    }
    let haystack = text.to_lowercase();
    question_tokens
        .iter()
        .filter(|token| haystack.contains(token.as_str()))
        .count()
        * weight
}

fn score_table_for_question(
    table: &serde_json::Value,
    question_tokens: &std::collections::HashSet<String>,
) -> usize {
    let table_name = table
        .get("table_name")
        .or_else(|| table.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let table_desc = table
        .get("ai_description")
        .or_else(|| table.get("description"))
        .or_else(|| table.get("user_description"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let mut score = score_text_against_tokens(table_name, question_tokens, 5)
        + score_text_against_tokens(table_desc, question_tokens, 2);
    if let Some(cols) = table.get("columns").and_then(|v| v.as_array()) {
        for col in cols {
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
            score += score_text_against_tokens(name, question_tokens, 3);
            score += score_text_against_tokens(desc, question_tokens, 1);
        }
    }
    score
}

fn score_datasource_schema(
    schema: &DatasourceSchemaInfo,
    cfg: &AgentTrinoConfig,
    question_tokens: &std::collections::HashSet<String>,
) -> usize {
    let mut score = score_text_against_tokens(&schema.datasource_name, question_tokens, 4)
        + score_text_against_tokens(&cfg.catalog, question_tokens, 3)
        + score_text_against_tokens(&cfg.effective_schema_label(), question_tokens, 3);
    if let Some(tables) = schema.tables.as_array() {
        score += tables
            .iter()
            .map(|t| score_table_for_question(t, question_tokens))
            .sum::<usize>();
    }
    score
}

const AGENT_SHARED_CONTEXT_MAX_CHARS: usize = 32_000;

fn contextual_agent_question(question: &str, shared_context: Option<&str>) -> String {
    let question = question.trim();
    let Some(context) = shared_context
        .map(str::trim)
        .filter(|context| !context.is_empty())
    else {
        return question.to_string();
    };
    let context = if context.chars().count() <= AGENT_SHARED_CONTEXT_MAX_CHARS {
        context.to_string()
    } else {
        let mut suffix = context
            .chars()
            .rev()
            .take(AGENT_SHARED_CONTEXT_MAX_CHARS)
            .collect::<Vec<_>>();
        suffix.reverse();
        format!(
            "...[older shared context truncated]\n{}",
            suffix.into_iter().collect::<String>()
        )
    };
    format!(
        "共享会话背景（仅用于消解代词、省略、业务对象和已知口径；不得延续或覆盖旧任务）：\n{context}\n\n用户当前问题（唯一需要执行的任务，最高优先级）：\n{question}"
    )
}

fn question_has_schema_signal(question: &str, schemas: &[DatasourceSchemaInfo]) -> bool {
    let tokens = super::reference::tokenize_for_sql_knowledge_tool(&question.to_lowercase());
    if tokens.is_empty() {
        return false;
    }
    schemas.iter().any(|schema| {
        let datasource_score = score_text_against_tokens(&schema.datasource_name, &tokens, 4);
        let best_table_score = schema
            .tables
            .as_array()
            .into_iter()
            .flatten()
            .map(|table| score_table_for_question(table, &tokens))
            .max()
            .unwrap_or_default();
        datasource_score.saturating_add(best_table_score) >= 5
    })
}

fn fully_qualified_trino_name(catalog: &str, schema: &str, table: &str) -> String {
    format!("{catalog}.{schema}.{table}")
}

fn build_federated_schema_value(
    workspace: &FederatedTrinoWorkspace,
    question_tokens: &std::collections::HashSet<String>,
) -> serde_json::Value {
    let mut tables_with_score: Vec<(usize, serde_json::Value)> = Vec::new();
    for source in &workspace.sources {
        let Some(tables) = source.schema.tables.as_array() else {
            continue;
        };
        for table in tables {
            let physical_name = table
                .get("physical_table_name")
                .or_else(|| table.get("name"))
                .or_else(|| table.get("table_name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if physical_name.trim().is_empty() {
                continue;
            }
            let mut cloned = table.clone();
            if let Some(obj) = cloned.as_object_mut() {
                let catalog = table
                    .get("catalog")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&source.cfg.catalog);
                let schema = table
                    .get("schema")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&source.cfg.schema);
                let full_name = table
                    .get("fully_qualified_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| fully_qualified_trino_name(catalog, schema, physical_name));
                obj.insert(
                    "table_name".to_string(),
                    serde_json::Value::String(full_name.clone()),
                );
                obj.insert(
                    "fully_qualified_name".to_string(),
                    serde_json::Value::String(full_name),
                );
                obj.insert(
                    "physical_table_name".to_string(),
                    serde_json::Value::String(physical_name.to_string()),
                );
                obj.insert(
                    "catalog".to_string(),
                    serde_json::Value::String(catalog.to_string()),
                );
                obj.insert(
                    "schema".to_string(),
                    serde_json::Value::String(schema.to_string()),
                );
                obj.insert(
                    "datasource_id".to_string(),
                    serde_json::Value::String(source.schema.datasource_id.clone()),
                );
                obj.insert(
                    "datasource_name".to_string(),
                    serde_json::Value::String(source.schema.datasource_name.clone()),
                );
            }
            tables_with_score.push((
                source.score + score_table_for_question(table, question_tokens),
                cloned,
            ));
        }
    }
    tables_with_score.sort_by(|a, b| b.0.cmp(&a.0));
    serde_json::Value::Array(
        tables_with_score
            .into_iter()
            .take(max_federated_trino_tables())
            .map(|(_, table)| table)
            .collect(),
    )
}

fn federated_workspace_instruction_snippet(
    workspace: &FederatedTrinoWorkspace,
) -> ReferencePromptSnippet {
    let sources = workspace
        .sources
        .iter()
        .map(|source| {
            format!(
                "- datasource=\"{}\" catalog=\"{}\" schema=\"{}\"",
                source.schema.datasource_name,
                source.cfg.catalog,
                source.cfg.effective_schema_label()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ReferencePromptSnippet {
        pack_id: "federated-trino-workspace".to_string(),
        pack_name: "Federated Trino Workspace".to_string(),
        file_id: "federated-trino-rules".to_string(),
        filename: "federated_trino_workspace.md".to_string(),
        chunk_id: "federated-trino-rules".to_string(),
        language: Some("markdown".to_string()),
        start_line: 1,
        end_line: 32,
        score: 1.0,
        reason: "federated Trino/Presto workspace execution rules".to_string(),
        chunk_type: "federated_workspace_rules".to_string(),
        verified: true,
        stale: false,
        content: format!(
            r#"Federated Trino/Presto workspace rules:
{sources}

Generate ONE executable Trino/Presto SELECT statement for the whole analysis.
Use fully qualified table names exactly as catalog.schema.table from the live schema.
Prefer CTEs for long report SQL; do not split the answer into multiple independent SQL statements.
Do not create temp tables, insert data, update data, or use DDL.
If a SQL Knowledge example uses schema.table or an old alias, rewrite it to the live catalog.schema.table names above.
Live schema wins over SQL Knowledge examples. If a table/field is absent from live schema, do not use it.
For boss-facing report analysis, aggregate before joining when possible, keep join keys explicit, and include stable metric aliases."#
        ),
    }
}

fn strip_trailing_semicolon(sql: &str) -> String {
    sql.trim().trim_end_matches(';').trim().to_string()
}

fn repair_decision_note(decision: &SqlRepairDecision) -> Option<String> {
    decision
        .rationale
        .clone()
        .or_else(|| decision.strategy.clone())
}

fn sql_attempt_decision_fields(
    decision: Option<&SqlRepairDecision>,
) -> (Option<String>, bool, bool, Option<String>) {
    decision.map_or((None, false, false, None), |decision| {
        (
            decision.strategy.clone(),
            decision.scope_changed,
            decision.diagnostic_only,
            decision.rationale.clone(),
        )
    })
}

fn annotate_attempt_with_decision(
    attempt: &mut crate::nl2sql::SqlExecutionAttempt,
    decision: &SqlRepairDecision,
) {
    let (strategy, scope_changed, diagnostic_only, rationale) =
        sql_attempt_decision_fields(Some(decision));
    attempt.repair_strategy = strategy;
    attempt.scope_changed = scope_changed;
    attempt.diagnostic_only = diagnostic_only;
    attempt.repair_rationale = rationale;
}

fn dialect_preflight_error(db_type: &str, sql: &str) -> Option<String> {
    let normalized = sql.to_ascii_lowercase();
    if matches!(db_type, "mysql" | "tidb") && normalized.contains("with recursive") {
        return Some(
            "SQL syntax is not compatible with this MySQL/TiDB datasource: WITH RECURSIVE / recursive CTE date-spine generation is not allowed. Rewrite using fact-table date aggregation, self-joins, conditional aggregation, or a non-recursive UNION ALL derived table."
                .to_string(),
        );
    }
    None
}

fn deterministic_dialect_repair(db_type: &str, sql: &str, error: &str) -> Option<String> {
    if !matches!(db_type, "presto" | "trino") {
        return None;
    }

    let normalized_error = error.to_ascii_lowercase();
    let marker = "mismatched input '";
    let token_start = normalized_error.find(marker)? + marker.len();
    let token_end = token_start + normalized_error[token_start..].find('\'')?;
    if !normalized_error[token_end..].contains("expecting: <identifier>") {
        return None;
    }

    let identifier = &error[token_start..token_end];
    if identifier.is_empty()
        || !identifier.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
        })
    {
        return None;
    }

    quote_unquoted_alias(sql, identifier, '"')
}

fn quote_unquoted_alias(sql: &str, identifier: &str, quote: char) -> Option<String> {
    let normalized_sql = sql.to_ascii_lowercase();
    let normalized_identifier = identifier.to_ascii_lowercase();
    let bytes = normalized_sql.as_bytes();
    let identifier_bytes = normalized_identifier.as_bytes();
    let code_mask = sql_code_mask(bytes);
    let mut replacements = Vec::new();
    let mut index = 0;

    while index + 2 <= bytes.len() {
        if !code_mask[index]
            || !code_mask[index + 1]
            || &bytes[index..index + 2] != b"as"
            || index > 0 && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_')
            || index + 2 >= bytes.len()
            || !bytes[index + 2].is_ascii_whitespace()
        {
            index += 1;
            continue;
        }

        let mut alias_start = index + 2;
        while alias_start < bytes.len() && bytes[alias_start].is_ascii_whitespace() {
            alias_start += 1;
        }
        let alias_end = alias_start.saturating_add(identifier_bytes.len());
        if alias_end <= bytes.len()
            && code_mask[alias_start..alias_end]
                .iter()
                .all(|is_code| *is_code)
            && &bytes[alias_start..alias_end] == identifier_bytes
            && (alias_end == bytes.len()
                || !(bytes[alias_end].is_ascii_alphanumeric() || bytes[alias_end] == b'_'))
        {
            replacements.push((alias_start, alias_end));
            index = alias_end;
        } else {
            index += 2;
        }
    }

    if replacements.is_empty() {
        return None;
    }

    let mut repaired = String::with_capacity(sql.len() + replacements.len() * 2);
    let mut cursor = 0;
    for (start, end) in replacements {
        repaired.push_str(&sql[cursor..start]);
        repaired.push(quote);
        repaired.push_str(&sql[start..end]);
        repaired.push(quote);
        cursor = end;
    }
    repaired.push_str(&sql[cursor..]);
    Some(repaired)
}

fn sql_code_mask(sql: &[u8]) -> Vec<bool> {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        SingleQuote,
        DoubleQuote,
        Backtick,
        LineComment,
        BlockComment,
    }

    let mut mask = vec![false; sql.len()];
    let mut state = State::Code;
    let mut index = 0;
    while index < sql.len() {
        match state {
            State::Code => match sql[index] {
                b'\'' => state = State::SingleQuote,
                b'"' => state = State::DoubleQuote,
                b'`' => state = State::Backtick,
                b'-' if sql.get(index + 1) == Some(&b'-') => {
                    state = State::LineComment;
                    index += 1;
                }
                b'/' if sql.get(index + 1) == Some(&b'*') => {
                    state = State::BlockComment;
                    index += 1;
                }
                _ => mask[index] = true,
            },
            State::SingleQuote => {
                if sql[index] == b'\'' {
                    if sql.get(index + 1) == Some(&b'\'') {
                        index += 1;
                    } else {
                        state = State::Code;
                    }
                }
            }
            State::DoubleQuote => {
                if sql[index] == b'"' {
                    if sql.get(index + 1) == Some(&b'"') {
                        index += 1;
                    } else {
                        state = State::Code;
                    }
                }
            }
            State::Backtick => {
                if sql[index] == b'`' {
                    if sql.get(index + 1) == Some(&b'`') {
                        index += 1;
                    } else {
                        state = State::Code;
                    }
                }
            }
            State::LineComment => {
                if matches!(sql[index], b'\n' | b'\r') {
                    state = State::Code;
                    mask[index] = true;
                }
            }
            State::BlockComment => {
                if sql[index] == b'*' && sql.get(index + 1) == Some(&b'/') {
                    index += 1;
                    state = State::Code;
                }
            }
        }
        index += 1;
    }
    mask
}

fn merge_input_error(
    inputs: &[crate::nl2sql::MergeInput],
    intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
) -> Option<String> {
    for input in inputs {
        let Some(result) = intermediate.get(&input.input_name) else {
            return Some(format!("input '{}' was not produced", input.input_name));
        };
        if let Some(error) = result.error.as_deref() {
            return Some(format!("input '{}' failed: {error}", input.input_name));
        }
    }
    None
}

#[derive(Debug, Clone)]
struct AgentSqlKnowledgeRouteCandidate {
    datasource_id: String,
    datasource_name: String,
    db_type: String,
    score: f64,
    schema_table_count: usize,
    snippets: Vec<ReferencePromptSnippet>,
}

fn agent_sql_knowledge_route_min_score() -> f64 {
    std::env::var("NL2SQL_AGENT_SQL_KNOWLEDGE_ROUTE_MIN_SCORE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(0.75)
}

fn agent_sql_knowledge_route_strong_score() -> f64 {
    std::env::var("NL2SQL_AGENT_SQL_KNOWLEDGE_ROUTE_STRONG_SCORE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(2.5)
}

fn agent_sql_knowledge_route_max_sources() -> usize {
    std::env::var("NL2SQL_AGENT_SQL_KNOWLEDGE_ROUTE_MAX_SOURCES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(48)
}

fn schema_table_count(schema: &DatasourceSchemaInfo) -> usize {
    schema.tables.as_array().map(|arr| arr.len()).unwrap_or(0)
}

fn score_agent_sql_knowledge_snippets(snippets: &[ReferencePromptSnippet]) -> f64 {
    let mut scores = snippets
        .iter()
        .filter(|snippet| !snippet.stale)
        .map(|snippet| snippet.score)
        .collect::<Vec<_>>();
    scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let Some(top) = scores.first().copied() else {
        return 0.0;
    };
    let supporting = scores.iter().skip(1).take(4).sum::<f64>() * 0.25;
    top + supporting + (scores.len().min(4) as f64 * 0.08)
}

fn should_use_agent_sql_knowledge_route(
    candidate: &AgentSqlKnowledgeRouteCandidate,
    runner_up_score: Option<f64>,
) -> bool {
    if candidate.score < agent_sql_knowledge_route_min_score() {
        return false;
    }
    if candidate.score >= agent_sql_knowledge_route_strong_score() {
        return true;
    }
    if candidate.schema_table_count == 0 {
        return true;
    }
    match runner_up_score {
        Some(second) => candidate.score >= second + 0.35 || candidate.score >= second * 1.18,
        None => true,
    }
}

fn format_agent_sql_knowledge_context(snippets: &[ReferencePromptSnippet]) -> String {
    if snippets.is_empty() {
        return "(no SQL Knowledge references selected for planning)".to_string();
    }
    snippets
        .iter()
        .take(8)
        .enumerate()
        .map(|(idx, snippet)| {
            let mut content = snippet.content.clone();
            const MAX_CHARS: usize = 4_000;
            if content.chars().count() > MAX_CHARS {
                content = content.chars().take(MAX_CHARS).collect::<String>();
                content.push_str("\n...[truncated]");
            }
            format!(
                "[ref-{n}] file=\"{file}\" lines={start}-{end} type={chunk_type} score={score:.2} reason=\"{reason}\"\n{content}",
                n = idx + 1,
                file = snippet.filename,
                start = snippet.start_line,
                end = snippet.end_line,
                chunk_type = snippet.chunk_type,
                score = snippet.score,
                reason = snippet.reason,
                content = content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// POST /api/v1/nl2sql/agent/execute — multi-datasource progressive query.
#[derive(Debug, Deserialize)]
pub struct AgentExecuteRequest {
    pub question: String,
    /// Optional concise query used only for datasource routing and SQL knowledge
    /// retrieval. The full `question` remains the model task. Deadline-bound
    /// orchestrators use this to keep their control instructions out of search.
    #[serde(default)]
    pub retrieval_question: Option<String>,
    /// Model selected by the calling assistant turn. The NL2SQL runtime still
    /// resolves and authorizes keys server-side and uses other keys on failure.
    #[serde(default)]
    pub preferred_model: Option<String>,
    /// Optional prior conversation context. It is never treated as the current
    /// question by deterministic routing; it only resolves follow-up ellipsis
    /// and is clearly delimited in model prompts.
    #[serde(default)]
    pub shared_context: Option<String>,
    /// Optional list of allowed datasource IDs. If empty, all accessible datasources
    /// are candidates. When provided, only these datasources will be used and the
    /// user must have access to each one.
    #[serde(default)]
    pub datasource_ids: Vec<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub max_steps: Option<usize>,
    /// Keep deterministic hybrid retrieval and full-file evidence, but avoid
    /// an additional open-ended LLM tool-navigation loop. Deadline-bound
    /// callers such as attribution use this to reserve time for SQL execution.
    #[serde(default)]
    pub bounded: bool,
}

/// Caps concurrent datasource work inside one top-level request. This is not a
/// lifetime submission quota: a completed request releases its permit so a
/// later drill-down can proceed. Trino also has the tenant-and-user scoped gate
/// above, which is the authoritative cross-task concurrency limit.
#[derive(Debug)]
pub(crate) struct DatasourceRequestBudget {
    in_flight: Arc<tokio::sync::Semaphore>,
}

impl DatasourceRequestBudget {
    pub(crate) fn new(max_requests: usize) -> Arc<Self> {
        let max_requests = max_requests.max(1);
        Arc::new(Self {
            in_flight: Arc::new(tokio::sync::Semaphore::new(max_requests)),
        })
    }

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        operation: &str,
    ) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
        self.in_flight
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("datasource concurrency gate closed before {operation}"))
    }
}

struct TrinoSubmissionGuard {
    client: Option<Arc<trino_rust_client::Client>>,
    query_id: Arc<std::sync::Mutex<Option<String>>>,
    user_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    unresolved_hold: std::time::Duration,
    completed: bool,
}

impl TrinoSubmissionGuard {
    fn new(
        client: Arc<trino_rust_client::Client>,
        user_permit: tokio::sync::OwnedSemaphorePermit,
        unresolved_hold: std::time::Duration,
    ) -> Self {
        Self {
            client: Some(client),
            query_id: Arc::new(std::sync::Mutex::new(None)),
            user_permit: Some(user_permit),
            unresolved_hold,
            completed: false,
        }
    }

    fn record_query_id(&self, query_id: &str) {
        *self
            .query_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(query_id.to_string());
    }

    fn complete(&mut self) {
        self.completed = true;
    }

    async fn cancel(&mut self, reason: &str) {
        let query_id = self
            .query_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let (Some(client), Some(query_id)) = (self.client.as_ref(), query_id) {
            match tokio::time::timeout(std::time::Duration::from_secs(10), client.cancel(&query_id))
                .await
            {
                Ok(Ok(())) => {
                    tracing::warn!(query_id, reason, "cancelled Trino query");
                    self.completed = true;
                }
                Ok(Err(error)) => {
                    tracing::warn!(query_id, reason, error = %error, "Trino query cancellation failed")
                }
                Err(_) => tracing::warn!(query_id, reason, "Trino query cancellation timed out"),
            }
        }
    }
}

impl Drop for TrinoSubmissionGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let Some(client) = self.client.take() else {
            return;
        };
        let query_id = self
            .query_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let user_permit = self.user_permit.take();
        let unresolved_hold = self.unresolved_hold;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            if let Some(query_id) = query_id {
                let cancelled = match tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client.cancel(&query_id),
                )
                .await
                {
                    Ok(Ok(())) => {
                        tracing::warn!(query_id, "cancelled abandoned Trino query");
                        true
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(query_id, error = %error, "abandoned Trino query cancellation failed");
                        false
                    }
                    Err(_) => {
                        tracing::warn!(query_id, "abandoned Trino query cancellation timed out");
                        false
                    }
                };
                if !cancelled {
                    tokio::time::sleep(unresolved_hold).await;
                }
            } else {
                // The POST may have reached Trino before its first response was
                // cancelled. Keep the user's slot for the original execution
                // budget instead of allowing an immediate replacement burst.
                tokio::time::sleep(unresolved_hold).await;
            }
            drop(user_permit);
        });
    }
}

pub(crate) async fn execute_trino_query_bounded(
    client: trino_rust_client::Client,
    sql: String,
    timeout_secs: u64,
    tenant_id: &str,
    user_id: &str,
    operation: &str,
    request_budget: Arc<DatasourceRequestBudget>,
) -> anyhow::Result<trino_rust_client::DataSet<trino_rust_client::Row>> {
    let _request_permit = request_budget.acquire(operation).await?;
    let user_permit = acquire_trino_user_permit(tenant_id, user_id).await?;
    let client = Arc::new(client);
    let unresolved_hold = std::time::Duration::from_secs(timeout_secs.max(1));
    let mut guard = TrinoSubmissionGuard::new(client.clone(), user_permit, unresolved_hold);
    // This is an inactivity window, not a wall-clock query lifetime. Slow
    // Trino statements may legitimately run for minutes while every response
    // still reports progress. Renew the window after each server response and
    // cancel only when the server becomes silent.
    let inactivity_timeout = std::time::Duration::from_secs(timeout_secs.max(1));

    super::agent_async::emit_agent_stage_detail(
        "execute_sql",
        "SQL 已生成，正在提交到数据源",
        serde_json::json!({
            "kind": "sql",
            "sql": sql.clone(),
            "status": "submitting",
        }),
    );

    tracing::info!(
        tenant_id,
        user_id,
        operation,
        "submitting bounded Trino query"
    );

    let mut response = match tokio::time::timeout(
        inactivity_timeout,
        client.get::<trino_rust_client::Row>(sql),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return Err(anyhow::anyhow!("trino query failed: {error}")),
        Err(_) => {
            guard.cancel("initial response timeout").await;
            return Err(anyhow::anyhow!(
                "Query execution timed out after {timeout_secs}s"
            ));
        }
    };
    guard.record_query_id(&response.id);
    super::agent_async::emit_agent_stage_detail(
        "execute_sql",
        "数据源已接受查询，正在运行",
        serde_json::json!({
            "kind": "query_progress",
            "queryId": response.id.clone(),
            "status": response.stats.state.clone(),
            "queued": response.stats.queued,
            "scheduled": response.stats.scheduled,
            "totalSplits": response.stats.total_splits,
            "queuedSplits": response.stats.queued_splits,
            "runningSplits": response.stats.running_splits,
            "completedSplits": response.stats.completed_splits,
            "processedRows": response.stats.processed_rows,
            "processedBytes": response.stats.processed_bytes,
            "elapsedMs": response.stats.elapsed_time_millis,
        }),
    );
    tracing::info!(
        tenant_id,
        user_id,
        operation,
        query_id = %response.id,
        "Trino query accepted"
    );

    let mut columns = response.columns.take();
    let mut rows = Vec::new();
    let mut last_progress_emit = std::time::Instant::now();
    loop {
        if let Some(error) = response.error.take() {
            guard.complete();
            return Err(anyhow::anyhow!("trino query failed: {error}"));
        }
        if let Some(data) = response.data.take() {
            match data {
                trino_rust_client::QueryResultData::Direct(data) => rows.extend(data),
                trino_rust_client::QueryResultData::Spooled(_) => {
                    guard.cancel("unsupported spooled response").await;
                    return Err(anyhow::anyhow!(
                        "trino query failed: server returned spooled data but this build does not enable spooling"
                    ));
                }
            }
        }
        let Some(next_uri) = response.next_uri.take() else {
            break;
        };
        response = match tokio::time::timeout(
            inactivity_timeout,
            client.get_next::<trino_rust_client::Row>(&next_uri),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                guard.cancel("result polling failed").await;
                return Err(anyhow::anyhow!("trino query failed: {error}"));
            }
            Err(_) => {
                guard.cancel("result polling timeout").await;
                return Err(anyhow::anyhow!(
                    "Query execution timed out after {timeout_secs}s"
                ));
            }
        };
        if columns.is_none() {
            columns = response.columns.take();
        }
        if last_progress_emit.elapsed() >= std::time::Duration::from_secs(5)
            || response.next_uri.is_none()
        {
            super::agent_async::emit_agent_stage_detail(
                "execute_sql",
                "查询仍在运行，已收到最新进度",
                serde_json::json!({
                    "kind": "query_progress",
                    "queryId": response.id.clone(),
                    "status": response.stats.state.clone(),
                    "queued": response.stats.queued,
                    "scheduled": response.stats.scheduled,
                    "totalSplits": response.stats.total_splits,
                    "queuedSplits": response.stats.queued_splits,
                    "runningSplits": response.stats.running_splits,
                    "completedSplits": response.stats.completed_splits,
                    "processedRows": response.stats.processed_rows,
                    "processedBytes": response.stats.processed_bytes,
                    "elapsedMs": response.stats.elapsed_time_millis,
                    "receivedRows": rows.len(),
                }),
            );
            last_progress_emit = std::time::Instant::now();
        }
    }

    super::agent_async::emit_agent_stage_detail(
        "execute_sql",
        "SQL 执行完成，正在整理结果",
        serde_json::json!({
            "kind": "query_result",
            "queryId": response.id.clone(),
            "status": "completed",
            "rowCount": rows.len(),
            "columns": columns.as_ref().map(|items| items.iter().map(|column| column.name.clone()).collect::<Vec<_>>()).unwrap_or_default(),
        }),
    );
    guard.complete();
    trino_rust_client::build_dataset(rows, columns)
        .map_err(|error| anyhow::anyhow!("trino query failed: {error}"))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExecuteResponse {
    pub steps: Vec<StepExecutionDetail>,
    pub final_result: FinalAgentResult,
    pub total_execution_ms: u64,
    pub total_steps: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_references: Vec<ReferenceUsageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepExecutionDetail {
    pub step_id: usize,
    pub step_type: String,
    pub datasource_id: Option<String>,
    pub description: String,
    pub output_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub row_count: usize,
    pub execution_ms: u64,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_attempts: Vec<crate::nl2sql::SqlExecutionAttempt>,
    #[serde(default)]
    pub diagnostic_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalAgentResult {
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub row_count: usize,
}

// ── Nl2SqlAgent ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Nl2SqlAgent {
    state: Arc<AppState>,
    max_steps: usize,
    max_rows_per_step: usize,
    preferred_model: Option<String>,
    bounded: bool,
    network_budget: Arc<DatasourceRequestBudget>,
    protected_request: bool,
}

#[derive(Clone)]
struct AgentSemanticGuard {
    conversation_id: String,
    intent_id: String,
    datasource_id: String,
    intent: nl2sql_core::semantic_ir::AnalyticIntentIR,
    metric_contracts: Vec<nl2sql_core::semantic_ir::MetricContract>,
    join_contracts: Vec<nl2sql_core::semantic_ir::JoinContract>,
}

impl AgentSemanticGuard {
    fn intent_json(&self) -> anyhow::Result<String> {
        serde_json::to_string(&self.intent)
            .map_err(|error| anyhow::anyhow!("failed to serialize analytic intent: {error}"))
    }
}

fn audit_agent_semantic_candidate(
    guard: &AgentSemanticGuard,
    sql: &str,
) -> anyhow::Result<super::semantic_audit::SemanticAudit> {
    super::semantic_audit::compile_canonical_intent_with_contracts_and_joins(
        &guard.intent,
        sql,
        &guard.metric_contracts,
        &guard.join_contracts,
    )
    .ok_or_else(|| anyhow::anyhow!("SQL candidate could not be parsed for semantic audit"))
}

fn reference_evidence_columns(snippets: &[ReferencePromptSnippet]) -> Vec<String> {
    let mut columns = snippets
        .iter()
        .flat_map(|snippet| super::extract_columns_from_sql(&snippet.content))
        .collect::<Vec<_>>();
    columns.sort_unstable_by_key(|column| column.to_ascii_lowercase());
    columns.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    columns
}

impl Nl2SqlAgent {
    pub fn new(state: Arc<AppState>, preferred_model: Option<String>, bounded: bool) -> Self {
        Self {
            state,
            max_steps: max_agent_steps(),
            max_rows_per_step: max_rows_per_step(),
            preferred_model: preferred_model.filter(|model| !model.trim().is_empty()),
            bounded,
            network_budget: DatasourceRequestBudget::new(3),
            protected_request: false,
        }
    }

    pub fn with_network_budget(
        state: Arc<AppState>,
        preferred_model: Option<String>,
        bounded: bool,
        network_budget: Arc<DatasourceRequestBudget>,
    ) -> Self {
        Self {
            state,
            max_steps: max_agent_steps(),
            max_rows_per_step: max_rows_per_step(),
            preferred_model: preferred_model.filter(|model| !model.trim().is_empty()),
            bounded,
            network_budget,
            protected_request: true,
        }
    }

    async fn prepare_semantic_guard(
        &self,
        claims: &Claims,
        conversation_id: &str,
        intent_id: &str,
        datasource_id: &str,
        question: &str,
        schema: &serde_json::Value,
        matched_metrics: &[String],
        evidence_columns: &[String],
    ) -> anyhow::Result<AgentSemanticGuard> {
        let understanding = if super::should_enable_qu() {
            match crate::nl2sql::resolve_chat_config(
                self.state.config_registry(),
                &claims.tenant_id,
                &claims.sub,
                &self.state.default_model,
                Some("nl2sql"),
            )
            .await
            {
                Ok(config) => {
                    let service = crate::nl2sql::query_understanding::QueryUnderstanding::new(
                        self.state.db.clone(),
                        config,
                    );
                    match service
                        .understand(question, datasource_id, &claims.tenant_id, schema)
                        .await
                    {
                        Ok(result) => Some(result),
                        Err(error) => {
                            tracing::warn!(
                                datasource_id,
                                error = %error,
                                "agent semantic compiler query-understanding proposal failed; using deterministic proposal"
                            );
                            None
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        datasource_id,
                        error = %error,
                        "agent semantic compiler could not resolve query-understanding model; using deterministic proposal"
                    );
                    None
                }
            }
        } else {
            None
        };
        let durable = super::semantic_audit::compile_bind_and_persist_intent(
            &self.state.db,
            &claims.tenant_id,
            datasource_id,
            conversation_id,
            intent_id,
            question,
            matched_metrics,
            schema,
            evidence_columns,
            understanding.as_ref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to persist analytic intent: {error}"))?;
        Ok(AgentSemanticGuard {
            conversation_id: conversation_id.to_string(),
            intent_id: intent_id.to_string(),
            datasource_id: datasource_id.to_string(),
            intent: durable.intent,
            metric_contracts: durable.metric_contracts,
            join_contracts: durable.join_contracts,
        })
    }

    async fn matched_metrics_for_schemas(
        &self,
        tenant_id: &str,
        question: &str,
        schemas: &[DatasourceSchemaInfo],
    ) -> Vec<String> {
        let mut candidates = Vec::new();
        for schema in schemas {
            let rows: Vec<(
                String,
                Option<serde_json::Value>,
                Option<String>,
                Option<serde_json::Value>,
            )> = sqlx::query_as(
                "SELECT metric_name, metric_aliases, expression, filter_conditions
                 FROM nl2sql_metrics
                 WHERE tenant_id = ? AND datasource_id = ? AND status = 'published' AND deleted_at IS NULL",
            )
            .bind(tenant_id)
            .bind(&schema.datasource_id)
            .fetch_all(&self.state.db)
            .await
            .unwrap_or_default();
            candidates.extend(rows.into_iter().map(
                |(name, aliases, expression, filter_conditions)| MetricMatchCandidate {
                    name,
                    aliases: parse_metric_aliases(aliases.as_ref()),
                    expression,
                    filter_conditions,
                },
            ));
        }
        let mut matched = matched_metric_names(question, &candidates);
        matched.sort_unstable();
        matched.dedup();
        matched
    }

    async fn verify_semantic_candidate(
        &self,
        claims: &Claims,
        guard: &AgentSemanticGuard,
        sql: &str,
        repair: bool,
    ) -> anyhow::Result<()> {
        let audit = audit_agent_semantic_candidate(guard, sql)?;
        let verification = super::semantic_audit::verification_json(&audit);
        let release_decision = serde_json::to_string(&audit.verification.release_decision)
            .unwrap_or_else(|_| "\"Reject\"".to_string())
            .trim_matches('"')
            .to_string();
        if repair {
            crate::semantic_kernel_store::persist_nl2sql_repair_verification(
                &self.state.db,
                &claims.tenant_id,
                &guard.intent_id,
                sql,
                &verification,
                &release_decision,
                f64::from(audit.verification.confidence_basis.calibrated_score),
            )
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to persist repaired SQL verification: {error}")
            })?;
        } else {
            crate::semantic_kernel_store::persist_nl2sql_semantic_audit(
                &self.state.db,
                &claims.tenant_id,
                &guard.datasource_id,
                &guard.conversation_id,
                &guard.intent_id,
                &serde_json::to_value(&guard.intent).map_err(|error| {
                    anyhow::anyhow!("failed to encode canonical analytic intent: {error}")
                })?,
                &verification,
                &release_decision,
                f64::from(audit.verification.confidence_basis.calibrated_score),
            )
            .await
            .map_err(|error| anyhow::anyhow!("failed to persist SQL semantic audit: {error}"))?;
        }
        super::semantic_audit::require_execution_validation_decision(&release_decision)
            .map_err(anyhow::Error::msg)
    }

    async fn datasource_has_direct_sql_knowledge(
        &self,
        tenant_id: &str,
        datasource_id: &str,
    ) -> bool {
        sqlx::query_scalar::<_, i32>(
            "SELECT 1 \
             FROM nl2sql_reference_packs p \
             JOIN nl2sql_reference_files f ON f.tenant_id = p.tenant_id AND f.pack_id = p.id \
             WHERE p.tenant_id = ? AND p.enabled = 1 AND f.status = 'indexed' \
               AND (p.datasource_id = ? OR f.datasource_id = ? OR EXISTS ( \
                    SELECT 1 FROM json_each( \
                        CASE WHEN json_valid(p.datasource_bindings_json) \
                             THEN p.datasource_bindings_json ELSE '[]' END \
                    ) WHERE value = ? \
               )) \
             LIMIT 1",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(datasource_id)
        .bind(datasource_id)
        .fetch_optional(&self.state.db)
        .await
        .ok()
        .flatten()
        .is_some()
    }

    async fn resolve_sql_knowledge_route_candidate(
        &self,
        claims: &Claims,
        question: &str,
        schemas: &[DatasourceSchemaInfo],
    ) -> Option<AgentSqlKnowledgeRouteCandidate> {
        super::agent_async::emit_agent_stage(
            "sql_knowledge_probe",
            "正在用 SQL 知识库辅助选择数据源",
        );
        let mut candidates = Vec::new();
        for schema in schemas.iter().take(agent_sql_knowledge_route_max_sources()) {
            if !self
                .datasource_has_direct_sql_knowledge(&claims.tenant_id, &schema.datasource_id)
                .await
            {
                continue;
            }
            let snippets = match super::reference::resolve_auto_query_references(
                &self.state,
                &claims.tenant_id,
                &schema.datasource_id,
                question,
                5,
            )
            .await
            {
                Ok(snippets) => snippets
                    .into_iter()
                    .filter(|snippet| !snippet.stale)
                    .collect::<Vec<_>>(),
                Err(e) => {
                    tracing::warn!(
                        tenant_id = %claims.tenant_id,
                        datasource_id = %schema.datasource_id,
                        error = %e,
                        "agent SQL knowledge route lookup failed"
                    );
                    continue;
                }
            };
            let score = score_agent_sql_knowledge_snippets(&snippets);
            if score <= 0.0 {
                continue;
            }
            candidates.push(AgentSqlKnowledgeRouteCandidate {
                datasource_id: schema.datasource_id.clone(),
                datasource_name: schema.datasource_name.clone(),
                db_type: schema.db_type.clone(),
                score,
                schema_table_count: schema_table_count(schema),
                snippets,
            });
        }

        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.schema_table_count.cmp(&a.schema_table_count))
        });
        let best = candidates.first().cloned()?;
        let runner_up_score = candidates.get(1).map(|c| c.score);
        if should_use_agent_sql_knowledge_route(&best, runner_up_score) {
            tracing::info!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                datasource_id = %best.datasource_id,
                datasource_name = %best.datasource_name,
                db_type = %best.db_type,
                score = best.score,
                runner_up_score = runner_up_score.unwrap_or(0.0),
                snippet_count = best.snippets.len(),
                schema_table_count = best.schema_table_count,
                "agent SQL knowledge route selected datasource"
            );
            Some(best)
        } else {
            tracing::info!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                top_datasource_id = %best.datasource_id,
                top_score = best.score,
                runner_up_score = runner_up_score.unwrap_or(0.0),
                "agent SQL knowledge route candidate was not confident enough"
            );
            None
        }
    }

    fn schema_ids_for_sql_knowledge_route(
        &self,
        route: &AgentSqlKnowledgeRouteCandidate,
        schemas: &[DatasourceSchemaInfo],
        question: &str,
    ) -> HashSet<String> {
        let mut ids = HashSet::from([route.datasource_id.clone()]);
        if route.schema_table_count == 0 || !matches!(route.db_type.as_str(), "presto" | "trino") {
            return ids;
        }
        let question_tokens =
            super::reference::tokenize_for_sql_knowledge_tool(&question.to_lowercase());
        let Some(selected_source) = schemas
            .iter()
            .find(|schema| schema.datasource_id == route.datasource_id)
            .and_then(|schema| {
                FederatedTrinoSource::from_schema(schema, &self.state.data_dir, &question_tokens)
            })
        else {
            return ids;
        };
        let mut same_cluster = schemas
            .iter()
            .filter_map(|schema| {
                let source = FederatedTrinoSource::from_schema(
                    schema,
                    &self.state.data_dir,
                    &question_tokens,
                )?;
                (source.key == selected_source.key && schema_table_count(schema) > 0)
                    .then_some(source)
            })
            .collect::<Vec<_>>();
        same_cluster.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.schema.datasource_name.cmp(&b.schema.datasource_name))
        });
        for source in same_cluster
            .into_iter()
            .take(max_federated_trino_datasources().max(1))
        {
            ids.insert(source.schema.datasource_id);
        }
        ids
    }

    fn build_federated_trino_workspace(
        &self,
        question: &str,
        schemas: &[DatasourceSchemaInfo],
        explicit_datasource_scope: bool,
    ) -> Option<(FederatedTrinoWorkspace, std::collections::HashSet<String>)> {
        if !federated_trino_enabled() {
            return None;
        }
        let question_tokens =
            super::reference::tokenize_for_sql_knowledge_tool(&question.to_lowercase());
        let mut groups: HashMap<TrinoClusterKey, Vec<FederatedTrinoSource>> = HashMap::new();
        for schema in schemas {
            if let Some(source) =
                FederatedTrinoSource::from_schema(schema, &self.state.data_dir, &question_tokens)
            {
                groups.entry(source.key.clone()).or_default().push(source);
            }
        }

        let all_candidates_are_trino_like = schemas
            .iter()
            .all(|s| matches!(s.db_type.as_str(), "presto" | "trino"));

        let mut workspaces: Vec<FederatedTrinoWorkspace> = groups
            .into_iter()
            .filter_map(|(key, mut sources)| {
                if sources.len() < 2 {
                    return None;
                }
                let total_score: usize = sources.iter().map(|s| s.score).sum();
                if total_score == 0 && !explicit_datasource_scope && !all_candidates_are_trino_like
                {
                    return None;
                }
                sources.sort_by(|a, b| {
                    b.score
                        .cmp(&a.score)
                        .then_with(|| a.schema.datasource_name.cmp(&b.schema.datasource_name))
                });
                sources.truncate(max_federated_trino_datasources().max(2));
                Some(FederatedTrinoWorkspace { key, sources })
            })
            .collect();

        workspaces.sort_by(|a, b| {
            let score_a: usize = a.sources.iter().map(|s| s.score).sum();
            let score_b: usize = b.sources.iter().map(|s| s.score).sum();
            score_b
                .cmp(&score_a)
                .then_with(|| b.sources.len().cmp(&a.sources.len()))
        });

        workspaces
            .into_iter()
            .next()
            .map(|workspace| (workspace, question_tokens))
    }

    async fn try_execute_federated_trino(
        &self,
        claims: &Claims,
        question: &str,
        schemas: &[DatasourceSchemaInfo],
        explicit_datasource_scope: bool,
        conversation_id: &str,
        query_id: &str,
    ) -> anyhow::Result<Option<AgentExecuteResponse>> {
        let Some((workspace, question_tokens)) =
            self.build_federated_trino_workspace(question, schemas, explicit_datasource_scope)
        else {
            return Ok(None);
        };

        let start = std::time::Instant::now();
        super::agent_async::emit_agent_stage(
            "federated_workspace",
            "检测到同一 Trino/Presto 集群，启用联邦 SQL 工作区",
        );
        tracing::info!(
            source_count = workspace.sources.len(),
            db_type = %workspace.key.db_type,
            host = %workspace.key.host,
            port = workspace.key.port,
            "nl2sql federated Trino workspace selected"
        );

        let schema = build_federated_schema_value(&workspace, &question_tokens);
        if schema.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            return Ok(Some(
                self.federated_error_response(
                    start,
                    &workspace,
                    None,
                    "federated Trino workspace has no discovered schema tables; fetch schema first"
                        .to_string(),
                    Vec::new(),
                ),
            ));
        }

        super::agent_async::emit_agent_stage("load_context", "正在检索 SQL 知识库和联邦 Schema");
        let reference_snippets = self
            .resolve_federated_references(claims, question, &workspace)
            .await;
        let mut used_references = reference_snippets
            .iter()
            .map(ReferencePromptSnippet::to_usage_dto)
            .collect::<Vec<_>>();

        super::agent_async::emit_agent_stage("generate_sql", "正在生成联邦 Trino SQL");
        let mut prompt_references = vec![federated_workspace_instruction_snippet(&workspace)];
        prompt_references.extend(reference_snippets.clone());
        let execution_source = &workspace.sources[0];

        let history = super::ConversationHistory {
            messages: Vec::new(),
            summary: Some(format!(
                "Federated Trino/Presto workspace over {} datasource(s). Generate one SELECT using catalog.schema.table.",
                workspace.sources.len()
            )),
        };
        let matched_metrics = self
            .matched_metrics_for_schemas(&claims.tenant_id, question, schemas)
            .await;
        let evidence_columns = reference_evidence_columns(&reference_snippets);
        let semantic_guard = self
            .prepare_semantic_guard(
                claims,
                conversation_id,
                query_id,
                &execution_source.schema.datasource_id,
                question,
                &schema,
                &matched_metrics,
                &evidence_columns,
            )
            .await?;
        let semantic_intent_json = semantic_guard.intent_json()?;
        let sql_result = match generate_sql(
            &self.state,
            claims,
            Some(&execution_source.schema.datasource_id),
            question,
            &schema,
            &[],
            &[],
            history,
            None,
            None,
            "trino",
            schema.as_array().map(|a| a.len() > 12).unwrap_or(false),
            &[],
            &[],
            &prompt_references,
            None,
            self.preferred_model.as_deref(),
            !self.bounded,
            &semantic_intent_json,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                return Ok(Some(self.federated_error_response(
                    start,
                    &workspace,
                    None,
                    format!("federated Trino SQL generation failed: {e}"),
                    used_references,
                )));
            }
        };

        if !sql_result.tool_reference_snippets.is_empty() {
            used_references.extend(
                sql_result
                    .tool_reference_snippets
                    .iter()
                    .map(ReferencePromptSnippet::to_usage_dto),
            );
        }

        if let Some(question) = sql_result.clarification_question {
            return Ok(Some(self.federated_error_response(
                start,
                &workspace,
                None,
                format!("需要澄清：{question}"),
                used_references,
            )));
        }

        let mut current_sql = strip_trailing_semicolon(&sql_result.sql);
        if current_sql.is_empty() {
            return Ok(Some(self.federated_error_response(
                start,
                &workspace,
                None,
                "federated Trino SQL generation returned empty SQL".to_string(),
                used_references,
            )));
        }

        super::agent_async::emit_agent_stage_detail(
            "generated_sql",
            "联邦 SQL 已生成",
            serde_json::json!({
                "kind": "sql",
                "sql": current_sql.clone(),
                "status": "generated",
            }),
        );

        if !self.bounded {
            super::agent_async::emit_agent_stage("explain_sql", "正在 EXPLAIN 校验联邦 SQL");
            match self
                .explain_and_repair_federated_sql(
                    claims,
                    question,
                    &schema,
                    &execution_source.schema.datasource_id,
                    &execution_source.schema.config,
                    &mut current_sql,
                    &semantic_guard,
                )
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    if federated_trino_explain_soft_fail() {
                        if trino_explain_preflight_was_skipped(&e) {
                            tracing::info!(
                                datasource_id = %execution_source.schema.datasource_id,
                                "federated Trino EXPLAIN preflight skipped during datasource cooldown; continuing to execution-stage validation"
                            );
                        } else {
                            tracing::warn!(
                                error = %e,
                                "federated Trino EXPLAIN preflight did not pass; continuing to execution-stage validation"
                            );
                        }
                        super::agent_async::emit_agent_stage(
                            "explain_sql",
                            if trino_explain_preflight_was_skipped(&e) {
                                "已跳过近期不可用的 EXPLAIN，直接进入执行阶段验证"
                            } else {
                                "EXPLAIN 未通过，继续进入执行阶段验证和修复"
                            },
                        );
                    } else {
                        return Ok(Some(self.federated_error_response(
                            start,
                            &workspace,
                            Some(current_sql),
                            e,
                            used_references,
                        )));
                    }
                }
            }
        }

        if let Err(error) = self
            .verify_semantic_candidate(claims, &semantic_guard, &current_sql, false)
            .await
        {
            return Ok(Some(self.federated_error_response(
                start,
                &workspace,
                Some(current_sql),
                format!("federated SQL semantic verification failed: {error}"),
                used_references,
            )));
        }

        super::agent_async::emit_agent_stage("execute_sql", "正在执行联邦 Trino SQL");
        match self
            .execute_federated_sql_with_repair(
                claims,
                question,
                &schema,
                &execution_source.schema.datasource_id,
                &execution_source.schema.config,
                &mut current_sql,
                &semantic_guard,
            )
            .await
        {
            Ok((columns, rows, execution_attempts)) => {
                let row_count = rows.len();
                let diagnostic_only = execution_attempts
                    .last()
                    .is_some_and(|attempt| attempt.diagnostic_only);
                let recovery_note = execution_attempts.last().and_then(|attempt| {
                    attempt
                        .repair_rationale
                        .clone()
                        .or_else(|| attempt.repair_strategy.clone())
                });
                let step = StepExecutionDetail {
                    step_id: 0,
                    step_type: "federated_trino_query".to_string(),
                    datasource_id: Some(execution_source.schema.datasource_id.clone()),
                    description: format!(
                        "Federated Trino SQL over {} datasource(s)",
                        workspace.sources.len()
                    ),
                    output_name: "federated_result".to_string(),
                    sql: Some(current_sql),
                    columns: columns.clone(),
                    rows: rows.clone(),
                    row_count,
                    execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    error: None,
                    execution_attempts,
                    diagnostic_only,
                    recovery_note,
                };
                Ok(Some(AgentExecuteResponse {
                    steps: vec![step],
                    final_result: FinalAgentResult {
                        columns,
                        rows,
                        row_count,
                    },
                    total_execution_ms: u64::try_from(start.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    total_steps: 1,
                    used_references,
                    conversation_id: None,
                    query_id: None,
                    error: None,
                }))
            }
            Err((error, execution_attempts)) => {
                let mut response = self.federated_error_response(
                    start,
                    &workspace,
                    Some(current_sql),
                    error,
                    used_references,
                );
                if let Some(step) = response.steps.first_mut() {
                    step.execution_attempts = execution_attempts;
                }
                Ok(Some(response))
            }
        }
    }

    async fn resolve_federated_references(
        &self,
        claims: &Claims,
        question: &str,
        workspace: &FederatedTrinoWorkspace,
    ) -> Vec<ReferencePromptSnippet> {
        let mut snippets: Vec<ReferencePromptSnippet> = Vec::new();
        for source in workspace
            .sources
            .iter()
            .take(max_federated_trino_datasources())
        {
            match super::reference::resolve_auto_query_references(
                &self.state,
                &claims.tenant_id,
                &source.schema.datasource_id,
                question,
                4,
            )
            .await
            {
                Ok(mut refs) => snippets.append(&mut refs),
                Err(e) => tracing::warn!(
                    error = %e,
                    datasource_id = %source.schema.datasource_id,
                    "federated Trino reference retrieval failed"
                ),
            }
        }

        if !super::should_enable_sql_generation_tool_loop() {
            if let Ok(mut chat_candidates) = crate::nl2sql::resolve_chat_config_candidates(
                self.state.config_registry(),
                &claims.tenant_id,
                &claims.sub,
                &self.state.default_model,
                Some("nl2sql"),
            )
            .await
            {
                crate::nl2sql::prioritize_chat_candidates(
                    &mut chat_candidates,
                    self.preferred_model.as_deref(),
                );
                if let Some(chat_cfg) = chat_candidates.first() {
                    for source in workspace.sources.iter().take(3) {
                        let mut tool_refs = super::run_sql_knowledge_tool_prefetch(
                            &self.state,
                            claims,
                            &source.schema.datasource_id,
                            question,
                            &source.schema.tables,
                            chat_cfg,
                        )
                        .await;
                        snippets.append(&mut tool_refs);
                    }
                }
            }
        }

        let merged = super::merge_reference_snippets(&[], snippets, 12);
        if !merged.is_empty() {
            super::reference::persist_sql_knowledge_usage_events(
                &self.state.db,
                &claims.tenant_id,
                &claims.sub,
                workspace
                    .sources
                    .first()
                    .map(|s| s.schema.datasource_id.as_str()),
                "federated_trino_reference_use",
                Some(question),
                None,
                &merged,
            )
            .await;
        }
        merged
    }

    async fn explain_and_repair_federated_sql(
        &self,
        claims: &Claims,
        question: &str,
        schema: &serde_json::Value,
        datasource_id: &str,
        config_json: &serde_json::Value,
        sql: &mut String,
        semantic_guard: &AgentSemanticGuard,
    ) -> Result<(), String> {
        if !claim_trino_explain_probe(datasource_id).await {
            return Err(format!(
                "{EXPLAIN_PREFLIGHT_SKIPPED_PREFIX} recent operational EXPLAIN failure for datasource {datasource_id} is still in cooldown"
            ));
        }
        let max_attempts = federated_trino_explain_repair_attempts();
        let mut context = SelfCorrectContext::default();
        for attempt in 0..=max_attempts {
            match self.explain_trino_sql(claims, config_json, sql).await {
                Ok(()) => {
                    clear_trino_explain_suppression(datasource_id).await;
                    return Ok(());
                }
                Err(e) if attempt < max_attempts => {
                    let error_kind = SqlExecErrorKind::new(&e);
                    if error_kind.allows_model_recovery_strategy() {
                        return Err(format!(
                            "federated Trino EXPLAIN encountered unavailable data; deferring model-guided recovery to the audited execution stage: {e}"
                        ));
                    }
                    if !error_kind.is_retryable() {
                        suppress_trino_explain(datasource_id).await;
                        return Err(format!(
                            "federated Trino EXPLAIN preflight was unavailable; SQL repair skipped because the failure was not caused by repairable SQL: {e}"
                        ));
                    }
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_attempts,
                        error = %e,
                        "federated Trino EXPLAIN failed; attempting SQL repair"
                    );
                    let repaired = correct_sql(
                        &self.state,
                        claims,
                        sql,
                        &e,
                        question,
                        schema,
                        &Vec::<ForeignKeyPrompt>::new(),
                        &[],
                        "federated-trino",
                        &mut context,
                        None,
                        "trino",
                        datasource_id,
                        self.preferred_model.as_deref(),
                        self.bounded,
                    )
                    .await;
                    let repaired = strip_trailing_semicolon(&repaired);
                    if repaired.is_empty() || repaired == *sql {
                        return Err(format!(
                            "federated Trino EXPLAIN failed and repair produced no better SQL: {e}"
                        ));
                    }
                    self.verify_semantic_candidate(claims, semantic_guard, &repaired, true)
                        .await
                        .map_err(|verification_error| {
                            format!(
                                "federated Trino EXPLAIN repair changed the canonical analytic intent and was blocked: {verification_error}"
                            )
                        })?;
                    *sql = repaired;
                }
                Err(e) => {
                    if !SqlExecErrorKind::new(&e).is_retryable() {
                        suppress_trino_explain(datasource_id).await;
                    }
                    return Err(format!(
                        "federated Trino EXPLAIN failed after {} repair attempt(s): {e}",
                        max_attempts
                    ));
                }
            }
        }
        Err("federated Trino EXPLAIN failed".to_string())
    }

    async fn execute_federated_sql_with_repair(
        &self,
        claims: &Claims,
        question: &str,
        schema: &serde_json::Value,
        datasource_id: &str,
        config_json: &serde_json::Value,
        sql: &mut String,
        semantic_guard: &AgentSemanticGuard,
    ) -> Result<
        (
            Vec<String>,
            Vec<serde_json::Value>,
            Vec<crate::nl2sql::SqlExecutionAttempt>,
        ),
        (String, Vec<crate::nl2sql::SqlExecutionAttempt>),
    > {
        let max_repair_attempts = max_self_correct_attempts().max(1);
        let max_operational_attempts = max_operational_retry_attempts();
        let mut repair_attempts = 0usize;
        let mut operational_attempts = 0usize;
        let mut context = SelfCorrectContext::default();
        let mut execution_attempts = Vec::new();
        let mut next_retry_reason: Option<String> = None;
        let mut current_repair_decision: Option<SqlRepairDecision> = None;
        loop {
            let attempt_started = std::time::Instant::now();
            match self
                .execute_trino_with_timeout(
                    claims,
                    sql,
                    config_json,
                    self.max_rows_per_step,
                    federated_trino_query_timeout_secs(),
                )
                .await
            {
                Ok((columns, rows)) => {
                    let (repair_strategy, scope_changed, diagnostic_only, repair_rationale) =
                        sql_attempt_decision_fields(current_repair_decision.as_ref());
                    execution_attempts.push(crate::nl2sql::SqlExecutionAttempt {
                        attempt: execution_attempts.len() + 1,
                        status: "succeeded".to_string(),
                        sql: sql.clone(),
                        execution_ms: u64::try_from(attempt_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        error: None,
                        retry_reason: next_retry_reason.take(),
                        repair_strategy,
                        scope_changed,
                        diagnostic_only,
                        repair_rationale,
                    });
                    if operational_attempts > 0 {
                        tracing::info!(
                            datasource_id,
                            operational_attempts,
                            "federated Trino execution succeeded after transient retry"
                        );
                        super::agent_async::emit_agent_stage(
                            "execute_sql",
                            "联邦 Trino 瞬时故障重试成功，SQL 已返回结果",
                        );
                    }
                    return Ok((columns, rows, execution_attempts));
                }
                Err(e) => {
                    let error = e.to_string();
                    let error_kind = SqlExecErrorKind::new(&error);
                    let (repair_strategy, scope_changed, diagnostic_only, repair_rationale) =
                        sql_attempt_decision_fields(current_repair_decision.as_ref());
                    execution_attempts.push(crate::nl2sql::SqlExecutionAttempt {
                        attempt: execution_attempts.len() + 1,
                        status: "failed".to_string(),
                        sql: sql.clone(),
                        execution_ms: u64::try_from(attempt_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        error: Some(error.clone()),
                        retry_reason: next_retry_reason.take(),
                        repair_strategy,
                        scope_changed,
                        diagnostic_only,
                        repair_rationale,
                    });
                    if error_kind.is_transient_operational()
                        && operational_attempts < max_operational_attempts
                    {
                        operational_attempts += 1;
                        let delay_secs = 1u64 << operational_attempts.saturating_sub(1).min(3);
                        tracing::warn!(
                            datasource_id,
                            attempt = operational_attempts,
                            max_attempts = max_operational_attempts,
                            delay_secs,
                            error = %error,
                            "federated Trino transient execution failure; retrying unchanged SQL"
                        );
                        super::agent_async::emit_agent_stage(
                            "retry_sql",
                            &format!(
                                "联邦 Trino 返回瞬时错误，正在保持原 SQL 重试（第 {operational_attempts}/{max_operational_attempts} 次）"
                            ),
                        );
                        if let Some(attempt) = execution_attempts.last_mut() {
                            attempt.retry_reason = Some("transient_retry".to_string());
                        }
                        next_retry_reason = Some("transient_retry".to_string());
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        continue;
                    }
                    if !error_kind.is_retryable() || repair_attempts >= max_repair_attempts {
                        return Err((
                            format!(
                                "federated Trino execution failed after {} SQL repair attempt(s) and {} transient retry attempt(s): {error}",
                                repair_attempts, operational_attempts
                            ),
                            execution_attempts,
                        ));
                    }
                    repair_attempts += 1;
                    tracing::warn!(
                        attempt = repair_attempts,
                        max_attempts = max_repair_attempts,
                        error = %error,
                        "federated Trino execution failed; attempting SQL repair"
                    );
                    let repaired = correct_sql(
                        &self.state,
                        claims,
                        sql,
                        &error,
                        question,
                        schema,
                        &Vec::<ForeignKeyPrompt>::new(),
                        &[],
                        "federated-trino",
                        &mut context,
                        None,
                        "trino",
                        datasource_id,
                        self.preferred_model.as_deref(),
                        self.bounded,
                    )
                    .await;
                    let repair_decision = context.take_last_decision();
                    let repaired = strip_trailing_semicolon(&repaired);
                    if repaired.is_empty() || repaired == *sql {
                        if let (Some(attempt), Some(decision)) =
                            (execution_attempts.last_mut(), repair_decision.as_ref())
                        {
                            annotate_attempt_with_decision(attempt, decision);
                        }
                        return Err((
                            format!(
                                "federated Trino execution failed and repair produced no better SQL: {error}"
                            ),
                            execution_attempts,
                        ));
                    }
                    if let Err(verification_error) = self
                        .verify_semantic_candidate(claims, semantic_guard, &repaired, true)
                        .await
                    {
                        if let (Some(attempt), Some(decision)) =
                            (execution_attempts.last_mut(), repair_decision.as_ref())
                        {
                            annotate_attempt_with_decision(attempt, decision);
                        }
                        return Err((
                            format!(
                                "federated Trino repair changed the canonical analytic intent and was blocked: {verification_error}"
                            ),
                            execution_attempts,
                        ));
                    }
                    *sql = repaired;
                    current_repair_decision = repair_decision;
                    if let Some(decision) = current_repair_decision.as_ref() {
                        super::agent_async::emit_agent_stage_detail(
                            "repair_sql",
                            "模型已选择数据可用性恢复策略，正在执行验证",
                            serde_json::json!({
                                "kind": "model_recovery_decision",
                                "strategy": decision.strategy,
                                "scopeChanged": decision.scope_changed,
                                "diagnosticOnly": decision.diagnostic_only,
                                "rationale": decision.rationale,
                            }),
                        );
                    }
                    if let Some(attempt) = execution_attempts.last_mut() {
                        attempt.retry_reason = Some("sql_repair:model".to_string());
                    }
                    next_retry_reason = Some("sql_repair:model".to_string());
                    if federated_trino_explain_after_execution_repair() {
                        if let Err(explain_err) =
                            self.explain_trino_sql(claims, config_json, sql).await
                        {
                            tracing::warn!(
                                error = %explain_err,
                                "federated Trino repaired SQL failed post-repair EXPLAIN; continuing with execution retry"
                            );
                        }
                    }
                }
            }
        }
    }

    async fn explain_trino_sql(
        &self,
        claims: &Claims,
        config_json: &serde_json::Value,
        sql: &str,
    ) -> Result<(), String> {
        let explain_sql = format!("EXPLAIN {}", strip_trailing_semicolon(sql));
        self.execute_trino_with_timeout(
            claims,
            &explain_sql,
            config_json,
            5,
            federated_trino_explain_timeout_secs(),
        )
        .await
        .map(|_| ())
        .map_err(|e| format!("EXPLAIN failed: {e}"))
    }

    fn federated_error_response(
        &self,
        start: std::time::Instant,
        workspace: &FederatedTrinoWorkspace,
        sql: Option<String>,
        error: String,
        used_references: Vec<ReferenceUsageDto>,
    ) -> AgentExecuteResponse {
        let elapsed = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let datasource_id = workspace
            .sources
            .first()
            .map(|s| s.schema.datasource_id.clone());
        AgentExecuteResponse {
            steps: vec![StepExecutionDetail {
                step_id: 0,
                step_type: "federated_trino_query".to_string(),
                datasource_id,
                description: format!(
                    "Federated Trino SQL over {} datasource(s)",
                    workspace.sources.len()
                ),
                output_name: "federated_result".to_string(),
                sql,
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                execution_ms: elapsed,
                error: Some(error.clone()),
                execution_attempts: Vec::new(),
                diagnostic_only: false,
                recovery_note: None,
            }],
            final_result: FinalAgentResult {
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
            },
            total_execution_ms: elapsed,
            total_steps: 1,
            used_references,
            conversation_id: None,
            query_id: None,
            error: Some(error),
        }
    }

    fn single_datasource_error_response(
        &self,
        start: std::time::Instant,
        schema: &DatasourceSchemaInfo,
        sql: Option<String>,
        error: String,
        used_references: Vec<ReferenceUsageDto>,
    ) -> AgentExecuteResponse {
        let elapsed = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        AgentExecuteResponse {
            steps: vec![StepExecutionDetail {
                step_id: 0,
                step_type: "single_datasource_query".to_string(),
                datasource_id: Some(schema.datasource_id.clone()),
                description: format!("Single datasource SQL on {}", schema.datasource_name),
                output_name: "result".to_string(),
                sql,
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                execution_ms: elapsed,
                error: Some(error.clone()),
                execution_attempts: Vec::new(),
                diagnostic_only: false,
                recovery_note: None,
            }],
            final_result: FinalAgentResult {
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
            },
            total_execution_ms: elapsed,
            total_steps: 1,
            used_references,
            conversation_id: None,
            query_id: None,
            error: Some(error),
        }
    }

    async fn execute_single_datasource_query(
        &self,
        claims: &Claims,
        question: &str,
        retrieval_question: &str,
        schema: &DatasourceSchemaInfo,
        route_snippets: &[ReferencePromptSnippet],
        conversation_id: &str,
        query_id: &str,
    ) -> anyhow::Result<AgentExecuteResponse> {
        let start = std::time::Instant::now();
        super::agent_async::emit_agent_stage(
            "load_context",
            "正在按数据探索链路检索 SQL 知识库和 Schema",
        );

        let mut schema_tables = super::enrich_schema_with_semantics(
            &self.state.db,
            &schema.datasource_id,
            schema.tables.clone(),
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                datasource_id = %schema.datasource_id,
                error = %e,
                "single datasource agent failed to enrich schema semantics; using raw schema"
            );
            schema.tables.clone()
        });
        schema_tables = match crate::nl2sql::routing::resolve_business_domains(
            &self.state.db,
            Some(&schema.datasource_id),
        )
        .await
        {
            Ok(domains) => {
                let strict_match = super::strict_domain_tables_for_question(
                    &domains,
                    &schema.datasource_id,
                    question,
                );
                if strict_match.allowed_tables.is_empty() {
                    schema_tables
                } else {
                    super::filter_schema_tables_by_allowlist(
                        &schema_tables,
                        &strict_match.allowed_tables,
                    )
                }
            }
            Err(e) => {
                tracing::warn!(
                    datasource_id = %schema.datasource_id,
                    error = %e,
                    "single datasource agent failed to resolve strict business domains"
                );
                schema_tables
            }
        };

        let mut reference_snippets = route_snippets.to_vec();
        match super::reference::resolve_auto_query_references(
            &self.state,
            &claims.tenant_id,
            &schema.datasource_id,
            retrieval_question,
            super::sql_knowledge_prompt_max_snippets().min(12),
        )
        .await
        {
            Ok(auto_refs) => {
                reference_snippets = super::merge_reference_snippets(
                    &reference_snippets,
                    auto_refs,
                    super::sql_knowledge_prompt_max_snippets(),
                );
            }
            Err(e) => tracing::warn!(
                error = %e,
                datasource_id = %schema.datasource_id,
                "single datasource agent failed to retrieve SQL knowledge references"
            ),
        }

        if !super::should_enable_sql_generation_tool_loop() {
            if let Ok(mut chat_candidates) = crate::nl2sql::resolve_chat_config_candidates(
                self.state.config_registry(),
                &claims.tenant_id,
                &claims.sub,
                &self.state.default_model,
                Some("nl2sql"),
            )
            .await
            {
                crate::nl2sql::prioritize_chat_candidates(
                    &mut chat_candidates,
                    self.preferred_model.as_deref(),
                );
                if let Some(chat_cfg) = chat_candidates.first() {
                    let tool_refs = super::run_sql_knowledge_tool_prefetch(
                        &self.state,
                        claims,
                        &schema.datasource_id,
                        question,
                        &schema_tables,
                        chat_cfg,
                    )
                    .await;
                    reference_snippets = super::merge_reference_snippets(
                        &reference_snippets,
                        tool_refs,
                        super::sql_knowledge_prompt_max_snippets(),
                    );
                }
            }
        }

        if reference_snippets.is_empty()
            && schema
                .tables
                .as_array()
                .map(|arr| arr.is_empty())
                .unwrap_or(true)
        {
            match super::reference::resolve_recent_sql_examples_for_datasource(
                &self.state,
                &claims.tenant_id,
                &schema.datasource_id,
                super::sql_knowledge_prompt_max_snippets().min(8),
            )
            .await
            {
                Ok(recent_refs) => {
                    reference_snippets = super::merge_reference_snippets(
                        &reference_snippets,
                        recent_refs,
                        super::sql_knowledge_prompt_max_snippets(),
                    );
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    datasource_id = %schema.datasource_id,
                    "single datasource agent empty-schema SQL knowledge fallback failed"
                ),
            }
        }

        let auto_opened_sql_files = super::auto_open_relevant_sql_knowledge_files(
            &self.state,
            claims,
            &schema.datasource_id,
            retrieval_question,
            &reference_snippets,
            if self.bounded { Some(2) } else { None },
        )
        .await;
        if !auto_opened_sql_files.is_empty() {
            reference_snippets = super::merge_reference_snippets(
                &auto_opened_sql_files,
                reference_snippets,
                super::sql_knowledge_prompt_max_snippets(),
            );
            super::agent_async::emit_agent_stage("load_context", "已打开命中的完整 SQL 文件上下文");
        }

        if self.bounded {
            reference_snippets = super::focus_bounded_sql_knowledge_references(
                retrieval_question,
                &reference_snippets,
            );
        }

        let hydrated = discover_knowledge_schema_tables(
            &self.state,
            claims,
            &schema.datasource_id,
            &schema.db_type,
            &schema.config,
            &schema_tables,
            &reference_snippets,
            self.protected_request.then(|| self.network_budget.clone()),
        )
        .await;
        if hydrated.as_array().is_some_and(|tables| !tables.is_empty()) {
            super::agent_async::emit_agent_stage(
                "load_schema",
                "已按 SQL 知识库命中的表按需确认 Schema",
            );
            schema_tables = super::merge_schema_tables(&schema_tables, &hydrated);
            if let Ok(domains) = crate::nl2sql::routing::resolve_business_domains(
                &self.state.db,
                Some(&schema.datasource_id),
            )
            .await
            {
                let strict_match = super::strict_domain_tables_for_question(
                    &domains,
                    &schema.datasource_id,
                    question,
                );
                if !strict_match.allowed_tables.is_empty() {
                    schema_tables = super::filter_schema_tables_by_allowlist(
                        &schema_tables,
                        &strict_match.allowed_tables,
                    );
                }
            }
        }

        let mut used_references = reference_snippets
            .iter()
            .map(ReferencePromptSnippet::to_usage_dto)
            .collect::<Vec<_>>();
        if !reference_snippets.is_empty() {
            super::reference::persist_sql_knowledge_usage_events(
                &self.state.db,
                &claims.tenant_id,
                &claims.sub,
                Some(schema.datasource_id.as_str()),
                "agent_single_reference_use",
                Some(question),
                None,
                &reference_snippets,
            )
            .await;
        }

        let mut foreign_keys: Vec<ForeignKeyPrompt> = schema
            .foreign_keys
            .iter()
            .map(|fk| ForeignKeyPrompt {
                source_table: fk.source_table.clone(),
                source_column: fk.source_column.clone(),
                source_type: fk.source_column_type.clone(),
                target_table: fk.target_table.clone(),
                target_column: fk.target_column.clone(),
                target_type: fk.target_column_type.clone(),
            })
            .collect();
        let manual_fks = crate::nl2sql::load_user_defined_fks_for_datasource(
            &self.state.db,
            &claims.tenant_id,
            &schema.datasource_id,
        )
        .await;
        foreign_keys.extend(manual_fks);
        let join_paths =
            super::load_join_paths_for_datasource(&self.state.db, &schema.datasource_id).await;

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
            .bind(&schema.datasource_id)
            .fetch_all(&self.state.db)
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
        let metrics: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT metric_name, expression, filter_conditions FROM nl2sql_metrics \
             WHERE tenant_id = ? AND datasource_id = ? AND status = 'published' AND deleted_at IS NULL",
        )
        .bind(&claims.tenant_id)
        .bind(&schema.datasource_id)
        .fetch_all(&self.state.db)
        .await
        .unwrap_or_default();
        let metrics_refs = metrics
            .iter()
            .map(|(name, expr, filter)| (name.clone(), expr.clone(), filter.as_deref()))
            .collect::<Vec<_>>();
        let matched_metrics = matched_metric_names(question, &metric_candidates);

        let qu_result: Option<crate::nl2sql::query_understanding::QueryUnderstandingResult> =
            if should_enable_qu() && !self.bounded {
                let chat_cfg = match crate::nl2sql::resolve_chat_config_candidates(
                    self.state.config_registry(),
                    &claims.tenant_id,
                    &claims.sub,
                    &self.state.default_model,
                    Some("nl2sql"),
                )
                .await
                {
                    Ok(mut candidates) => {
                        crate::nl2sql::prioritize_chat_candidates(
                            &mut candidates,
                            self.preferred_model.as_deref(),
                        );
                        candidates.into_iter().next()
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            datasource_id = %schema.datasource_id,
                            "single datasource agent failed to resolve chat config for QU"
                        );
                        None
                    }
                };
                if let Some(cfg) = chat_cfg {
                    let qu = crate::nl2sql::query_understanding::QueryUnderstanding::new(
                        self.state.db.clone(),
                        cfg,
                    );
                    let schema_for_qu = serde_json::json!(schema_tables.clone());
                    match qu
                        .understand(
                            question,
                            &schema.datasource_id,
                            &claims.tenant_id,
                            &schema_for_qu,
                        )
                        .await
                    {
                        Ok(result) => Some(result),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                datasource_id = %schema.datasource_id,
                                "single datasource agent QU understand failed"
                            );
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

        super::agent_async::emit_agent_stage("generate_sql", "正在生成单数据源 SQL");
        let history = super::ConversationHistory {
            messages: Vec::new(),
            summary: Some(format!(
                "Single datasource agent query on '{}' ({}). Use SQL Knowledge references when they match the question.",
                schema.datasource_name, schema.db_type
            )),
        };
        let evidence_columns = reference_evidence_columns(&reference_snippets);
        let semantic_guard = self
            .prepare_semantic_guard(
                claims,
                conversation_id,
                query_id,
                &schema.datasource_id,
                question,
                &schema_tables,
                &matched_metrics,
                &evidence_columns,
            )
            .await?;
        let semantic_intent_json = semantic_guard.intent_json()?;
        let large_schema_mode = schema_tables
            .as_array()
            .map(|arr| arr.len() > 20)
            .unwrap_or(false);
        let sql_result = match generate_sql(
            &self.state,
            claims,
            Some(&schema.datasource_id),
            question,
            &schema_tables,
            &foreign_keys,
            &join_paths,
            history,
            None,
            qu_result.as_ref(),
            &schema.db_type,
            large_schema_mode,
            &metrics_refs,
            &matched_metrics,
            &reference_snippets,
            None,
            self.preferred_model.as_deref(),
            !self.bounded,
            &semantic_intent_json,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                return Ok(self.single_datasource_error_response(
                    start,
                    schema,
                    None,
                    format!("single datasource SQL generation failed: {e}"),
                    used_references,
                ));
            }
        };

        if !sql_result.tool_reference_snippets.is_empty() {
            used_references.extend(
                sql_result
                    .tool_reference_snippets
                    .iter()
                    .map(ReferencePromptSnippet::to_usage_dto),
            );
        }
        if let Some(question) = sql_result.clarification_question {
            return Ok(self.single_datasource_error_response(
                start,
                schema,
                None,
                format!("需要澄清：{question}"),
                used_references,
            ));
        }

        let mut current_sql = strip_trailing_semicolon(&sql_result.sql);
        if current_sql.is_empty() {
            return Ok(self.single_datasource_error_response(
                start,
                schema,
                None,
                "single datasource SQL generation returned empty SQL".to_string(),
                used_references,
            ));
        }

        super::agent_async::emit_agent_stage_detail(
            "generated_sql",
            "SQL 已生成",
            serde_json::json!({
                "kind": "sql",
                "sql": current_sql.clone(),
                "status": "generated",
            }),
        );

        if !self.bounded && matches!(schema.db_type.as_str(), "presto" | "trino") {
            super::agent_async::emit_agent_stage("explain_sql", "正在 EXPLAIN 校验 SQL");
            if let Err(e) = self
                .explain_and_repair_federated_sql(
                    claims,
                    question,
                    &schema_tables,
                    &schema.datasource_id,
                    &schema.config,
                    &mut current_sql,
                    &semantic_guard,
                )
                .await
            {
                if federated_trino_explain_soft_fail() {
                    if trino_explain_preflight_was_skipped(&e) {
                        tracing::info!(
                            datasource_id = %schema.datasource_id,
                            "Trino/Presto EXPLAIN preflight skipped during datasource cooldown; continuing to execution-stage validation"
                        );
                    } else {
                        tracing::warn!(
                            datasource_id = %schema.datasource_id,
                            error = %e,
                            "Trino/Presto EXPLAIN preflight did not pass; continuing to execution-stage validation"
                        );
                    }
                    super::agent_async::emit_agent_stage(
                        "explain_sql",
                        if trino_explain_preflight_was_skipped(&e) {
                            "已跳过近期不可用的 EXPLAIN，直接进入执行阶段验证"
                        } else {
                            "EXPLAIN 未通过，继续进入执行阶段验证和修复"
                        },
                    );
                } else {
                    return Ok(self.single_datasource_error_response(
                        start,
                        schema,
                        Some(current_sql),
                        e,
                        used_references,
                    ));
                }
            }
        }

        super::agent_async::emit_agent_stage("execute_sql", "正在执行 SQL");
        let enriched_schema = DatasourceSchemaInfo {
            tables: schema_tables,
            ..schema.clone()
        };
        let step = self
            .execute_query_step(
                claims,
                0,
                &schema.datasource_id,
                &current_sql,
                "result",
                Some(self.max_rows_per_step),
                std::slice::from_ref(&enriched_schema),
                question,
                &join_paths,
                &semantic_guard,
            )
            .await?;
        let response_rows_cap = max_agent_response_rows().max(50);
        let final_result = FinalAgentResult {
            columns: step.columns.clone(),
            rows: step.rows.iter().take(response_rows_cap).cloned().collect(),
            row_count: step.row_count,
        };
        let error = step
            .error
            .clone()
            .map(|e| format!("agent execution failed: {e}"));
        Ok(AgentExecuteResponse {
            steps: vec![StepExecutionDetail {
                step_id: step.step_id,
                step_type: if step.error.is_some() {
                    "error".to_string()
                } else {
                    "single_datasource_query".to_string()
                },
                datasource_id: step.datasource_id.clone(),
                description: step.output_name.clone(),
                output_name: step.output_name.clone(),
                sql: step.sql.clone(),
                columns: step.columns.clone(),
                rows: step.rows.iter().take(response_rows_cap).cloned().collect(),
                row_count: step.row_count,
                execution_ms: step.execution_ms,
                error: step.error.clone(),
                execution_attempts: step.execution_attempts.clone(),
                diagnostic_only: step.diagnostic_only,
                recovery_note: step.recovery_note.clone(),
            }],
            final_result,
            total_execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            total_steps: 1,
            used_references,
            conversation_id: None,
            query_id: None,
            error,
        })
    }

    /// Main entry point: load schemas, generate plan, execute it.
    pub async fn execute(
        &self,
        claims: &Claims,
        question: &str,
        retrieval_question: Option<&str>,
        shared_context: Option<&str>,
        max_steps_override: Option<usize>,
        allowed_datasource_ids: &[String],
        conversation_id: &str,
        query_id: &str,
    ) -> anyhow::Result<AgentExecuteResponse> {
        let start = std::time::Instant::now();
        super::agent_async::emit_agent_stage("request_validation", "开始校验请求");
        // B-12: Clamp max_steps to hard limit (10) to prevent unbounded loops from malicious clients.
        let max_steps_hard_limit: usize = 10;
        let effective_max_steps = max_steps_override
            .unwrap_or(self.max_steps)
            .min(max_steps_hard_limit)
            .max(1);

        // 1. Load all accessible datasources' schemas for the tenant, optionally filtered.
        super::agent_async::emit_agent_stage("load_schema", "开始加载多数据源 Schema");
        let schemas = self
            .load_accessible_schemas(&claims.tenant_id, &claims.sub, &claims.role)
            .await?;

        let mut schemas = if allowed_datasource_ids.is_empty() {
            schemas
        } else {
            schemas
                .into_iter()
                .filter(|s| allowed_datasource_ids.contains(&s.datasource_id))
                .collect()
        };

        if schemas.is_empty() {
            return Err(anyhow::anyhow!(
                "No accessible datasources found for this tenant"
            ));
        }

        let model_question = contextual_agent_question(question, shared_context);
        let retrieval_question = retrieval_question
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(question);
        let mut routing_question = retrieval_question;
        let mut sql_knowledge_route = if allowed_datasource_ids.is_empty() {
            self.resolve_sql_knowledge_route_candidate(claims, retrieval_question, &schemas)
                .await
        } else {
            None
        };
        // A terse follow-up may have no table/metric signal of its own. Only in
        // that case may prior context participate in datasource routing; an
        // explicit current question always wins over old tables and metrics.
        if sql_knowledge_route.is_none()
            && allowed_datasource_ids.is_empty()
            && model_question != question
            && !question_has_schema_signal(question, &schemas)
        {
            routing_question = &model_question;
            sql_knowledge_route = self
                .resolve_sql_knowledge_route_candidate(claims, routing_question, &schemas)
                .await;
        }
        if let Some(route) = sql_knowledge_route.as_ref() {
            let selected_ids =
                self.schema_ids_for_sql_knowledge_route(route, &schemas, routing_question);
            let filtered = schemas
                .iter()
                .filter(|schema| selected_ids.contains(&schema.datasource_id))
                .cloned()
                .collect::<Vec<_>>();
            if !filtered.is_empty() {
                super::agent_async::emit_agent_stage(
                    "route_selected",
                    &format!("已根据 SQL 知识库选择数据源：{}", route.datasource_name),
                );
                schemas = filtered;
            }
        }

        if let Some(resp) = self
            .try_execute_federated_trino(
                claims,
                routing_question,
                &schemas,
                !allowed_datasource_ids.is_empty() || sql_knowledge_route.is_some(),
                conversation_id,
                query_id,
            )
            .await?
        {
            return Ok(resp);
        }

        if schemas.len() == 1 {
            let route_snippets = sql_knowledge_route
                .as_ref()
                .filter(|route| route.datasource_id == schemas[0].datasource_id)
                .map(|route| route.snippets.as_slice())
                .unwrap_or(&[]);
            return self
                .execute_single_datasource_query(
                    claims,
                    &model_question,
                    retrieval_question,
                    &schemas[0],
                    route_snippets,
                    conversation_id,
                    query_id,
                )
                .await;
        }

        let planning_references = sql_knowledge_route
            .as_ref()
            .map(|route| route.snippets.clone())
            .unwrap_or_default();
        // 2. Generate the multi-step execution plan.
        super::agent_async::emit_agent_stage(
            "query_understanding",
            "开始理解问题并规划多数据源步骤",
        );
        let combined_schema = serde_json::Value::Array(
            schemas
                .iter()
                .flat_map(|schema| schema.tables.as_array().into_iter().flatten().cloned())
                .collect(),
        );
        let datasource_scope = schemas
            .iter()
            .map(|schema| schema.datasource_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let planning_metrics = self
            .matched_metrics_for_schemas(&claims.tenant_id, &model_question, &schemas)
            .await;
        let planning_evidence_columns = reference_evidence_columns(&planning_references);
        let planning_guard = self
            .prepare_semantic_guard(
                claims,
                conversation_id,
                query_id,
                &datasource_scope,
                &model_question,
                &combined_schema,
                &planning_metrics,
                &planning_evidence_columns,
            )
            .await?;
        let planning_intent_json = planning_guard.intent_json()?;
        let plan = self
            .generate_multi_step_plan(
                claims,
                &model_question,
                &schemas,
                &planning_references,
                &planning_intent_json,
            )
            .await?;

        // 3. Execute the plan.
        super::agent_async::emit_agent_stage(
            "generate_sql",
            &format!("开始执行 {} 个多数据源步骤", plan.steps.len()),
        );
        let step_results = self
            .execute_plan(
                claims,
                &model_question,
                &plan,
                &schemas,
                effective_max_steps,
                conversation_id,
                query_id,
            )
            .await?;

        // 4. Build response.
        super::agent_async::emit_agent_stage("persist_result", "开始汇总多数据源执行结果");
        let response_rows_cap = max_agent_response_rows().max(50);
        let final_result = step_results
            .last()
            .map(|r| FinalAgentResult {
                columns: r.columns.clone(),
                rows: r.rows.iter().take(response_rows_cap).cloned().collect(),
                row_count: r.row_count,
            })
            .unwrap_or(FinalAgentResult {
                columns: vec![],
                rows: vec![],
                row_count: 0,
            });

        let steps: Vec<StepExecutionDetail> = step_results
            .iter()
            .map(|r| StepExecutionDetail {
                step_id: r.step_id,
                step_type: if r.error.is_some() {
                    "error".to_string()
                } else if r.columns.is_empty() && r.rows.is_empty() {
                    "merge".to_string()
                } else {
                    "query".to_string()
                },
                datasource_id: r.datasource_id.clone(),
                description: r.output_name.clone(),
                output_name: r.output_name.clone(),
                sql: r.sql.clone(),
                columns: r.columns.clone(),
                rows: r.rows.iter().take(response_rows_cap).cloned().collect(),
                row_count: r.row_count,
                execution_ms: r.execution_ms,
                error: r.error.clone(),
                execution_attempts: r.execution_attempts.clone(),
                diagnostic_only: r.diagnostic_only,
                recovery_note: r.recovery_note.clone(),
            })
            .collect();
        let error = step_results
            .iter()
            .find_map(|r| r.error.clone())
            .map(|e| format!("agent execution failed: {e}"));

        Ok(AgentExecuteResponse {
            steps,
            final_result,
            total_execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            total_steps: step_results.len(),
            used_references: planning_references
                .iter()
                .map(ReferencePromptSnippet::to_usage_dto)
                .collect(),
            conversation_id: None,
            query_id: None,
            error,
        })
    }

    /// Load schemas for all accessible datasources (executable types only).
    async fn load_accessible_schemas(
        &self,
        tenant_id: &str,
        user_id: &str,
        role: &str,
    ) -> anyhow::Result<Vec<DatasourceSchemaInfo>> {
        let is_admin = role == "admin" || role == "superadmin";

        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<serde_json::Value>,
                serde_json::Value,
                Option<String>,
                String,
            ),
        >(
            "SELECT id, name, db_type, schema_info, config, user_id, visibility \
             FROM data_sources WHERE tenant_id = ? AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .fetch_all(&self.state.db)
        .await?;

        // Load all cross-datasource relations for this tenant in one query.
        let cross_ds_relations: Vec<CrossDatasourceRelation> = {
            let rows: Vec<(String, String, String, String, String, String, String, String)> =
                sqlx::query_as(
                    "SELECT left_datasource_id, left_table, left_column, \
                     right_datasource_id, right_table, right_column, match_type, semantic_description \
                     FROM nl2sql_cross_datasource_relations WHERE tenant_id = ? AND deleted_at IS NULL",
                )
                .bind(tenant_id)
                .fetch_all(&self.state.db)
                .await
                .unwrap_or_default();
            rows.into_iter()
                .map(
                    |(ldid, lt, lc, rdid, rt, rc, mt, sd)| CrossDatasourceRelation {
                        left_datasource_id: ldid,
                        left_table: lt,
                        left_column: lc,
                        right_datasource_id: rdid,
                        right_table: rt,
                        right_column: rc,
                        match_type: mt,
                        semantic_description: sd,
                    },
                )
                .collect()
        };

        let mut schemas = Vec::new();
        for (ds_id, ds_name, db_type, schema_info, config, owner_user_id, visibility) in rows {
            // Filter to executable db_types only
            if !matches!(
                db_type.as_str(),
                "mysql" | "tidb" | "postgres" | "clickhouse" | "presto" | "trino" | "mongodb"
            ) {
                continue;
            }

            // Members can use their own private sources and tenant-shared
            // sources. Admins retain access to all tenant sources.
            if !datasource_visible_to_user(is_admin, user_id, owner_user_id.as_deref(), &visibility)
            {
                continue;
            }

            let schema_info = schema_info.unwrap_or_else(super::empty_schema_info);
            let parsed = extract_schema_tables_and_fks(&schema_info);
            schemas.push(DatasourceSchemaInfo {
                datasource_id: ds_id.clone(),
                datasource_name: ds_name,
                db_type,
                config,
                tables: parsed.0,
                foreign_keys: parsed.1,
                // Attach cross-datasource relations relevant to this datasource.
                cross_datasource_relations: cross_ds_relations
                    .iter()
                    .filter(|r| r.left_datasource_id == ds_id || r.right_datasource_id == ds_id)
                    .cloned()
                    .collect(),
            });
        }

        Ok(schemas)
    }

    /// Generate a multi-step execution plan using the LLM.
    async fn generate_multi_step_plan(
        &self,
        claims: &Claims,
        question: &str,
        schemas: &[DatasourceSchemaInfo],
        reference_snippets: &[ReferencePromptSnippet],
        semantic_intent_json: &str,
    ) -> anyhow::Result<crate::nl2sql::MultiStepPlan> {
        let mut chat_candidates = crate::nl2sql::resolve_chat_config_candidates(
            self.state.config_registry(),
            &claims.tenant_id,
            &claims.sub,
            &self.state.default_model,
            Some("nl2sql"),
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to create LLM client: {}", e))?;
        crate::nl2sql::prioritize_chat_candidates(
            &mut chat_candidates,
            self.preferred_model.as_deref(),
        );
        let chat_cfg = chat_candidates
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("failed to create LLM client: no candidates"))?;

        let model = chat_cfg.model.clone();
        let client = chat_cfg.client;

        let _schemas_json = serde_json::to_string_pretty(schemas)?;
        // P2-4: Load cross-domain clusters for the agent planning context
        let clusters_summary =
            load_cross_domain_clusters_summary(&self.state.db, &claims.tenant_id).await;
        let knowledge_context = format_agent_sql_knowledge_context(reference_snippets);
        let mut prompt =
            build_agent_planning_prompt(question, schemas, &clusters_summary, &knowledge_context);
        super::append_canonical_semantic_intent(&mut prompt, semantic_intent_json);

        let request = api::MessageRequest {
            model: model.clone(),
            max_tokens: 2048,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text { text: prompt }],
            }],
            system: None,
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
            .map_err(|e| anyhow::anyhow!("LLM planning call failed: {}", e))?;

        let text_content = response
            .content
            .iter()
            .find_map(|block| match block {
                api::OutputContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        parse_multi_step_plan(&text_content)
    }

    /// Execute the plan step by step.
    async fn execute_plan(
        &self,
        claims: &Claims,
        question: &str,
        plan: &crate::nl2sql::MultiStepPlan,
        schemas: &[DatasourceSchemaInfo],
        max_steps: usize,
        conversation_id: &str,
        query_id: &str,
    ) -> anyhow::Result<Vec<crate::nl2sql::StepResult>> {
        let mut results: Vec<crate::nl2sql::StepResult> = Vec::new();
        let mut intermediate: std::collections::HashMap<String, crate::nl2sql::StepResult> =
            std::collections::HashMap::new();

        for (i, step) in plan.steps.iter().enumerate() {
            if i >= max_steps {
                break;
            }
            super::agent_async::emit_agent_stage(
                "generate_sql",
                &format!(
                    "执行多数据源步骤 {}/{}",
                    i + 1,
                    max_steps.min(plan.steps.len())
                ),
            );

            let result = match step {
                crate::nl2sql::ExecutionStep::Query {
                    step_id,
                    datasource_id,
                    sql,
                    description,
                    output_name,
                    max_rows,
                } => {
                    let schema = schemas
                        .iter()
                        .find(|schema| schema.datasource_id == datasource_id.as_str())
                        .map(|schema| schema.tables.clone())
                        .unwrap_or_else(super::empty_schema_info);
                    let step_intent_id = format!("{query_id}:step:{step_id}");
                    let step_question = format!("{question}\nStep requirement: {description}");
                    let semantic_guard = self
                        .prepare_semantic_guard(
                            claims,
                            conversation_id,
                            &step_intent_id,
                            datasource_id,
                            &step_question,
                            &schema,
                            &[],
                            &[],
                        )
                        .await?;
                    self.execute_query_step(
                        claims,
                        *step_id,
                        datasource_id,
                        sql,
                        output_name,
                        *max_rows,
                        schemas,
                        question,
                        &[],
                        &semantic_guard,
                    )
                    .await?
                }
                crate::nl2sql::ExecutionStep::Merge {
                    step_id,
                    strategy,
                    inputs,
                    output_name,
                    description: _,
                } => {
                    // P2-4: Validate MERGE step against known cross-datasource relations.
                    if let Some(err_msg) = self.validate_merge_step(strategy, schemas) {
                        tracing::warn!(
                            "MERGE step {} uses columns without known cross-datasource relationship: {}",
                            step_id, err_msg
                        );
                    }
                    self.execute_merge_step(*step_id, strategy, inputs, output_name, &intermediate)?
                }
            };

            intermediate.insert(result.output_name.clone(), result.clone());
            results.push(result);
        }

        Ok(results)
    }

    /// Execute a single Query step: run SQL on the target datasource.
    /// On schema errors (TableNotFound, ColumnNotFound), retries up to max_self_correct_attempts().
    async fn execute_query_step(
        &self,
        claims: &Claims,
        step_id: usize,
        datasource_id: &str,
        sql: &str,
        output_name: &str,
        max_rows: Option<usize>,
        schemas: &[DatasourceSchemaInfo],
        question_context: &str,
        join_paths: &[(String, String)],
        semantic_guard: &AgentSemanticGuard,
    ) -> anyhow::Result<crate::nl2sql::StepResult> {
        let start = std::time::Instant::now();

        // Validate access.
        let db_type = match validate_data_source_access(
            &self.state,
            &claims.tenant_id,
            &claims.sub,
            &claims.role,
            datasource_id,
        )
        .await
        {
            Ok(t) => t,
            Err(e) => {
                return Ok(crate::nl2sql::StepResult {
                    step_id,
                    output_name: output_name.to_owned(),
                    sql: Some(sql.to_owned()),
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                    execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    error: Some(format!("access denied: {}", e)),
                    datasource_id: Some(datasource_id.to_owned()),
                    execution_attempts: Vec::new(),
                    diagnostic_only: false,
                    recovery_note: None,
                });
            }
        };

        // Load config and schema for the target datasource.
        let row = sqlx::query("SELECT config, schema_info FROM data_sources WHERE id = ?")
            .bind(datasource_id)
            .fetch_optional(&self.state.db)
            .await?;

        let (config_json, schema_json): (serde_json::Value, serde_json::Value) = match row {
            Some(r) => (
                r.get("config"),
                r.get::<Option<serde_json::Value>, _>("schema_info")
                    .unwrap_or_else(super::empty_schema_info),
            ),
            None => {
                return Ok(crate::nl2sql::StepResult {
                    step_id,
                    output_name: output_name.to_owned(),
                    sql: Some(sql.to_owned()),
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                    execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    error: Some("datasource not found".to_owned()),
                    datasource_id: Some(datasource_id.to_owned()),
                    execution_attempts: Vec::new(),
                    diagnostic_only: false,
                    recovery_note: None,
                });
            }
        };

        let parsed = extract_schema_tables_and_fks(&schema_json);
        let schema_context = schemas.iter().find(|s| s.datasource_id == datasource_id);
        let schema_tables = schema_context
            .map(|s| s.tables.clone())
            .unwrap_or_else(|| parsed.0.clone());

        // Load auto-detected FKs from schema.
        let auto_fk_raw = schema_context
            .map(|s| s.foreign_keys.clone())
            .unwrap_or(parsed.1);
        let auto_fks: Vec<crate::nl2sql::ForeignKeyPrompt> = auto_fk_raw
            .into_iter()
            .map(|fk| crate::nl2sql::ForeignKeyPrompt {
                source_table: fk.source_table.clone(),
                source_column: fk.source_column.clone(),
                source_type: fk.source_column_type.clone(),
                target_table: fk.target_table.clone(),
                target_column: fk.target_column.clone(),
                target_type: fk.target_column_type.clone(),
            })
            .collect();
        let mut foreign_keys = auto_fks;

        // Append user-defined FKs (they take precedence).
        let manual_fks = crate::nl2sql::load_user_defined_fks_for_datasource(
            &self.state.db,
            &claims.tenant_id,
            datasource_id,
        )
        .await;
        foreign_keys.extend(manual_fks);

        let effective_max_rows = max_rows.unwrap_or(self.max_rows_per_step);
        let mut current_sql = sql.to_string();
        let mut correct_context = SelfCorrectContext::default();
        let mut attempts = 0;
        let mut operational_attempts = 0;
        let max_attempts = max_self_correct_attempts();
        let max_operational_attempts = max_operational_retry_attempts();
        let mut last_repair_method: Option<&'static str> = None;
        let mut execution_attempts = Vec::new();
        let mut next_retry_reason: Option<String> = None;
        let mut current_repair_decision: Option<SqlRepairDecision> = None;

        if let Err(error) = self
            .verify_semantic_candidate(claims, semantic_guard, &current_sql, false)
            .await
        {
            return Ok(crate::nl2sql::StepResult {
                step_id,
                output_name: output_name.to_owned(),
                sql: Some(current_sql),
                columns: vec![],
                rows: vec![],
                row_count: 0,
                execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                error: Some(format!("SQL semantic verification failed: {error}")),
                datasource_id: Some(datasource_id.to_owned()),
                execution_attempts,
                diagnostic_only: false,
                recovery_note: None,
            });
        }

        loop {
            let attempt_started = std::time::Instant::now();
            let attempt_number = execution_attempts.len() + 1;
            super::agent_async::emit_agent_stage_detail(
                "execute_sql",
                &format!("正在执行步骤 {step_id} 的第 {attempt_number} 次 SQL 尝试"),
                serde_json::json!({
                    "kind": "sql_attempt",
                    "stepId": step_id,
                    "attempt": attempt_number,
                    "datasourceId": datasource_id,
                    "sql": current_sql.clone(),
                    "status": "running",
                }),
            );
            let result =
                if let Some(preflight_error) = dialect_preflight_error(&db_type, &current_sql) {
                    Err(anyhow::anyhow!(preflight_error))
                } else {
                    self.execute_sql_on_datasource(
                        claims,
                        &db_type,
                        &config_json,
                        &current_sql,
                        effective_max_rows,
                        datasource_id,
                    )
                    .await
                };

            match result {
                Ok((columns, rows)) => {
                    let (repair_strategy, scope_changed, diagnostic_only, repair_rationale) =
                        sql_attempt_decision_fields(current_repair_decision.as_ref());
                    execution_attempts.push(crate::nl2sql::SqlExecutionAttempt {
                        attempt: execution_attempts.len() + 1,
                        status: "succeeded".to_string(),
                        sql: current_sql.clone(),
                        execution_ms: u64::try_from(attempt_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        error: None,
                        retry_reason: next_retry_reason.take(),
                        repair_strategy,
                        scope_changed,
                        diagnostic_only,
                        repair_rationale,
                    });
                    if attempts > 0 || operational_attempts > 0 {
                        tracing::info!(
                            step_id,
                            datasource_id = %datasource_id,
                            repair_attempts = attempts,
                            operational_attempts,
                            repair_method = last_repair_method.unwrap_or("unknown"),
                            row_count = rows.len(),
                            execution_ms = u64::try_from(start.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                            "execute_query_step: SQL execution succeeded after retry"
                        );
                        super::agent_async::emit_agent_stage(
                            "execute_sql",
                            &if attempts > 0 {
                                format!(
                                    "步骤 {step_id} 的修正版 SQL 已执行成功，返回 {} 行",
                                    rows.len()
                                )
                            } else {
                                format!(
                                    "步骤 {step_id} 的 SQL 瞬时故障重试成功，返回 {} 行",
                                    rows.len()
                                )
                            },
                        );
                    }
                    super::agent_async::emit_agent_stage_detail(
                        "execute_sql",
                        &format!("步骤 {step_id} 执行完成，返回 {} 行", rows.len()),
                        serde_json::json!({
                            "kind": "query_result",
                            "stepId": step_id,
                            "attempt": attempt_number,
                            "datasourceId": datasource_id,
                            "sql": current_sql.clone(),
                            "status": "completed",
                            "rowCount": rows.len(),
                            "columns": columns.iter().take(40).cloned().collect::<Vec<_>>(),
                            "rowsPreview": rows.iter().take(12).cloned().collect::<Vec<_>>(),
                            "elapsedMs": u64::try_from(attempt_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                            "diagnosticOnly": diagnostic_only,
                            "recoveryNote": current_repair_decision
                                .as_ref()
                                .and_then(repair_decision_note),
                        }),
                    );
                    let recovery_note = current_repair_decision
                        .as_ref()
                        .and_then(repair_decision_note);
                    return Ok(crate::nl2sql::StepResult {
                        step_id,
                        output_name: output_name.to_owned(),
                        sql: Some(current_sql.clone()),
                        columns,
                        rows: rows.clone(),
                        row_count: rows.len(),
                        execution_ms: u64::try_from(start.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        error: None,
                        datasource_id: Some(datasource_id.to_owned()),
                        execution_attempts,
                        diagnostic_only,
                        recovery_note,
                    });
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    let err_kind = SqlExecErrorKind::new(&err_msg);
                    super::agent_async::emit_agent_stage_detail(
                        "execute_sql_failed",
                        &format!("步骤 {step_id} 的第 {attempt_number} 次 SQL 尝试未通过"),
                        serde_json::json!({
                            "kind": "sql_attempt",
                            "stepId": step_id,
                            "attempt": attempt_number,
                            "datasourceId": datasource_id,
                            "sql": current_sql.clone(),
                            "status": "failed",
                            "error": err_msg.clone(),
                            "elapsedMs": u64::try_from(attempt_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                        }),
                    );
                    let (repair_strategy, scope_changed, diagnostic_only, repair_rationale) =
                        sql_attempt_decision_fields(current_repair_decision.as_ref());
                    execution_attempts.push(crate::nl2sql::SqlExecutionAttempt {
                        attempt: execution_attempts.len() + 1,
                        status: "failed".to_string(),
                        sql: current_sql.clone(),
                        execution_ms: u64::try_from(attempt_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        error: Some(err_msg.clone()),
                        retry_reason: next_retry_reason.take(),
                        repair_strategy,
                        scope_changed,
                        diagnostic_only,
                        repair_rationale,
                    });
                    if err_kind.is_transient_operational()
                        && operational_attempts < max_operational_attempts
                    {
                        operational_attempts += 1;
                        let delay_secs = 1u64 << operational_attempts.saturating_sub(1).min(3);
                        tracing::warn!(
                            step_id,
                            datasource_id = %datasource_id,
                            attempt = operational_attempts,
                            max_attempts = max_operational_attempts,
                            delay_secs,
                            error = %err_msg,
                            "execute_query_step: transient datasource failure; retrying unchanged SQL"
                        );
                        super::agent_async::emit_agent_stage(
                            "retry_sql",
                            &format!(
                                "步骤 {step_id} 遇到瞬时数据源错误，正在保持原 SQL 重试（第 {operational_attempts}/{max_operational_attempts} 次）"
                            ),
                        );
                        if let Some(attempt) = execution_attempts.last_mut() {
                            attempt.retry_reason = Some("transient_retry".to_string());
                        }
                        next_retry_reason = Some("transient_retry".to_string());
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        continue;
                    }

                    if err_kind.is_retryable() && attempts < max_attempts {
                        attempts += 1;
                        tracing::warn!(
                            step_id,
                            datasource_id = %datasource_id,
                            repair_attempt = attempts,
                            max_repair_attempts = max_attempts,
                            error_kind = err_kind.label(),
                            sql_chars = current_sql.chars().count(),
                            error = %err_msg,
                            "execute_query_step: SQL execution failed; starting automatic repair"
                        );
                        super::agent_async::emit_agent_stage(
                            "repair_sql",
                            &format!(
                                "步骤 {step_id} 的 SQL 执行未通过，正在自动修复（第 {attempts}/{max_attempts} 次）"
                            ),
                        );

                        // Build question context for the correction prompt.
                        let schema = schemas.iter().find(|s| s.datasource_id == datasource_id);
                        let question = schema
                            .map(|s| {
                                format!(
                                    "User question: {}\nTarget datasource: '{}' ({})\nOriginal step SQL:\n{}",
                                    question_context, s.datasource_name, s.db_type, sql
                                )
                            })
                            .unwrap_or_else(|| {
                                format!("User question: {}\nOriginal step SQL:\n{}", question_context, sql)
                            });

                        let (new_sql, repair_method, repair_decision) = if let Some(repaired) =
                            deterministic_dialect_repair(&db_type, &current_sql, &err_msg)
                        {
                            (repaired, "deterministic", None)
                        } else {
                            let repaired = correct_sql(
                                &self.state,
                                claims,
                                &current_sql,
                                &err_msg,
                                &question,
                                &schema_tables,
                                &foreign_keys,
                                join_paths,
                                "",
                                &mut correct_context,
                                None,
                                &db_type,
                                datasource_id,
                                self.preferred_model.as_deref(),
                                self.bounded,
                            )
                            .await;
                            let decision = correct_context.take_last_decision();
                            (repaired, "model", decision)
                        };

                        let new_sql = strip_trailing_semicolon(&new_sql);
                        if !new_sql.is_empty() && new_sql != strip_trailing_semicolon(&current_sql)
                        {
                            if let Err(verification_error) = self
                                .verify_semantic_candidate(claims, semantic_guard, &new_sql, true)
                                .await
                            {
                                if let (Some(attempt), Some(decision)) =
                                    (execution_attempts.last_mut(), repair_decision.as_ref())
                                {
                                    annotate_attempt_with_decision(attempt, decision);
                                }
                                tracing::warn!(
                                    step_id,
                                    datasource_id = %datasource_id,
                                    error = %verification_error,
                                    "execute_query_step: repaired SQL failed canonical semantic verification"
                                );
                                return Ok(crate::nl2sql::StepResult {
                                    step_id,
                                    output_name: output_name.to_owned(),
                                    sql: Some(current_sql),
                                    columns: vec![],
                                    rows: vec![],
                                    row_count: 0,
                                    execution_ms: u64::try_from(start.elapsed().as_millis())
                                        .unwrap_or(u64::MAX),
                                    error: Some(format!(
                                        "repaired SQL changed the canonical analytic intent and was blocked: {verification_error}"
                                    )),
                                    datasource_id: Some(datasource_id.to_owned()),
                                    execution_attempts,
                                    diagnostic_only: false,
                                    recovery_note: None,
                                });
                            }
                            tracing::info!(
                                step_id,
                                datasource_id = %datasource_id,
                                repair_attempt = attempts,
                                repair_method,
                                sql_chars = new_sql.chars().count(),
                                "execute_query_step: SQL repair produced a candidate; retrying execution"
                            );
                            super::agent_async::emit_agent_stage(
                                "repair_sql",
                                &format!("步骤 {step_id} 已生成修正版 SQL，正在重新执行验证"),
                            );
                            current_sql = new_sql;
                            current_repair_decision = repair_decision;
                            if let Some(decision) = current_repair_decision.as_ref() {
                                super::agent_async::emit_agent_stage_detail(
                                    "repair_sql",
                                    &format!(
                                        "步骤 {step_id} 已选择数据可用性恢复策略，正在执行验证"
                                    ),
                                    serde_json::json!({
                                        "kind": "model_recovery_decision",
                                        "stepId": step_id,
                                        "datasourceId": datasource_id,
                                        "strategy": decision.strategy,
                                        "scopeChanged": decision.scope_changed,
                                        "diagnosticOnly": decision.diagnostic_only,
                                        "rationale": decision.rationale,
                                    }),
                                );
                            }
                            last_repair_method = Some(repair_method);
                            if let Some(attempt) = execution_attempts.last_mut() {
                                attempt.retry_reason = Some(format!("sql_repair:{repair_method}"));
                            }
                            next_retry_reason = Some(format!("sql_repair:{repair_method}"));
                            continue;
                        }
                        if let (Some(attempt), Some(decision)) =
                            (execution_attempts.last_mut(), repair_decision.as_ref())
                        {
                            annotate_attempt_with_decision(attempt, decision);
                        }
                        tracing::warn!(
                            step_id,
                            datasource_id = %datasource_id,
                            repair_attempt = attempts,
                            repair_method,
                            error_kind = err_kind.label(),
                            "execute_query_step: automatic repair produced no changed SQL candidate"
                        );
                    }

                    tracing::warn!(
                        step_id,
                        datasource_id = %datasource_id,
                        repair_attempts = attempts,
                        max_repair_attempts = max_attempts,
                        error_kind = err_kind.label(),
                        error = %err_msg,
                        "execute_query_step: SQL execution did not pass validation; marking step as failed"
                    );
                    super::agent_async::emit_agent_stage(
                        "execute_sql_failed",
                        &format!("步骤 {step_id} 的 SQL 未能通过执行验证，该步骤不会作为有效证据"),
                    );

                    return Ok(crate::nl2sql::StepResult {
                        step_id,
                        output_name: output_name.to_owned(),
                        sql: Some(current_sql.clone()),
                        columns: vec![],
                        rows: vec![],
                        row_count: 0,
                        execution_ms: u64::try_from(start.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        error: Some(err_msg),
                        datasource_id: Some(datasource_id.to_owned()),
                        execution_attempts,
                        diagnostic_only: false,
                        recovery_note: None,
                    });
                }
            }
        }
    }

    /// Execute SQL on a datasource, returning (columns, rows).
    async fn execute_sql_on_datasource(
        &self,
        claims: &Claims,
        db_type: &str,
        config_json: &serde_json::Value,
        sql: &str,
        max_rows: usize,
        _datasource_id: &str,
    ) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
        let _network_permit = if matches!(db_type, "presto" | "trino") {
            None
        } else {
            Some(
                self.network_budget
                    .acquire("datasource SQL execution")
                    .await?,
            )
        };
        match db_type {
            "mysql" | "tidb" => self.execute_mysql(sql, config_json, max_rows).await,
            "clickhouse" => self.execute_clickhouse(sql, config_json, max_rows).await,
            "presto" | "trino" => self.execute_trino(claims, sql, config_json, max_rows).await,
            "postgres" => self.execute_postgres(sql, config_json, max_rows).await,
            "mongodb" => {
                let config_val = decrypt_config(config_json, &self.state.data_dir)?;
                let config: nl2sql_domain::datasource_config::MongoConfig =
                    serde_json::from_value(config_val)?;
                let result = super::mongodb_query::execute(
                    &config,
                    sql,
                    max_rows,
                    std::time::Duration::from_secs(30),
                )
                .await
                .map_err(anyhow::Error::msg)?;
                Ok((result.columns, result.rows))
            }
            _ => Err(anyhow::anyhow!("unsupported db_type: {}", db_type)),
        }
    }

    async fn execute_mysql(
        &self,
        sql: &str,
        config_json: &serde_json::Value,
        max_rows: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
        #[derive(Debug, Deserialize)]
        struct SqlConfig {
            host: String,
            port: u16,
            database: String,
            username: String,
            password: String,
        }
        let config_val = decrypt_config(config_json, &self.state.data_dir)?;
        let cfg: SqlConfig = serde_json::from_value(config_val)?;
        let url = crate::routes::data_sources::build_mysql_url_parts(
            &cfg.username,
            &cfg.password,
            &cfg.host,
            cfg.port,
            &cfg.database,
        );
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .connect(&url)
            .await
            .map_err(|e| anyhow::anyhow!("mysql connection failed: {}", e))?;

        let sql_rows = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            sqlx::query(sql).fetch_all(&pool),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Query execution timed out"))?
        .map_err(|e| anyhow::anyhow!("mysql query failed: {}", e))?;

        pool.close().await;

        let columns: Vec<String> = if sql_rows.is_empty() {
            vec![]
        } else {
            sql_rows[0]
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect()
        };

        let rows: Vec<serde_json::Value> = sql_rows
            .into_iter()
            .take(max_rows)
            .map(|row| {
                let mut map = serde_json::Map::new();
                for (i, col) in row.columns().iter().enumerate() {
                    let value = decode_mysql_cell(&row, i);
                    map.insert(col.name().to_string(), value);
                }
                serde_json::Value::Object(map)
            })
            .collect();

        Ok((columns, rows))
    }

    async fn execute_clickhouse(
        &self,
        sql: &str,
        config_json: &serde_json::Value,
        max_rows: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
        #[derive(Debug, Deserialize)]
        struct ClickHouseConfig {
            host: String,
            port: u16,
            database: String,
            username: String,
            password: String,
        }
        let config_val = decrypt_config(config_json, &self.state.data_dir)?;
        let cfg: ClickHouseConfig = serde_json::from_value(config_val)?;

        let addr = format!("http://{}:{}", cfg.host, cfg.port);
        let client = clickhouse::Client::default()
            .with_url(&addr)
            .with_user(&cfg.username)
            .with_password(&cfg.password)
            .with_database(&cfg.database);

        let rows_data: Vec<serde_json::Value> =
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                let cursor = client
                    .query(sql)
                    .fetch_bytes("JSONEachRow")
                    .map_err(|e| anyhow::anyhow!("clickhouse query failed: {}", e))?;

                use tokio::io::AsyncBufReadExt;
                let mut lines = cursor.lines();
                let mut rows: Vec<serde_json::Value> = Vec::new();
                while let Some(line) = lines
                    .next_line()
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                {
                    if line.is_empty() {
                        continue;
                    }
                    let value: serde_json::Value = serde_json::from_str(&line)?;
                    rows.push(value);
                }
                Ok::<_, anyhow::Error>(rows)
            })
            .await
            .map_err(|_| anyhow::anyhow!("Query execution timed out"))?
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let columns: Vec<String> = if rows_data.is_empty() {
            vec![]
        } else if let Some(first) = rows_data[0].as_object() {
            first.keys().cloned().collect()
        } else {
            vec![]
        };

        let rows: Vec<serde_json::Value> = rows_data
            .into_iter()
            .take(max_rows)
            .map(|v| {
                if let serde_json::Value::Object(map) = v {
                    serde_json::Value::Object(map)
                } else {
                    v
                }
            })
            .collect();

        Ok((columns, rows))
    }

    async fn execute_trino(
        &self,
        claims: &Claims,
        sql: &str,
        config_json: &serde_json::Value,
        max_rows: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
        self.execute_trino_with_timeout(
            claims,
            sql,
            config_json,
            max_rows,
            agent_trino_query_timeout_secs(),
        )
        .await
    }

    async fn execute_trino_with_timeout(
        &self,
        claims: &Claims,
        sql: &str,
        config_json: &serde_json::Value,
        max_rows: usize,
        timeout_secs: u64,
    ) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
        #[derive(Debug, Deserialize)]
        struct TrinoConfig {
            host: String,
            port: u16,
            catalog: String,
            #[serde(default)]
            schema: String,
            #[serde(default)]
            schemas: Vec<String>,
            username: String,
            #[serde(default)]
            password: String,
            #[serde(default)]
            ssl: Option<bool>,
            #[serde(default)]
            basic_auth: Option<bool>,
        }
        let config_val = decrypt_config(config_json, &self.state.data_dir)?;
        let cfg: TrinoConfig = serde_json::from_value(config_val)?;

        let normalized_host = nl2sql_domain::datasource_config::normalize_host_input(&cfg.host);
        let port = normalized_host.port.unwrap_or(cfg.port);
        let secure = cfg.ssl.or(normalized_host.secure).unwrap_or(port == 443);
        let effective_schemas = nl2sql_domain::datasource_config::normalize_trino_schemas(
            &cfg.schema,
            cfg.schemas.iter().map(String::as_str),
        );
        let schema = effective_schemas
            .first()
            .map(String::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("default");
        let mut builder =
            trino_rust_client::ClientBuilder::new(&cfg.username, &normalized_host.host)
                .port(port)
                .catalog(&cfg.catalog)
                .schema(schema)
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
            .map_err(|e| anyhow::anyhow!("trino client build failed: {}", e))?;

        let dataset = match execute_trino_query_bounded(
            cli,
            sql.to_string(),
            timeout_secs,
            &claims.tenant_id,
            &claims.sub,
            "Trino SQL execution or EXPLAIN",
            self.network_budget.clone(),
        )
        .await
        {
            Ok(dataset) => dataset,
            Err(error) if error.to_string().contains("empty data") => {
                return Ok((Vec::new(), Vec::new()));
            }
            Err(error) => return Err(error),
        };

        let (types, rows) = dataset.split();
        let column_count = types.len();
        let columns: Vec<String> = types.iter().map(|(n, _)| n.clone()).collect();

        let rows: Vec<serde_json::Value> = rows
            .into_iter()
            .take(max_rows)
            .map(|row| {
                let row_data: Vec<serde_json::Value> = row.into_json();
                let mut map = serde_json::Map::new();
                for (i, v) in row_data.into_iter().enumerate() {
                    let (col_name, val) = if i < column_count {
                        (types[i].0.clone(), v)
                    } else {
                        (format!("col_{}", i), v)
                    };
                    map.insert(col_name, val);
                }
                serde_json::Value::Object(map)
            })
            .collect();

        Ok((columns, rows))
    }

    async fn execute_postgres(
        &self,
        sql: &str,
        config_json: &serde_json::Value,
        max_rows: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
        #[derive(Debug, Deserialize)]
        struct PgConfig {
            host: String,
            port: u16,
            database: String,
            username: String,
            password: String,
        }
        let config_val = decrypt_config(config_json, &self.state.data_dir)?;
        let cfg: PgConfig = serde_json::from_value(config_val)?;
        let url = crate::routes::data_sources::build_postgres_url_parts(
            &cfg.username,
            &cfg.password,
            &cfg.host,
            cfg.port,
            &cfg.database,
        );
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .connect(&url)
            .await
            .map_err(|e| anyhow::anyhow!("postgres connection failed: {}", e))?;

        let sql_rows = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            sqlx::query(sql).fetch_all(&pool),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Query execution timed out"))?
        .map_err(|e| anyhow::anyhow!("postgres query failed: {}", e))?;

        pool.close().await;

        let columns: Vec<String> = if sql_rows.is_empty() {
            vec![]
        } else {
            sql_rows[0]
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect()
        };

        let rows: Vec<serde_json::Value> = sql_rows
            .into_iter()
            .take(max_rows)
            .map(|row| {
                let mut map = serde_json::Map::new();
                for (i, col) in row.columns().iter().enumerate() {
                    let value = decode_pg_cell(&row, i);
                    map.insert(col.name().to_string(), value);
                }
                serde_json::Value::Object(map)
            })
            .collect();

        Ok((columns, rows))
    }

    /// P2-4: Validate that a MERGE step uses columns from known cross-datasource relations.
    /// Returns Some(error_message) if the MERGE uses JOIN columns without a known relationship.
    fn validate_merge_step(
        &self,
        strategy: &crate::nl2sql::MergeStrategy,
        schemas: &[DatasourceSchemaInfo],
    ) -> Option<String> {
        // Only validate JOIN strategies (not UnionAll)
        let join_cols: Vec<&str> = match strategy {
            crate::nl2sql::MergeStrategy::InnerJoin { on } => {
                on.iter().map(|s| s.as_str()).collect()
            }
            crate::nl2sql::MergeStrategy::LeftJoin { on } => {
                on.iter().map(|s| s.as_str()).collect()
            }
            crate::nl2sql::MergeStrategy::RightJoin { on } => {
                on.iter().map(|s| s.as_str()).collect()
            }
            crate::nl2sql::MergeStrategy::FullOuterJoin { on } => {
                on.iter().map(|s| s.as_str()).collect()
            }
            _ => return None,
        };

        if join_cols.is_empty() {
            return None;
        }

        // Collect all known cross-datasource relation columns
        let known_cols: std::collections::HashSet<String> = schemas
            .iter()
            .flat_map(|s| s.cross_datasource_relations.iter())
            .flat_map(|r| [r.left_column.clone(), r.right_column.clone()])
            .collect();

        // Check each join column against known relations
        let unknown: Vec<&str> = join_cols
            .iter()
            .filter(|col| !known_cols.contains(&(**col).to_string()))
            .copied()
            .collect();

        if unknown.is_empty() {
            None
        } else {
            Some(format!(
                "join columns {} are not registered in any cross-datasource relation (MERGE will proceed but join quality may be poor)",
                unknown.join(", ")
            ))
        }
    }

    /// Execute a Merge step (hash join or union all).
    fn execute_merge_step(
        &self,
        step_id: usize,
        strategy: &crate::nl2sql::MergeStrategy,
        inputs: &[crate::nl2sql::MergeInput],
        output_name: &str,
        intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
    ) -> anyhow::Result<crate::nl2sql::StepResult> {
        let start = std::time::Instant::now();
        let execution_ms = || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        if let Some(error) = merge_input_error(inputs, intermediate) {
            tracing::warn!(
                step_id,
                output_name,
                error = %error,
                "execute_merge_step: refusing to merge failed or missing input"
            );
            return Ok(crate::nl2sql::StepResult {
                step_id,
                output_name: output_name.to_owned(),
                sql: None,
                columns: vec![],
                rows: vec![],
                row_count: 0,
                execution_ms: execution_ms(),
                error: Some(format!("merge step blocked: {error}")),
                datasource_id: None,
                execution_attempts: Vec::new(),
                diagnostic_only: false,
                recovery_note: None,
            });
        }

        let diagnostic_only = inputs.iter().any(|input| {
            intermediate
                .get(&input.input_name)
                .is_some_and(|result| result.diagnostic_only)
        });
        let recovery_note = inputs.iter().find_map(|input| {
            intermediate
                .get(&input.input_name)
                .and_then(|result| result.recovery_note.clone())
        });

        let result: anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> = match strategy {
            crate::nl2sql::MergeStrategy::UnionAll => union_all(inputs, intermediate),
            crate::nl2sql::MergeStrategy::UnionDistinct => union_distinct(inputs, intermediate),
            crate::nl2sql::MergeStrategy::InnerJoin { on } => {
                hash_join(inputs, intermediate, on, false)
            }
            crate::nl2sql::MergeStrategy::LeftJoin { on } => {
                hash_join(inputs, intermediate, on, true)
            }
            crate::nl2sql::MergeStrategy::RightJoin { on } => right_join(inputs, intermediate, on),
            crate::nl2sql::MergeStrategy::FullOuterJoin { on } => {
                full_outer_join(inputs, intermediate, on)
            }
            crate::nl2sql::MergeStrategy::CrossJoin => cross_join(inputs, intermediate),
        };

        match result {
            Ok((columns, rows)) => {
                let row_count = rows.len();
                let json_rows: Vec<serde_json::Value> = rows;
                Ok(crate::nl2sql::StepResult {
                    step_id,
                    output_name: output_name.to_owned(),
                    sql: None,
                    columns,
                    rows: json_rows,
                    row_count,
                    execution_ms: execution_ms(),
                    error: None,
                    datasource_id: None,
                    execution_attempts: Vec::new(),
                    diagnostic_only,
                    recovery_note,
                })
            }
            Err(e) => Ok(crate::nl2sql::StepResult {
                step_id,
                output_name: output_name.to_owned(),
                sql: None,
                columns: vec![],
                rows: vec![],
                row_count: 0,
                execution_ms: execution_ms(),
                error: Some(format!("merge step failed: {}", e)),
                datasource_id: None,
                execution_attempts: Vec::new(),
                diagnostic_only,
                recovery_note,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_trino_user_permit, audit_agent_semantic_candidate, contextual_agent_question,
        datasource_visible_to_user, deterministic_dialect_repair, dialect_preflight_error,
        execute_trino_query_bounded, merge_input_error, AgentSemanticGuard,
        DatasourceRequestBudget, AGENT_SHARED_CONTEXT_MAX_CHARS,
    };
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::{delete, get, post};
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn agent_semantic_guard_releases_matching_sql_and_blocks_grain_drift() {
        let mut intent = super::super::semantic_audit::compile_question_intent(
            "tenant",
            "datasource",
            "按设备统计订单数",
            &[],
        );
        super::super::semantic_audit::bind_schema_dimensions(
            &mut intent,
            &serde_json::json!([{
                "table_name": "task_offer",
                "columns": [{"name": "executor_device_id"}, {"name": "order_id"}]
            }]),
            &[],
        );
        let guard = AgentSemanticGuard {
            conversation_id: "conversation".into(),
            intent_id: "intent".into(),
            datasource_id: "datasource".into(),
            intent,
            metric_contracts: vec![],
            join_contracts: vec![],
        };
        let matching = audit_agent_semantic_candidate(
            &guard,
            "SELECT executor_device_id, COUNT(*) AS order_count FROM task_offer GROUP BY executor_device_id",
        )
        .expect("matching SQL audit");
        assert_eq!(
            matching.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release
        );
        let drifting = audit_agent_semantic_candidate(
            &guard,
            "SELECT COUNT(*) AS order_count FROM task_offer",
        )
        .expect("drifting SQL audit");
        assert_ne!(
            drifting.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release
        );
    }

    #[test]
    fn datasource_visibility_includes_tenant_shared_sources_for_members() {
        assert!(datasource_visible_to_user(
            false,
            "member-a",
            Some("owner-b"),
            "tenant"
        ));
        assert!(datasource_visible_to_user(
            false,
            "member-a",
            Some("member-a"),
            "private"
        ));
        assert!(!datasource_visible_to_user(
            false,
            "member-a",
            Some("owner-b"),
            "private"
        ));
        assert!(datasource_visible_to_user(
            true,
            "admin-a",
            Some("owner-b"),
            "private"
        ));
    }

    #[derive(Clone)]
    struct FakeTrinoState {
        base_url: String,
        submissions: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        cancellations: Arc<AtomicUsize>,
        submit_delay: std::time::Duration,
        poll_delay: Option<std::time::Duration>,
    }

    fn fake_trino_stats() -> serde_json::Value {
        serde_json::json!({
            "state": "FINISHED",
            "queued": false,
            "scheduled": true,
            "nodes": 1,
            "totalSplits": 1,
            "queuedSplits": 0,
            "runningSplits": 0,
            "completedSplits": 1,
            "cpuTimeMillis": 1,
            "wallTimeMillis": 1,
            "queuedTimeMillis": 0,
            "elapsedTimeMillis": 1,
            "processedRows": 1,
            "processedBytes": 1,
            "peakMemoryBytes": 1,
            "spilledBytes": 0
        })
    }

    async fn fake_trino_submit(State(state): State<FakeTrinoState>) -> Json<serde_json::Value> {
        let submission = state.submissions.fetch_add(1, Ordering::SeqCst) + 1;
        let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
        state.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(state.submit_delay).await;
        state.active.fetch_sub(1, Ordering::SeqCst);

        let query_id = format!("query-{submission}");
        let next_uri = state
            .poll_delay
            .map(|_| format!("{}/v1/next/{query_id}", state.base_url));
        Json(serde_json::json!({
            "id": query_id,
            "infoUri": format!("{}/ui/query.html", state.base_url),
            "partialCancelUri": null,
            "nextUri": next_uri,
            "columns": if next_uri.is_none() {
                serde_json::json!([{
                    "name": "value",
                    "type": "varchar",
                    "typeSignature": {"rawType": "varchar", "arguments": []}
                }])
            } else {
                serde_json::Value::Null
            },
            "data": if next_uri.is_none() {
                serde_json::json!([["ok"]])
            } else {
                serde_json::Value::Null
            },
            "error": null,
            "stats": fake_trino_stats(),
            "warnings": [],
            "updateType": null,
            "updateCount": null
        }))
    }

    async fn fake_trino_poll(State(state): State<FakeTrinoState>) -> Json<serde_json::Value> {
        tokio::time::sleep(
            state
                .poll_delay
                .unwrap_or_else(|| std::time::Duration::from_millis(1)),
        )
        .await;
        Json(serde_json::json!({
            "id": "query-poll",
            "infoUri": format!("{}/ui/query.html", state.base_url),
            "partialCancelUri": null,
            "nextUri": null,
            "columns": [{
                "name": "value",
                "type": "varchar",
                "typeSignature": {"rawType": "varchar", "arguments": []}
            }],
            "data": [["ok"]],
            "error": null,
            "stats": fake_trino_stats(),
            "warnings": [],
            "updateType": null,
            "updateCount": null
        }))
    }

    async fn fake_trino_cancel(State(state): State<FakeTrinoState>) -> StatusCode {
        state.cancellations.fetch_add(1, Ordering::SeqCst);
        StatusCode::NO_CONTENT
    }

    async fn start_fake_trino(
        submit_delay: std::time::Duration,
        poll_delay: Option<std::time::Duration>,
    ) -> (FakeTrinoState, u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Trino");
        let port = listener.local_addr().expect("fake Trino address").port();
        let state = FakeTrinoState {
            base_url: format!("http://127.0.0.1:{port}"),
            submissions: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            cancellations: Arc::new(AtomicUsize::new(0)),
            submit_delay,
            poll_delay,
        };
        let app = Router::new()
            .route("/v1/statement", post(fake_trino_submit))
            .route("/v1/next/{query_id}", get(fake_trino_poll))
            .route("/v1/query/{query_id}", delete(fake_trino_cancel))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fake Trino");
        });
        (state, port, server)
    }

    fn fake_trino_client(port: u16) -> trino_rust_client::Client {
        trino_rust_client::ClientBuilder::new("test-user", "127.0.0.1")
            .port(port)
            .catalog("memory")
            .schema("default")
            .secure(false)
            .max_attempt(0)
            .build()
            .expect("build fake Trino client")
    }

    #[tokio::test]
    async fn datasource_budget_limits_concurrency_without_becoming_a_lifetime_quota() {
        let budget = DatasourceRequestBudget::new(3);
        let first = budget.acquire("one").await.expect("first permit");
        let second = budget.acquire("two").await.expect("second permit");
        let third = budget.acquire("three").await.expect("third permit");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), budget.acquire("four"),)
                .await
                .is_err(),
            "the fourth concurrent request must wait"
        );

        drop(first);
        let fourth =
            tokio::time::timeout(std::time::Duration::from_secs(1), budget.acquire("four"))
                .await
                .expect("fourth request should proceed after a completion")
                .expect("fourth permit");
        drop((second, third, fourth));
    }

    #[tokio::test]
    async fn fourth_concurrent_trino_request_waits_for_the_same_user() {
        let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let user = format!("user-{}", uuid::Uuid::new_v4());
        let first = acquire_trino_user_permit(&tenant, &user)
            .await
            .expect("first permit");
        let second = acquire_trino_user_permit(&tenant, &user)
            .await
            .expect("second permit");
        let third = acquire_trino_user_permit(&tenant, &user)
            .await
            .expect("third permit");

        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                acquire_trino_user_permit(&tenant, &user),
            )
            .await
            .is_err(),
            "the fourth same-user request must wait"
        );

        drop(first);
        let fourth = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            acquire_trino_user_permit(&tenant, &user),
        )
        .await
        .expect("fourth permit should wake after one slot is released")
        .expect("fourth permit");
        drop((second, third, fourth));
    }

    #[tokio::test]
    async fn trino_concurrency_is_isolated_between_users_in_one_tenant() {
        let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let first_user = format!("user-a-{}", uuid::Uuid::new_v4());
        let second_user = format!("user-b-{}", uuid::Uuid::new_v4());
        let mut first_user_permits = Vec::new();
        for _ in 0..3 {
            first_user_permits.push(
                acquire_trino_user_permit(&tenant, &first_user)
                    .await
                    .expect("first user permit"),
            );
        }

        let second_user_permit = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            acquire_trino_user_permit(&tenant, &second_user),
        )
        .await
        .expect("another user in the tenant must not be blocked")
        .expect("second user permit");
        drop((first_user_permits, second_user_permit));
    }

    #[tokio::test]
    async fn actual_trino_submissions_never_exceed_three_for_one_user() {
        let (state, port, server) =
            start_fake_trino(std::time::Duration::from_millis(150), None).await;
        let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let user = format!("user-{}", uuid::Uuid::new_v4());
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let tenant = tenant.clone();
            let user = user.clone();
            tasks.push(tokio::spawn(async move {
                execute_trino_query_bounded(
                    fake_trino_client(port),
                    "SELECT 'ok' AS value".to_string(),
                    5,
                    &tenant,
                    &user,
                    "test query",
                    DatasourceRequestBudget::new(1),
                )
                .await
            }));
        }
        for task in tasks {
            task.await.expect("join fake Trino query").expect("query");
        }

        assert_eq!(state.submissions.load(Ordering::SeqCst), 4);
        assert_eq!(state.max_active.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn shared_request_budget_allows_later_trino_submissions_after_completion() {
        let (state, port, server) =
            start_fake_trino(std::time::Duration::from_millis(1), None).await;
        let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let user = format!("user-{}", uuid::Uuid::new_v4());
        let budget = DatasourceRequestBudget::new(3);
        for _ in 0..3 {
            execute_trino_query_bounded(
                fake_trino_client(port),
                "SELECT 'ok' AS value".to_string(),
                5,
                &tenant,
                &user,
                "test query",
                budget.clone(),
            )
            .await
            .expect("bounded query");
        }
        execute_trino_query_bounded(
            fake_trino_client(port),
            "SELECT 'later' AS value".to_string(),
            5,
            &tenant,
            &user,
            "fourth query",
            budget,
        )
        .await
        .expect("fourth query should run after the first three completed");

        assert_eq!(state.submissions.load(Ordering::SeqCst), 4);
        server.abort();
    }

    #[tokio::test]
    async fn timed_out_trino_query_is_cancelled_without_recovery_submission() {
        let (state, port, server) = start_fake_trino(
            std::time::Duration::from_millis(1),
            Some(std::time::Duration::from_secs(5)),
        )
        .await;
        let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let user = format!("user-{}", uuid::Uuid::new_v4());
        let error = execute_trino_query_bounded(
            fake_trino_client(port),
            "SELECT 'slow' AS value".to_string(),
            1,
            &tenant,
            &user,
            "timeout query",
            DatasourceRequestBudget::new(3),
        )
        .await
        .expect_err("query should time out");

        assert!(error.to_string().contains("timed out"));
        assert_eq!(state.submissions.load(Ordering::SeqCst), 1);
        assert_eq!(state.cancellations.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn responsive_trino_query_may_outlive_each_inactivity_window() {
        // Submission and polling each respond within one second, but their
        // combined wall-clock duration exceeds one second. The old absolute
        // deadline cancelled this healthy query during the poll.
        let (state, port, server) = start_fake_trino(
            std::time::Duration::from_millis(700),
            Some(std::time::Duration::from_millis(700)),
        )
        .await;
        let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let user = format!("user-{}", uuid::Uuid::new_v4());
        let dataset = execute_trino_query_bounded(
            fake_trino_client(port),
            "SELECT 'slow-but-responsive' AS value".to_string(),
            1,
            &tenant,
            &user,
            "responsive slow query",
            DatasourceRequestBudget::new(1),
        )
        .await
        .expect("responsive query must not be cancelled by total wall time");

        assert_eq!(dataset.len(), 1);
        assert_eq!(state.submissions.load(Ordering::SeqCst), 1);
        assert_eq!(state.cancellations.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    #[ignore = "requires explicit AOS_REAL_TRINO_* environment variables and network access"]
    async fn real_trino_single_submission_smoke() {
        use sqlx::{sqlite::SqliteConnectOptions, Row};

        let db_path = std::env::var("AOS_REAL_TRINO_DB_PATH")
            .expect("AOS_REAL_TRINO_DB_PATH must point to an existing AOS SQLite database");
        let data_dir = std::env::var("AOS_REAL_TRINO_DATA_DIR")
            .expect("AOS_REAL_TRINO_DATA_DIR must point to its matching data directory");
        let datasource_id = std::env::var("AOS_REAL_TRINO_DATASOURCE_ID")
            .expect("AOS_REAL_TRINO_DATASOURCE_ID must identify one Trino datasource");

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .read_only(true);
        let pool = sqlx::SqlitePool::connect_with(options)
            .await
            .expect("open AOS database read-only");
        let row = sqlx::query(
            "SELECT tenant_id, user_id, db_type, config FROM data_sources WHERE id = ?",
        )
        .bind(&datasource_id)
        .fetch_one(&pool)
        .await
        .expect("load Trino datasource");
        let tenant_id: String = row.get("tenant_id");
        let user_id: Option<String> = row.get("user_id");
        let db_type: String = row.get("db_type");
        assert!(matches!(db_type.as_str(), "trino" | "presto"));
        let encrypted: serde_json::Value = row.get("config");
        let config = crate::routes::data_sources::decrypt_config(
            &encrypted,
            std::path::Path::new(&data_dir),
        )
        .expect("decrypt Trino datasource config");

        let host = config["host"].as_str().expect("Trino host");
        let normalized_host = nl2sql_domain::datasource_config::normalize_host_input(host);
        let configured_port = config["port"].as_u64().unwrap_or(8080) as u16;
        let port = normalized_host.port.unwrap_or(configured_port);
        let username = config["username"].as_str().expect("Trino username");
        let catalog = config["catalog"].as_str().expect("Trino catalog");
        let schema = config["schema"].as_str().unwrap_or("default");
        let password = config["password"].as_str().unwrap_or_default();
        let secure = config["ssl"]
            .as_bool()
            .or(normalized_host.secure)
            .unwrap_or(port == 443);
        let mut builder = trino_rust_client::ClientBuilder::new(username, &normalized_host.host)
            .port(port)
            .catalog(catalog)
            .schema(schema)
            .secure(secure);
        if config["basic_auth"]
            .as_bool()
            .unwrap_or(!password.is_empty())
        {
            builder = builder.auth(trino_rust_client::auth::Auth::Basic(
                username.to_string(),
                Some(password.to_string()),
            ));
        }
        let client = builder.max_attempt(0).build().expect("build Trino client");
        let effective_user = user_id.as_deref().unwrap_or(&tenant_id);

        let dataset = execute_trino_query_bounded(
            client,
            "SELECT 1 AS aos_smoke_check".to_string(),
            10,
            &tenant_id,
            effective_user,
            "explicit real Trino single-submission smoke test",
            DatasourceRequestBudget::new(1),
        )
        .await
        .expect("single Trino SELECT 1 should succeed");
        assert_eq!(dataset.len(), 1);
    }

    #[test]
    fn mysql_tidb_recursive_cte_is_rejected_before_execution() {
        let sql = "WITH RECURSIVE date_spine AS (SELECT CURDATE()) SELECT * FROM date_spine";
        assert!(dialect_preflight_error("tidb", sql).is_some());
        assert!(dialect_preflight_error("mysql", sql).is_some());
        assert!(dialect_preflight_error("trino", sql).is_none());
    }

    #[test]
    fn trino_identifier_error_quotes_only_the_failed_output_alias() {
        let sql = "SELECT current_date AS report_date, 'AS current_date' AS note, dt AS current_date FROM metrics -- AS current_date";
        let error = "Query failed with SYNTAX_ERROR: line 1:43: mismatched input 'current_date'. Expecting: <identifier>";

        assert_eq!(
            deterministic_dialect_repair("trino", sql, error).as_deref(),
            Some("SELECT current_date AS report_date, 'AS current_date' AS note, dt AS \"current_date\" FROM metrics -- AS current_date")
        );
        assert!(deterministic_dialect_repair("mysql", sql, error).is_none());
        assert!(deterministic_dialect_repair("trino", sql, "Query execution timed out").is_none());
    }

    #[test]
    fn merge_rejects_failed_input_as_evidence() {
        let inputs = vec![crate::nl2sql::MergeInput {
            input_name: "failed_query".to_string(),
            alias: None,
        }];
        let intermediate = std::collections::HashMap::from([(
            "failed_query".to_string(),
            crate::nl2sql::StepResult {
                step_id: 0,
                output_name: "failed_query".to_string(),
                sql: Some("SELECT broken".to_string()),
                columns: vec![],
                rows: vec![],
                row_count: 0,
                execution_ms: 1,
                error: Some("syntax error".to_string()),
                datasource_id: Some("ds-1".to_string()),
                execution_attempts: Vec::new(),
                diagnostic_only: false,
                recovery_note: None,
            },
        )]);

        let error = merge_input_error(&inputs, &intermediate).expect("failed input is rejected");
        assert!(error.contains("failed_query"));
        assert!(error.contains("syntax error"));
    }

    #[test]
    fn contextual_question_keeps_current_task_separate_and_last() {
        let text =
            contextual_agent_question("查最近订单", Some("上一轮分析的是 ROI，数据源是 aos"));
        assert!(text.contains("上一轮分析的是 ROI"));
        assert!(text.contains("不得延续或覆盖旧任务"));
        assert!(text.ends_with("查最近订单"));
    }

    #[test]
    fn contextual_question_bounds_old_context_without_cutting_current_question() {
        let text = contextual_agent_question(
            "当前问题必须完整保留",
            Some(&"旧".repeat(AGENT_SHARED_CONTEXT_MAX_CHARS + 1_000)),
        );
        assert!(text.contains("older shared context truncated"));
        assert!(text.ends_with("当前问题必须完整保留"));
        assert!(text.chars().count() < AGENT_SHARED_CONTEXT_MAX_CHARS + 200);
    }
}
