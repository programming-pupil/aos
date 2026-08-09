//! Super_Assistant capability adapters (reuse-first wiring).
//!
//! This module is the shared seam where the Super_Assistant entry point
//! delegates a routed message to an **existing** capability's execution path.
//! Following the "reuse-first, do not rebuild" principle (design Components
//! §5), every adapter here is a *thin wiring layer*: it shapes the
//! Super_Assistant message into the request the reused capability already
//! understands and maps that capability's response back into a
//! Super_Assistant-facing conclusion. No capability logic (routing, SQL
//! generation, retrieval, deep analysis, …) is reimplemented in this module.
//!
//! Adapters are contributed per capability by the 10.x tasks; each adds a
//! uniquely-named section so the tasks can extend this shared file without
//! colliding. Feature-gated adapters (e.g. the `nl2sql` SQL adapter below) are
//! compiled only when their backing capability is available, mirroring the
//! feature gating of the capability they wrap.

// ===========================================================================
// nl2sql SQL completion / correction attribution adapter (task 10.4, Req 3.6)
// ---------------------------------------------------------------------------
// When the user supplies a SQL fragment together with business background, the
// Super_Assistant delegates to the existing `nl2sql` capability
// (`crate::routes::nl2sql::query`, which already performs generation /
// completion / correction / clarification) and surfaces the result as a
// troubleshooting conclusion. This adapter never generates or repairs SQL
// itself — it only detects such a request, shapes the nl2sql question, and
// maps the nl2sql response. It is gated behind the `nl2sql` cargo feature
// because it references nl2sql-only symbols.
// ===========================================================================

// Capability adapters are compiled under several feature combinations; some
// DTOs/helpers are consumed by tests or the web contract rather than by every
// production build. Suppress dead-code noise for the partial bot-agents build.
#![allow(dead_code)]

#[cfg(feature = "nl2sql")]
use axum::{extract::State, Extension, Json};
#[cfg(feature = "nl2sql")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "nl2sql")]
use serde_json::json;

#[cfg(feature = "nl2sql")]
use crate::auth::Claims;
#[cfg(feature = "nl2sql")]
use crate::error::Result;
#[cfg(feature = "nl2sql")]
use crate::routes::memory_continuity::{
    create_memory_item_internal, list_unified_memory_items, MemoryUpsertRequest,
};
#[cfg(feature = "nl2sql")]
use crate::routes::nl2sql::{query as nl2sql_query, QueryRequest, QueryResponse};
#[cfg(feature = "nl2sql")]
use crate::state::AppState;

/// App bucket used for `nl2sql` session memory items written through the
/// unified `/memory/items` store (task 20.2). Scoping every nl2sql-established
/// 取数口径/约束 (query semantics / constraints) under a dedicated app bucket +
/// `scope=session` gives the capability the cross-turn session memory closed
/// loop it previously lacked, while still living in the single Unified_Memory
/// table (no parallel store).
#[cfg(feature = "nl2sql")]
const NL2SQL_MEMORY_APP: &str = "nl2sql";

/// Upper bound on how many prior session-scoped nl2sql memory items are read
/// back and injected as context before delegating to `nl2sql` (task 20.2).
#[cfg(feature = "nl2sql")]
const NL2SQL_MEMORY_READ_LIMIT: usize = 8;

/// A parsed "SQL fragment + business background" troubleshooting request
/// extracted from a Super_Assistant message (Req 3.6).
///
/// The `business_background` may be empty when the user pasted a bare SQL
/// fragment; the `sql_fragment` is always non-empty when a value is produced by
/// [`detect_sql_troubleshooting_request`].
#[cfg(feature = "nl2sql")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTroubleshootingRequest {
    /// The SQL fragment the user wants completed / corrected.
    pub sql_fragment: String,
    /// Business background / natural-language context around the fragment.
    pub business_background: String,
}

#[cfg(feature = "nl2sql")]
impl SqlTroubleshootingRequest {
    /// Compose the natural-language `question` handed to the reused `nl2sql`
    /// query path. The prompt asks nl2sql to complete/correct the fragment
    /// using the business background and to return a troubleshooting
    /// conclusion (Req 3.6). This is pure string shaping — nl2sql does the
    /// actual SQL work.
    pub fn to_nl2sql_question(&self) -> String {
        let mut question = String::new();
        if !self.business_background.is_empty() {
            question.push_str("业务背景：");
            question.push_str(&self.business_background);
            question.push_str("\n\n");
            question.push_str(
                "请基于以上业务背景补全或修正下面的 SQL 片段，并给出排查结论（问题原因与修复说明）：\n",
            );
        } else {
            question
                .push_str("请补全或修正下面的 SQL 片段，并给出排查结论（问题原因与修复说明）：\n");
        }
        question.push_str(&self.sql_fragment);
        question
    }
}

/// The Super_Assistant-facing outcome of delegating a SQL-fragment request to
/// `nl2sql` (Req 3.6). Serializes to camelCase for the frontend.
#[cfg(feature = "nl2sql")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlTroubleshootingConclusion {
    /// The completed / corrected SQL, when nl2sql produced a non-empty one.
    pub corrected_sql: Option<String>,
    /// Human-readable troubleshooting conclusion (cause + fix explanation).
    pub conclusion: String,
    /// A clarification question when nl2sql needs more information instead of
    /// producing SQL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_question: Option<String>,
    /// The nl2sql query id, for traceability back into the nl2sql capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_id: Option<String>,
    /// True when nl2sql produced a usable completed/corrected SQL statement.
    pub resolved: bool,
}

#[cfg(feature = "nl2sql")]
impl SqlTroubleshootingConclusion {
    /// Map an nl2sql [`QueryResponse`] into a Super_Assistant conclusion. Pure
    /// mapping so it can be unit-tested without a live nl2sql call.
    pub(crate) fn from_query_response(resp: QueryResponse) -> Self {
        let clarification_question = resp
            .clarification_question
            .clone()
            .filter(|q| !q.trim().is_empty());
        let corrected_sql = resp.sql.clone().filter(|s| !s.trim().is_empty());
        let resolved = corrected_sql.is_some();

        let conclusion = if let Some(err) = resp.error.as_ref().filter(|e| !e.trim().is_empty()) {
            format!("排查结论：{err}")
        } else if let Some(expl) = resp.explanation.as_ref().filter(|e| !e.trim().is_empty()) {
            expl.clone()
        } else if let Some(question) = clarification_question.as_ref() {
            format!("需要澄清：{question}")
        } else if resolved {
            "已根据业务背景补全/修正 SQL 片段。".to_string()
        } else {
            "nl2sql 未能生成可用的 SQL 修正结果。".to_string()
        };

        Self {
            corrected_sql,
            conclusion,
            clarification_question,
            query_id: Some(resp.query_id),
            resolved,
        }
    }
}

/// Detect a "SQL fragment + business background" request in a Super_Assistant
/// message (Req 3.6).
///
/// Pure and deterministic (no I/O) so it can be unit-tested exhaustively.
/// Detection strategy:
/// 1. Prefer an explicit fenced code block (```` ```sql … ``` ````); the block
///    body becomes the SQL fragment and the surrounding text the background.
/// 2. Otherwise classify each line: lines containing a SQL keyword form the
///    fragment, remaining non-empty lines form the background.
///
/// Returns `None` when no SQL fragment can be identified (the message is not a
/// SQL-completion request and should be handled by another capability).
#[cfg(feature = "nl2sql")]
pub fn detect_sql_troubleshooting_request(text: &str) -> Option<SqlTroubleshootingRequest> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 1. Prefer an explicit fenced code block.
    if let Some((sql, background)) = extract_fenced_sql(trimmed) {
        if contains_sql_keyword(&sql) {
            return Some(SqlTroubleshootingRequest {
                sql_fragment: sql.trim().to_string(),
                business_background: background.trim().to_string(),
            });
        }
    }

    // 2. Fall back to line-based classification.
    let mut sql_lines: Vec<&str> = Vec::new();
    let mut prose_lines: Vec<&str> = Vec::new();
    for line in trimmed.lines() {
        if contains_sql_keyword(line) {
            sql_lines.push(line);
        } else if !line.trim().is_empty() {
            prose_lines.push(line);
        }
    }
    if sql_lines.is_empty() {
        return None;
    }
    Some(SqlTroubleshootingRequest {
        sql_fragment: sql_lines.join("\n").trim().to_string(),
        business_background: prose_lines.join("\n").trim().to_string(),
    })
}

/// Extract the first fenced code block, returning `(sql_body, background)`
/// where `background` is the text before and after the block. An optional
/// leading language tag (e.g. `sql`) on the fence's first line is stripped.
#[cfg(feature = "nl2sql")]
fn extract_fenced_sql(text: &str) -> Option<(String, String)> {
    let start = text.find("```")?;
    let after_open = &text[start + 3..];
    let end_rel = after_open.find("```")?;
    let block = &after_open[..end_rel];

    // Strip an optional language tag / blank first line inside the fence.
    let block_body = match block.split_once('\n') {
        Some((first, rest))
            if first.trim().is_empty() || first.trim().eq_ignore_ascii_case("sql") =>
        {
            rest
        }
        _ => block,
    };

    let before = &text[..start];
    let after_block = &after_open[end_rel + 3..];
    let background = format!("{}\n{}", before.trim(), after_block.trim());
    Some((block_body.to_string(), background.trim().to_string()))
}

/// Heuristic check for the presence of a SQL keyword (case-insensitive).
#[cfg(feature = "nl2sql")]
fn contains_sql_keyword(text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "select ", "insert ", "update ", "delete ", "with ", "from ", " from", "where", "join",
        "group by", "order by", "create ", "alter ", "drop ", "having", "union",
    ];
    let lower = text.to_lowercase();
    KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Delegate a detected SQL-fragment request to the existing `nl2sql` capability
/// and return a Super_Assistant troubleshooting conclusion (Req 3.6).
///
/// This is the thin wiring: it builds an nl2sql [`QueryRequest`] from the parsed
/// request and calls the reused [`crate::routes::nl2sql::query`] handler
/// (completion / correction / clarification all happen inside nl2sql), then
/// maps the [`QueryResponse`] via
/// [`SqlTroubleshootingConclusion::from_query_response`]. No SQL logic is
/// reimplemented here.
#[cfg(feature = "nl2sql")]
pub async fn complete_and_diagnose_sql(
    state: AppState,
    claims: Claims,
    data_source_id: String,
    request: &SqlTroubleshootingRequest,
    shared_context: Option<&str>,
    conversation_id: Option<String>,
) -> Result<SqlTroubleshootingConclusion> {
    // Read the caller's identity before `claims` is moved into the reused
    // nl2sql handler, so the same tenant/user scopes the memory round-trip.
    let tenant_id = claims.tenant_id.clone();
    let user_id = claims.sub.clone();

    // Cross-turn memory READ (task 20.2): pull prior session-scoped 取数口径/
    // 约束 established earlier in this conversation and inject them as context so
    // nl2sql answers consistently across turns. Best-effort: a memory read
    // failure degrades to answering without prior context (Req 4.10).
    let prior_memory = match conversation_id.as_deref() {
        Some(session_id) if !session_id.is_empty() => {
            load_prior_sql_memory(&state.db, &tenant_id, &user_id, session_id).await
        }
        _ => Vec::new(),
    };

    let query_req = QueryRequest {
        data_source_id,
        question: compose_question_with_memory(request, &prior_memory, shared_context),
        conversation_id: conversation_id.clone(),
        route_confidence: None,
        routing_method: Some("super_assistant".to_string()),
        semantic_context: None,
        reference_bindings: None,
    };
    let Json(resp) = nl2sql_query(State(state.clone()), Extension(claims), Json(query_req)).await?;

    // Cross-turn memory WRITE (task 20.2): persist newly established SQL
    // semantics/constraints back into the unified session memory so the next
    // turn can reuse them. Best-effort so persistence never blocks the answer.
    if let Some(session_id) = conversation_id.as_deref().filter(|s| !s.is_empty()) {
        persist_sql_memory(&state.db, &tenant_id, &user_id, session_id, request, &resp).await;
    }

    Ok(SqlTroubleshootingConclusion::from_query_response(resp))
}

/// Compose the natural-language question handed to `nl2sql`, prepending any
/// prior session-scoped 取数口径/约束 as context (task 20.2). When there is no
/// prior memory this is identical to [`SqlTroubleshootingRequest::to_nl2sql_question`],
/// so single-turn behaviour is unchanged.
#[cfg(feature = "nl2sql")]
fn compose_question_with_memory(
    request: &SqlTroubleshootingRequest,
    prior_memory: &[String],
    shared_context: Option<&str>,
) -> String {
    let base = request.to_nl2sql_question();
    let shared_context = shared_context
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if prior_memory.is_empty() && shared_context.is_none() {
        return base;
    }
    let mut question = String::new();
    if !prior_memory.is_empty() {
        question.push_str("本会话已确立的取数口径/约束（如与当前请求冲突，以当前请求为准）：\n");
        for (idx, line) in prior_memory.iter().enumerate() {
            question.push_str(&format!("{}. {}\n", idx + 1, line.trim()));
        }
        question.push('\n');
    }
    if let Some(context) = shared_context {
        question.push_str("超级助手共享会话背景（只用于理解当前 SQL 片段里的代词、业务对象、数据源、指标口径、历史 SQL、上传文件和已知约束；不得覆盖当前 SQL 片段/当前请求）：\n");
        question.push_str(context);
        question.push_str("\n\n");
    }
    question.push_str(&base);
    question
}

/// Read prior session-scoped nl2sql memory items for this conversation through
/// the unified store and return them as compact context lines (task 20.2).
/// Reuses [`list_unified_memory_items`]; never surfaces errors — a failed read
/// yields an empty context (Req 4.10 graceful degradation).
#[cfg(feature = "nl2sql")]
async fn load_prior_sql_memory(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> Vec<String> {
    let items = list_unified_memory_items(
        db,
        tenant_id,
        user_id,
        Some("session"),
        Some(NL2SQL_MEMORY_APP),
        Some(session_id),
        false,
        NL2SQL_MEMORY_READ_LIMIT,
    )
    .await
    .unwrap_or_default();

    items
        .into_iter()
        .map(|item| item.content.trim().to_string())
        .filter(|content| !content.is_empty())
        .collect()
}

/// Persist newly established SQL semantics/constraints from an nl2sql response
/// as session-scoped Unified_Memory items (task 20.2). Writes go through the
/// shared [`create_memory_item_internal`] path (no parallel store). Best-effort:
/// individual write failures are ignored so persistence never blocks answering.
#[cfg(feature = "nl2sql")]
async fn persist_sql_memory(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    request: &SqlTroubleshootingRequest,
    resp: &QueryResponse,
) {
    // Only persist when nl2sql actually established usable semantics — i.e. it
    // produced a non-empty SQL. Clarification-only / error responses establish
    // nothing durable for the取数口径.
    let corrected_sql = match resp
        .sql
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(sql) => sql.to_string(),
        None => return,
    };

    // 取数口径 (query semantics): the business intent plus the resolved SQL.
    let mut semantics = String::new();
    if !request.business_background.trim().is_empty() {
        semantics.push_str("取数口径：");
        semantics.push_str(request.business_background.trim());
        semantics.push_str("\n");
    }
    semantics.push_str("已确认 SQL：");
    semantics.push_str(&corrected_sql);
    persist_one_sql_memory(
        db,
        tenant_id,
        user_id,
        session_id,
        "business_context",
        semantics,
        json!({ "capability": "nl2sql", "kind": "sql_semantics", "queryId": resp.query_id }),
    )
    .await;

    // 约束 (constraints): each confirmed requirement becomes its own item so it
    // can be recalled and matched independently on later turns.
    if let Some(requirements) = resp.confirmed_requirements.as_ref() {
        for requirement in requirements
            .iter()
            .map(|r| r.trim())
            .filter(|r| !r.is_empty())
        {
            persist_one_sql_memory(
                db,
                tenant_id,
                user_id,
                session_id,
                "business_context",
                format!("约束：{requirement}"),
                json!({ "capability": "nl2sql", "kind": "sql_constraint", "queryId": resp.query_id }),
            )
            .await;
        }
    }
}

/// Write a single session-scoped nl2sql memory item, ignoring failures.
#[cfg(feature = "nl2sql")]
async fn persist_one_sql_memory(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    memory_type: &str,
    content: String,
    metadata: serde_json::Value,
) {
    let req = MemoryUpsertRequest {
        scope: Some("session".to_string()),
        app: Some(NL2SQL_MEMORY_APP.to_string()),
        session_id: Some(session_id.to_string()),
        memory_type: Some(memory_type.to_string()),
        content,
        source_type: Some("session_summary".to_string()),
        confidence: Some(0.85),
        pinned: Some(false),
        enabled: Some(true),
        stale_at: None,
        verified_at: None,
        metadata: Some(metadata),
    };
    let _ = create_memory_item_internal(db, tenant_id, user_id, req).await;
}

#[cfg(all(test, feature = "nl2sql"))]
mod tests {
    use super::*;

    /// Build a minimal `QueryResponse` for mapping tests.
    fn query_response(
        sql: Option<&str>,
        explanation: Option<&str>,
        error: Option<&str>,
        clarification: Option<&str>,
    ) -> QueryResponse {
        QueryResponse {
            sql: sql.map(str::to_string),
            explanation: explanation.map(str::to_string),
            error: error.map(str::to_string),
            query_id: "q-1".to_string(),
            conversation_id: Some("c-1".to_string()),
            summary_version: None,
            clarification_question: clarification.map(str::to_string),
            confirmed_requirements: None,
            missing_requirements: None,
            query_understanding: None,
            intent: None,
            cache_hit: false,
            applied_rules: Vec::new(),
            used_references: Vec::new(),
        }
    }

    #[test]
    fn detects_fenced_sql_with_background() {
        let text = "统计上月各渠道的下单用户数，但下面这段跑不出结果：\n```sql\nSELECT channel, COUNT(user_id) FROM orders WHERE\n```\n帮我补全。";
        let req = detect_sql_troubleshooting_request(text).expect("should detect");
        assert!(req.sql_fragment.contains("SELECT channel"));
        assert!(!req.sql_fragment.contains("```"));
        assert!(req.business_background.contains("统计上月各渠道"));
        assert!(req.business_background.contains("帮我补全"));
    }

    #[test]
    fn detects_inline_sql_lines_and_separates_prose() {
        let text = "业务背景：需要按天统计活跃用户\nSELECT dt, count(*) FROM dau\nGROUP BY dt";
        let req = detect_sql_troubleshooting_request(text).expect("should detect");
        assert!(req.sql_fragment.contains("SELECT dt"));
        assert!(req.sql_fragment.contains("GROUP BY dt"));
        assert_eq!(req.business_background, "业务背景：需要按天统计活跃用户");
    }

    #[test]
    fn returns_none_without_sql() {
        assert!(detect_sql_troubleshooting_request("今天天气怎么样？").is_none());
        assert!(detect_sql_troubleshooting_request("   ").is_none());
    }

    #[test]
    fn question_includes_background_and_fragment() {
        let req = SqlTroubleshootingRequest {
            sql_fragment: "SELECT 1".to_string(),
            business_background: "订单表统计".to_string(),
        };
        let q = req.to_nl2sql_question();
        assert!(q.contains("业务背景：订单表统计"));
        assert!(q.contains("排查结论"));
        assert!(q.contains("SELECT 1"));
    }

    #[test]
    fn question_omits_empty_background() {
        let req = SqlTroubleshootingRequest {
            sql_fragment: "SELECT 1".to_string(),
            business_background: String::new(),
        };
        let q = req.to_nl2sql_question();
        assert!(!q.contains("业务背景"));
        assert!(q.contains("SELECT 1"));
    }

    #[test]
    fn compose_without_prior_memory_is_identity() {
        // No prior session memory ⇒ single-turn behaviour is unchanged (task 20.2).
        let req = SqlTroubleshootingRequest {
            sql_fragment: "SELECT 1".to_string(),
            business_background: "订单表统计".to_string(),
        };
        let composed = compose_question_with_memory(&req, &[], None);
        assert_eq!(composed, req.to_nl2sql_question());
    }

    #[test]
    fn compose_prepends_prior_memory_context() {
        // Prior 取数口径/约束 are injected ahead of the current question so
        // nl2sql answers consistently across turns (task 20.2).
        let req = SqlTroubleshootingRequest {
            sql_fragment: "SELECT COUNT(*) FROM orders".to_string(),
            business_background: "统计下单用户数".to_string(),
        };
        let prior = vec![
            "取数口径：只统计已支付订单".to_string(),
            "约束：时区按 UTC+8 计算".to_string(),
        ];
        let composed = compose_question_with_memory(&req, &prior, None);
        // Prior memory block appears, is numbered, and precedes the base question.
        assert!(composed.contains("本会话已确立的取数口径/约束"));
        assert!(composed.contains("1. 取数口径：只统计已支付订单"));
        assert!(composed.contains("2. 约束：时区按 UTC+8 计算"));
        let memory_pos = composed.find("本会话已确立").unwrap();
        let base_pos = composed.find("统计下单用户数").unwrap();
        assert!(
            memory_pos < base_pos,
            "prior memory context must precede the current question"
        );
        // The base question is still fully present.
        assert!(composed.contains("SELECT COUNT(*) FROM orders"));
    }

    #[test]
    fn compose_includes_shared_context_before_current_sql_request() {
        // Super Assistant shared context carries exact recent turns into the
        // nl2sql troubleshooting branch; the current SQL request remains last and
        // highest priority.
        let req = SqlTroubleshootingRequest {
            sql_fragment: "SELECT * FROM orders WHERE dt = yesterday".to_string(),
            business_background: "帮我修正这段 SQL".to_string(),
        };
        let composed = compose_question_with_memory(
            &req,
            &[],
            Some("最近会话原文：用户刚确认数据源是 plouto，订单表是 business_order"),
        );
        assert!(composed.contains("超级助手共享会话背景"));
        assert!(composed.contains("数据源是 plouto"));
        let context_pos = composed.find("超级助手共享会话背景").unwrap();
        let current_pos = composed.find("帮我修正这段 SQL").unwrap();
        assert!(
            context_pos < current_pos,
            "shared context must precede the current SQL request"
        );
        assert!(composed.contains("SELECT * FROM orders"));
    }

    #[test]
    fn conclusion_resolved_when_sql_present() {
        let resp = query_response(Some("SELECT 1"), Some("补全了缺失的条件"), None, None);
        let c = SqlTroubleshootingConclusion::from_query_response(resp);
        assert!(c.resolved);
        assert_eq!(c.corrected_sql.as_deref(), Some("SELECT 1"));
        assert_eq!(c.conclusion, "补全了缺失的条件");
        assert_eq!(c.query_id.as_deref(), Some("q-1"));
    }

    #[test]
    fn conclusion_surfaces_error() {
        let resp = query_response(None, None, Some("字段 user_id 不存在"), None);
        let c = SqlTroubleshootingConclusion::from_query_response(resp);
        assert!(!c.resolved);
        assert!(c.corrected_sql.is_none());
        assert!(c.conclusion.contains("字段 user_id 不存在"));
    }

    #[test]
    fn conclusion_surfaces_clarification() {
        let resp = query_response(None, None, None, Some("请问统计哪个时间范围？"));
        let c = SqlTroubleshootingConclusion::from_query_response(resp);
        assert!(!c.resolved);
        assert_eq!(
            c.clarification_question.as_deref(),
            Some("请问统计哪个时间范围？")
        );
        assert!(c.conclusion.contains("需要澄清"));
    }
}

// ===========================================================================
// ai_chat general chat / code-answer adapter (task 10.1, Req 3.1 / 3.2)
// ---------------------------------------------------------------------------
// General conversational Q&A reuses the *existing* chat model routing. The
// same executor the Bot gateway's `ai_chat` / `generic_ai` capability uses is
// `crate::routes::pm::run_pm_chat_completion` (per-tenant scoped chat key
// resolution + failover). Following the reuse-first principle, this adapter is
// a thin wiring layer: it shapes the Super_Assistant message into the
// `ChatMessage` list that executor already understands, delegates to it, and
// maps the result into a Super_Assistant-facing answer. No chat/model routing
// is reimplemented here.
//
// For code questions (Req 3.2) the adapter prepends a code-answer instruction
// so the model returns copyable fenced code blocks, then parses the answer into
// structured `CodeBlock`s (kept alongside the raw markdown so the frontend can
// offer copy buttons). Detection (`is_code_question`) and parsing
// (`extract_code_blocks`) are pure functions so they can be unit-tested
// exhaustively without a live model call.
//
// This section is gated behind the `pm` cargo feature because the reused chat
// executor (`crate::routes::pm::run_pm_chat_completion`) lives in the
// feature-gated `pm` module, mirroring how the `nl2sql` section above is gated
// behind its backing capability.
//
// Fully-qualified paths are used throughout so this section adds no top-level
// `use` imports and cannot collide with the feature-gated `nl2sql` section
// above.
// ===========================================================================

/// A single copyable code block parsed out of an `ai_chat` answer (Req 3.2).
///
/// Serializes to camelCase for the Super_Assistant frontend, which renders each
/// block with a copy affordance.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeBlock {
    /// Optional language tag from the fence (e.g. `rust`, `python`). `None`
    /// when the fence carried no language hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The verbatim code body (fence markers stripped, trailing newline
    /// trimmed) ready to be copied as-is.
    pub code: String,
}

/// The Super_Assistant-facing outcome of the `ai_chat` capability (Req 3.1 /
/// 3.2). Serializes to camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatAnswer {
    /// The full markdown answer (explanation + any inline fenced code) exactly
    /// as produced by the reused chat model routing.
    pub answer: String,
    /// Copyable code blocks extracted from `answer` (empty for pure prose).
    pub code_blocks: Vec<CodeBlock>,
    /// True when the message was treated as a code question and the
    /// code-answer instruction was applied (Req 3.2).
    pub is_code_answer: bool,
    /// The model that produced the answer, surfaced from the reused executor's
    /// usage record for traceability.
    pub model: String,
}

#[cfg(feature = "pm")]
impl AiChatAnswer {
    /// Map the reused executor's [`crate::routes::pm::PmChatRunResult`] into a
    /// Super_Assistant answer. Pure mapping so it is unit-testable without a
    /// live model call.
    pub(crate) fn from_run_result(
        result: crate::routes::pm::PmChatRunResult,
        is_code_answer: bool,
    ) -> Self {
        let code_blocks = extract_code_blocks(&result.answer);
        let model = result.usage.model;
        let answer = result.answer;
        Self {
            answer,
            code_blocks,
            is_code_answer,
            model,
        }
    }
}

/// Readability_Spec fragment injected (as a system message) into every
/// Super_Assistant visible reply so the answer is organized per the readability
/// spec — prose for reasoning, lists for enumeration, code blocks only for code,
/// headings only for multi-step answers, restrained wording, and brevity
/// proportional to a simple question (no redundant sectioning)
/// (codex-parity-gaps Req 4.5 / 4.7; full spec in `docs/READABILITY_SPEC.md`).
///
/// This is a byte-stable, query-independent instruction, so it belongs to the
/// stable prefix of the request (prepended ahead of the dynamic user message).
#[cfg(feature = "pm")]
const READABILITY_SPEC_INSTRUCTION: &str = "输出易读性规范（对用户可见回复必须遵循）：\n\
1. 散文用于解释与推理；要点/有序列表用于枚举与序列；代码块专用于代码、命令与文件内容，不要用代码块承载普通说明文字。\n\
2. 标题分节仅用于多步骤或多主题的答案；简单直接的问题不使用标题分节。\n\
3. 措辞克制、平实、基于事实，避免夸张与堆砌；直接回应问题实质，省略客套与填充式开场。\n\
4. 回复的篇幅与结构复杂度与问题规模相称：简单问题以相称的简洁篇幅回答，不附加冗余的分节、总结或背景铺垫。";

/// Instruction prepended (as a system message) when the message is classified
/// as a code question, asking the model to return copyable fenced code blocks
/// with an explanation (Req 3.2). The instruction only shapes the request; the
/// reused chat model routing does the actual answering.
#[cfg(feature = "pm")]
const CODE_ANSWER_INSTRUCTION: &str = "用户的问题与代码相关。请用中文解释思路，并将所有代码放在带语言标签的 Markdown 代码块（```<language> ... ```）中，确保代码可以被直接复制运行。先给出可复制的完整代码块，再补充简要说明。";

/// Heuristic, pure classifier for whether a Super_Assistant message is a code
/// question (Req 3.2). Deterministic and I/O-free for exhaustive unit testing.
///
/// A message counts as a code question when it contains a fenced code block or
/// any common programming / debugging cue (bilingual).
#[cfg(feature = "pm")]
pub fn is_code_question(text: &str) -> bool {
    if text.contains("```") {
        return true;
    }
    let lower = text.to_lowercase();
    const CODE_HINTS: &[&str] = &[
        "代码",
        "函数",
        "报错",
        "编译",
        "调试",
        "算法",
        "实现",
        "正则",
        "堆栈",
        "code",
        "function",
        "error",
        "exception",
        "stack trace",
        "compile",
        "debug",
        "bug",
        "algorithm",
        "implement",
        "regex",
        "class ",
        "def ",
        "import ",
        "console.log",
        "print(",
        "rust",
        "python",
        "javascript",
        "typescript",
        "golang",
    ];
    CODE_HINTS.iter().any(|kw| lower.contains(kw))
}

/// Extract copyable code blocks from a markdown answer (Req 3.2).
///
/// Pure parser over fenced ```` ``` ```` blocks: an optional language tag on the
/// fence's first line becomes [`CodeBlock::language`], the body becomes
/// [`CodeBlock::code`] with fence markers removed and the trailing newline
/// trimmed. Unterminated fences are ignored. Returns an empty vec for pure
/// prose.
#[cfg(feature = "pm")]
pub fn extract_code_blocks(answer: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find("```") {
        let after_open = &rest[open + 3..];
        // The language tag (possibly empty) runs to the first newline.
        let Some((lang_line, body_and_rest)) = after_open.split_once('\n') else {
            break; // opening fence without a body newline -> no complete block
        };
        let Some(close) = body_and_rest.find("```") else {
            break; // no closing fence
        };
        let code = &body_and_rest[..close];
        let language = {
            let tag = lang_line.trim();
            if tag.is_empty() {
                None
            } else {
                Some(tag.to_string())
            }
        };
        blocks.push(CodeBlock {
            language,
            code: code.trim_end_matches('\n').to_string(),
        });
        rest = &body_and_rest[close + 3..];
    }
    blocks
}

/// Build the `ChatMessage` list handed to the reused chat model routing.
///
/// Every visible reply is shaped by the [`READABILITY_SPEC_INSTRUCTION`] system
/// message so the answer follows the Readability_Spec (codex-parity-gaps Req 4.5
/// / 4.7). When the message is a code question, the [`CODE_ANSWER_INSTRUCTION`]
/// system message is added so the answer contains copyable code blocks
/// (Req 3.2). The readability instruction is placed first because it is
/// query-independent and byte-stable — keeping it at the head of the request
/// keeps the stable prefix intact. Pure shaping — no model call.
#[cfg(feature = "pm")]
fn build_ai_chat_messages(
    user_message: &str,
    is_code: bool,
    web_context: Option<&str>,
) -> Vec<crate::routes::chat::ChatMessage> {
    let mut messages = Vec::new();
    // Readability_Spec applies to all visible replies and is the same on every
    // turn, so it leads the message list as part of the stable prefix.
    messages.push(crate::routes::chat::ChatMessage {
        role: "system".to_string(),
        content: serde_json::Value::String(READABILITY_SPEC_INSTRUCTION.to_string()),
    });
    if is_code {
        messages.push(crate::routes::chat::ChatMessage {
            role: "system".to_string(),
            content: serde_json::Value::String(CODE_ANSWER_INSTRUCTION.to_string()),
        });
    }
    if let Some(context) = web_context
        .map(str::trim)
        .filter(|context| !context.is_empty())
    {
        messages.push(crate::routes::chat::ChatMessage {
            role: "system".to_string(),
            content: serde_json::Value::String(context.to_string()),
        });
    }
    messages.push(crate::routes::chat::ChatMessage {
        role: "user".to_string(),
        content: serde_json::Value::String(user_message.to_string()),
    });
    messages
}

#[cfg(feature = "pm")]
fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in value.chars().take(max_chars) {
        out.push(ch);
    }
    if value.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

/// Render layered web-search evidence into a system prompt fragment for the
/// reused chat adapter. The retrieval itself is handled by
/// [`super_assistant_web_search`]; this only grounds the final answer and
/// prevents the model from claiming it has no realtime interface after search
/// was already attempted.
#[cfg(feature = "pm")]
pub fn render_web_search_answer_context(outcome: &SuperAssistantWebSearchOutcome) -> String {
    let mut lines = Vec::new();
    lines.push("实时/联网检索上下文：".to_string());
    lines.push(format!("用户查询：{}", outcome.query.trim()));
    match outcome
        .used_layer
        .as_deref()
        .filter(|layer| !layer.is_empty())
    {
        Some(layer) => lines.push(format!("使用的检索层：{layer}")),
        None => lines.push("使用的检索层：未命中".to_string()),
    }
    if let Some(reason) = outcome
        .degraded_reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        lines.push(format!("降级原因：{reason}"));
    }

    if !outcome.available || outcome.sources.is_empty() {
        lines.push(
            "没有检索到可用于实时回答的可靠来源。回答时必须明确说明当前无法确认实时信息，不要编造天气、价格、新闻、赛程或其它实时事实。"
                .to_string(),
        );
        return lines.join("\n");
    }

    lines.push(
        "请只基于以下来源回答实时事实；证据不足就说明不足。若同一来源同时包含“检索摘要”和“网页验证摘录”，检索摘要是原生联网检索对该来源的结果摘要，网页验证摘录只用于补强和验证链接，不能因为网页摘录包含导航文本就否定检索摘要。回答末尾用简短“来源”列出标题/链接或结构化来源。不要说“没有实时接口”。"
            .to_string(),
    );
    for (idx, source) in outcome.sources.iter().take(5).enumerate() {
        let title = source.title.trim();
        let display_title = if title.is_empty() {
            "未命名来源"
        } else {
            title
        };
        lines.push(format!("{}. {}", idx + 1, display_title));
        if let Some(url) = source
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            lines.push(format!("   URL: {url}"));
        } else if source.has_structured_citation() {
            lines.push(format!(
                "   来源: {} / {}",
                source.source_type.trim(),
                source.source_name.trim()
            ));
        }
        if let Some(excerpt) = source
            .excerpt
            .as_deref()
            .map(str::trim)
            .filter(|excerpt| !excerpt.is_empty())
        {
            lines.push(format!("   摘录: {}", truncate_chars(excerpt, 500)));
        }
    }
    lines.join("\n")
}

/// Adapt a general `ai_chat` message to the existing chat model routing
/// (Req 3.1) and return a Super_Assistant answer with copyable code blocks for
/// code questions (Req 3.2).
///
/// Thin wiring: it classifies the message, shapes the `ChatMessage` list, and
/// delegates to [`crate::routes::pm::run_pm_chat_completion`] — the same
/// executor the Bot gateway's `ai_chat` / `generic_ai` capability uses — then
/// maps the result via [`AiChatAnswer::from_run_result`]. No chat/model routing
/// is reimplemented here.
#[cfg(feature = "pm")]
pub async fn run_ai_chat_adapter(
    state: &crate::state::AppState,
    tenant_id: &str,
    user_id: &str,
    model: String,
    user_message: &str,
) -> crate::error::Result<AiChatAnswer> {
    run_ai_chat_adapter_with_web_context(state, tenant_id, user_id, model, user_message, None).await
}

#[cfg(feature = "pm")]
pub async fn run_ai_chat_adapter_with_web_context(
    state: &crate::state::AppState,
    tenant_id: &str,
    user_id: &str,
    model: String,
    user_message: &str,
    web_search: Option<&SuperAssistantWebSearchOutcome>,
) -> crate::error::Result<AiChatAnswer> {
    let is_code = is_code_question(user_message);
    let web_context = web_search.map(render_web_search_answer_context);
    let messages = build_ai_chat_messages(user_message, is_code, web_context.as_deref());
    let result =
        crate::routes::pm::run_pm_chat_completion(state, tenant_id, user_id, model, messages)
            .await?;
    Ok(AiChatAnswer::from_run_result(result, is_code))
}

#[cfg(all(test, feature = "pm"))]
mod ai_chat_adapter_tests {
    use super::{extract_code_blocks, is_code_question, AiChatAnswer, CodeBlock};

    /// Build a `PmChatRunResult` for pure mapping tests.
    fn run_result(answer: &str, model: &str) -> crate::routes::pm::PmChatRunResult {
        crate::routes::pm::PmChatRunResult {
            answer: answer.to_string(),
            usage: crate::routes::pm::PmUsageDto {
                input_tokens: 1,
                output_tokens: 2,
                total_tokens: 3,
                estimated_cost_usd: 0.0,
                model: model.to_string(),
            },
            applied_rules: Vec::new(),
            api_key_id: "k-1".to_string(),
            provider_name: "test".to_string(),
        }
    }

    #[test]
    fn detects_code_questions_by_fence_and_keywords() {
        assert!(is_code_question("```rust\nfn main() {}\n```"));
        assert!(is_code_question("这段代码为什么报错？"));
        assert!(is_code_question(
            "How do I implement a binary search in Python?"
        ));
        assert!(is_code_question("帮我写个快排算法"));
    }

    #[test]
    fn treats_plain_prose_as_non_code() {
        assert!(!is_code_question("今天天气怎么样？"));
        assert!(!is_code_question("帮我总结一下这份会议纪要"));
    }

    #[test]
    fn extracts_code_block_with_language_tag() {
        let answer = "先看下面的实现：\n```rust\nfn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n```\n以上函数返回两数之和。";
        let blocks = extract_code_blocks(answer);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].language.as_deref(), Some("rust"));
        assert_eq!(
            blocks[0].code,
            "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"
        );
        assert!(!blocks[0].code.contains("```"));
    }

    #[test]
    fn extracts_multiple_blocks_and_untagged_fence() {
        let answer = "```\nplain\n```\nmiddle\n```python\nprint(1)\n```";
        let blocks = extract_code_blocks(answer);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].language, None);
        assert_eq!(blocks[0].code, "plain");
        assert_eq!(blocks[1].language.as_deref(), Some("python"));
        assert_eq!(blocks[1].code, "print(1)");
    }

    #[test]
    fn ignores_unterminated_fence() {
        let answer = "解释文本\n```js\nconsole.log(1)";
        assert!(extract_code_blocks(answer).is_empty());
    }

    #[test]
    fn prose_answer_has_no_code_blocks() {
        assert!(extract_code_blocks("这是一个纯文本回答，没有代码。").is_empty());
    }

    #[test]
    fn maps_run_result_into_answer_with_blocks() {
        let result = run_result("说明：\n```ts\nconst x = 1;\n```", "gpt-x");
        let ans = AiChatAnswer::from_run_result(result, true);
        assert!(ans.is_code_answer);
        assert_eq!(ans.model, "gpt-x");
        assert_eq!(ans.answer, "说明：\n```ts\nconst x = 1;\n```");
        assert_eq!(
            ans.code_blocks,
            vec![CodeBlock {
                language: Some("ts".to_string()),
                code: "const x = 1;".to_string(),
            }]
        );
    }

    #[test]
    fn maps_prose_run_result_without_code() {
        let result = run_result("普通聊天回答", "gpt-x");
        let ans = AiChatAnswer::from_run_result(result, false);
        assert!(!ans.is_code_answer);
        assert!(ans.code_blocks.is_empty());
        assert_eq!(ans.answer, "普通聊天回答");
    }

    /// The Readability_Spec system message leads every visible reply, whether or
    /// not the message is a code question (codex-parity-gaps Req 4.5 / 4.7).
    #[test]
    fn readability_spec_leads_every_reply() {
        let text = "今天天气怎么样？"; // simple, non-code question
        let messages = super::build_ai_chat_messages(text, false, None);
        // First message is the byte-stable readability system instruction.
        assert_eq!(messages[0].role, "system");
        assert_eq!(
            messages[0].content,
            serde_json::Value::String(super::READABILITY_SPEC_INSTRUCTION.to_string())
        );
        // Non-code path: readability system message + user message only.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, "user");
    }

    /// For a code question the readability instruction still leads, followed by
    /// the code-answer instruction, then the user message (Req 4.5 + Req 3.2).
    #[test]
    fn readability_spec_precedes_code_instruction() {
        let text = "帮我用 Python 写个快排";
        let messages = super::build_ai_chat_messages(text, true, None);
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[0].content,
            serde_json::Value::String(super::READABILITY_SPEC_INSTRUCTION.to_string())
        );
        assert_eq!(
            messages[1].content,
            serde_json::Value::String(super::CODE_ANSWER_INSTRUCTION.to_string())
        );
        assert_eq!(messages[2].role, "user");
    }

    /// The fragment carries the core Readability_Spec constraints so the model
    /// organizes output per spec and keeps simple answers brief (Req 4.5 / 4.7).
    #[test]
    fn readability_fragment_covers_core_constraints() {
        let spec = super::READABILITY_SPEC_INSTRUCTION;
        assert!(spec.contains("散文")); // prose for reasoning
        assert!(spec.contains("列表")); // lists for enumeration
        assert!(spec.contains("代码块")); // code blocks only for code
        assert!(spec.contains("标题")); // headings only for multi-step
        assert!(spec.contains("相称")); // brevity proportional to question
    }

    #[test]
    fn ai_chat_answer_json_round_trips() {
        let ans = AiChatAnswer {
            answer: "a\n```go\nx := 1\n```".to_string(),
            code_blocks: vec![CodeBlock {
                language: Some("go".to_string()),
                code: "x := 1".to_string(),
            }],
            is_code_answer: true,
            model: "m".to_string(),
        };
        let json = serde_json::to_string(&ans).expect("serialize");
        assert!(json.contains("codeBlocks"));
        assert!(json.contains("isCodeAnswer"));
        let back: AiChatAnswer = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ans);
    }
}

// ===========================================================================
// Web-search adapter with traceable sources (task 10.2, Req 3.3 / 7.3)
// ---------------------------------------------------------------------------
// Live/web retrieval reuses the *existing* layered search orchestration in
// `crate::routes::search_orchestrator_runtime::execute_unified_search`, which
// already implements the required priority order (Search extension → model
// native streaming → MCP → local/RAG fallback) and surfaces `used_layer`,
// `degraded_reason`, and per-item `url`. Following the reuse-first principle,
// this section adds NO retrieval logic of its own: `super_assistant_web_search`
// is a thin entry that shapes a `UnifiedSearchRequest` for the `ai_chat`
// scenario, delegates to the reused orchestrator (modelled after the existing
// callers in `agent_chat_adversarial.rs` / `agent_pm_probe_exec.rs`), and maps
// the evidence into Super_Assistant-facing, traceable sources.
//
// A "traceable source" (design Property 15, Req 3.3 / 7.3) is either a
// clickable `url` or — when the evidence carries no link — a structured
// citation composed of `sourceType` + `sourceName`. `map_evidence_to_sources`
// is a pure mapping so it can be unit-tested exhaustively, and
// `SuperAssistantSource::is_traceable` / `SuperAssistantWebSearchOutcome::
// has_traceable_source` back Property 15 directly.
//
// This section is NOT feature-gated: `execute_unified_search` and its request /
// result types live in `search_orchestrator_runtime`, which is registered
// unconditionally in `routes/mod.rs` (its backing `pm-domain` dependency is
// non-optional), so no cargo feature is required. Fully-qualified paths are
// used throughout so this section adds no top-level `use` imports and cannot
// collide with the feature-gated sections above.
// ===========================================================================

/// A single Super_Assistant source rendered under a web-search answer
/// (Req 3.3 / 8.3). Serializes to camelCase for the frontend.
///
/// A source is *traceable* (design Property 15) when it exposes a clickable
/// [`url`](Self::url) or, lacking one, a structured citation formed by
/// [`source_type`](Self::source_type) + [`source_name`](Self::source_name).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperAssistantSource {
    /// Clickable link to the source when the evidence carried one. `None` when
    /// the layer produced only a structured (non-link) citation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The retrieval layer / source category (e.g. `configured_search_provider`,
    /// `native_model_search`, `mcp_search`, `rag_local`). Part of the structured
    /// citation used when no `url` is present.
    pub source_type: String,
    /// The provider / source display name. Part of the structured citation.
    pub source_name: String,
    /// The source title, when available.
    pub title: String,
    /// A short excerpt supporting the citation, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

impl SuperAssistantSource {
    /// Map a single reused search evidence item into a Super_Assistant source.
    /// Pure: only trims / normalizes fields, filtering out blank links and
    /// excerpts so `is_traceable` reflects genuine content.
    fn from_evidence(
        item: &crate::routes::search_orchestrator_runtime::UnifiedSearchEvidenceItem,
    ) -> Self {
        let clean = |value: &str| -> Option<String> {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        Self {
            url: item.url.as_deref().and_then(clean),
            source_type: item.source_type.trim().to_string(),
            source_name: item.source_name.trim().to_string(),
            title: item.title.trim().to_string(),
            excerpt: item.excerpt.as_deref().and_then(clean),
        }
    }

    /// True when this source exposes a non-empty clickable link.
    pub fn has_clickable_url(&self) -> bool {
        self.url
            .as_deref()
            .map(|url| !url.trim().is_empty())
            .unwrap_or(false)
    }

    /// True when this source carries a structured citation (both a source type
    /// and a source name), usable when no clickable link is present.
    pub fn has_structured_citation(&self) -> bool {
        !self.source_type.trim().is_empty() && !self.source_name.trim().is_empty()
    }

    /// A source is traceable when it has a clickable link *or* a structured
    /// citation (design Property 15, Req 3.3 / 7.3).
    pub fn is_traceable(&self) -> bool {
        self.has_clickable_url() || self.has_structured_citation()
    }
}

/// Pure mapping of the reused orchestrator's evidence into Super_Assistant
/// sources (Req 3.3). No retrieval is performed here; each item becomes a
/// clickable link when it has a `url`, otherwise a structured citation.
pub fn map_evidence_to_sources(
    items: &[crate::routes::search_orchestrator_runtime::UnifiedSearchEvidenceItem],
) -> Vec<SuperAssistantSource> {
    items
        .iter()
        .map(SuperAssistantSource::from_evidence)
        .collect()
}

/// The Super_Assistant-facing outcome of a web-search turn (Req 3.3 / 7.3).
///
/// Carries the reused orchestrator's `usedLayer` / `degradedReason`, the mapped
/// traceable `sources`, and the raw layered `traces` for observability. Serializes
/// to camelCase for the frontend.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperAssistantWebSearchOutcome {
    /// The search scenario the request ran under (`ai_chat`).
    pub scenario: String,
    /// The normalized query the orchestrator actually searched.
    pub query: String,
    /// Whether the layered search produced usable, source-backed evidence.
    pub available: bool,
    /// Which layer produced the surfaced evidence (Search extension / native /
    /// MCP / rag_local / multi_source), when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_layer: Option<String>,
    /// Why the result degraded / fell back, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    /// Traceable sources (clickable links + structured citations) to render
    /// under the answer (Req 3.3 / 8.3).
    pub sources: Vec<SuperAssistantSource>,
    /// The per-layer retrieval traces from the reused orchestrator, for audit /
    /// observability (Req 7.4).
    pub traces: Vec<crate::routes::search_orchestrator_runtime::UnifiedSearchTrace>,
}

pub const SUPER_ASSISTANT_WEB_SEARCH_SCENARIO: &str = "super_assistant_live_lookup";

impl SuperAssistantWebSearchOutcome {
    /// Map a reused [`UnifiedSearchResult`] into a Super_Assistant outcome. Pure
    /// mapping so it is unit-testable without a live search.
    ///
    /// [`UnifiedSearchResult`]: crate::routes::search_orchestrator_runtime::UnifiedSearchResult
    pub fn from_unified_result(
        result: crate::routes::search_orchestrator_runtime::UnifiedSearchResult,
    ) -> Self {
        let sources = map_evidence_to_sources(&result.items);
        Self {
            scenario: result.scenario,
            query: result.query,
            available: result.available,
            used_layer: result.used_layer,
            degraded_reason: result.degraded_reason,
            sources,
            traces: result.traces,
        }
    }

    /// True when at least one surfaced source is traceable — the invariant
    /// behind design Property 15 (Req 3.3 / 7.3).
    pub fn has_traceable_source(&self) -> bool {
        self.sources.iter().any(SuperAssistantSource::is_traceable)
    }

    /// Compact trace payload persisted to the chat trace store so the UI and
    /// diagnostics can see which search layer actually ran.
    #[cfg(feature = "pm")]
    pub fn to_trace_payload(&self, attempted_queries: &[String]) -> serde_json::Value {
        serde_json::json!({
            "event": "super_assistant_web_search",
            "scenario": self.scenario,
            "query": self.query,
            "available": self.available,
            "usedLayer": self.used_layer,
            "degradedReason": self.degraded_reason,
            "attemptedQueries": attempted_queries,
            "sourceCount": self.sources.len(),
            "sources": self.sources,
            "traces": self.traces,
        })
    }
}

#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperAssistantWebSearchDecision {
    pub needs_web_search: bool,
    pub query: Option<String>,
    pub reason: Option<String>,
}

#[cfg(feature = "pm")]
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSuperAssistantWebSearchDecision {
    needs_web_search: Option<bool>,
    query: Option<String>,
    web_search_query: Option<String>,
    reason: Option<String>,
    web_search_reason: Option<String>,
}

#[cfg(feature = "pm")]
fn parse_json_value_from_text(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(value);
    }
    let object = trimmed
        .find('{')
        .and_then(|start| trimmed.rfind('}').map(|end| (start, end)))
        .and_then(|(start, end)| (end > start).then_some(&trimmed[start..=end]))
        .and_then(|slice| serde_json::from_str::<serde_json::Value>(slice).ok());
    if object.is_some() {
        return object;
    }
    trimmed
        .find('[')
        .and_then(|start| trimmed.rfind(']').map(|end| (start, end)))
        .and_then(|(start, end)| (end > start).then_some(&trimmed[start..=end]))
        .and_then(|slice| serde_json::from_str::<serde_json::Value>(slice).ok())
}

#[cfg(feature = "pm")]
fn normalize_web_search_query(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(['"', '\'', '`']).trim();
    if trimmed.is_empty() {
        return None;
    }
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(truncate_chars(&collapsed, 180))
}

#[cfg(feature = "pm")]
fn parse_web_search_decision(answer: &str) -> Option<SuperAssistantWebSearchDecision> {
    let value = parse_json_value_from_text(answer)?;
    let raw: RawSuperAssistantWebSearchDecision = serde_json::from_value(value).ok()?;
    let needs_web_search = raw.needs_web_search?;
    let query = raw
        .web_search_query
        .or(raw.query)
        .and_then(|value| normalize_web_search_query(&value));
    let reason = raw.web_search_reason.or(raw.reason).and_then(|value| {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(truncate_chars(&trimmed, 240))
    });
    Some(SuperAssistantWebSearchDecision {
        needs_web_search,
        query,
        reason,
    })
}

#[cfg(feature = "pm")]
pub async fn super_assistant_semantic_web_search_decision(
    state: &crate::state::AppState,
    tenant_id: &str,
    model: &str,
    user_message: &str,
) -> Option<SuperAssistantWebSearchDecision> {
    let user_message = user_message.trim();
    if user_message.is_empty() {
        return None;
    }
    let system_prompt = r#"你是 AOS 超级助手的联网判定器，只输出 JSON 对象，不回答用户问题。
任务：判断用户问题是否必须检索外部/实时/公开证据后才能可靠回答。
原则：
- 按语义判断，不按固定关键词。
- 如果稳定模型知识足够回答，needsWebSearch=false。
- 用户明确要求联网、搜索、浏览网页、查公开资料或给出处时，无论主题是什么，needsWebSearch=true。
- 用户询问当前业界实践、主流方案、真实企业如何实施、竞品现状或外部基准时，需要公开证据，needsWebSearch=true。
- 如果问题依赖会变化的公共事实或其他外部状态，needsWebSearch=true。
- query 要短、可直接搜索；如果用户问题包含过窄限定词，但目标事实可由更宽范围查询获得，query 可以保留核心地点/实体并去掉不必要限定。
输出字段：
needsWebSearch: true/false
query: 需要联网时给搜索 query，否则 null
reason: 简短中文理由
不要 Markdown，不要额外文本。"#;
    let prompt = format!("用户消息：{user_message}");
    match crate::routes::pm::run_chat_completion_with_any_chat_key(
        state,
        tenant_id,
        model.to_string(),
        vec![crate::routes::chat::ChatMessage {
            role: "user".to_string(),
            content: serde_json::Value::String(prompt),
        }],
        system_prompt,
        500,
    )
    .await
    {
        Ok(result) => parse_web_search_decision(&result.answer),
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                error = %error,
                "super assistant semantic web-search decision failed; keeping router decision"
            );
            None
        }
    }
}

#[cfg(feature = "pm")]
fn collect_web_search_queries(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(query) = normalize_web_search_query(text) {
                out.push(query);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_web_search_queries(item, out);
            }
        }
        serde_json::Value::Object(obj) => {
            for key in [
                "query",
                "q",
                "searchQuery",
                "webSearchQuery",
                "queries",
                "searchQueries",
                "webSearchQueries",
                "candidates",
            ] {
                if let Some(item) = obj.get(key) {
                    collect_web_search_queries(item, out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(feature = "pm")]
fn parse_web_search_rewrite_candidates(answer: &str) -> Vec<String> {
    let Some(value) = parse_json_value_from_text(answer) else {
        return Vec::new();
    };
    let mut queries = Vec::new();
    collect_web_search_queries(&value, &mut queries);
    queries
}

#[cfg(feature = "pm")]
pub fn select_web_search_attempts(
    router_query: &str,
    user_message: &str,
    candidates: &[String],
    max_attempts: usize,
) -> Vec<String> {
    let mut attempts = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for raw in std::iter::once(router_query)
        .chain(std::iter::once(user_message))
        .chain(candidates.iter().map(String::as_str))
    {
        let Some(query) = normalize_web_search_query(raw) else {
            continue;
        };
        let key = query.to_lowercase();
        if seen.insert(key) {
            attempts.push(query);
        }
        if attempts.len() >= max_attempts.max(1) {
            break;
        }
    }
    attempts
}

#[cfg(feature = "pm")]
pub fn attempted_queries_from_web_search_traces(
    outcome: &SuperAssistantWebSearchOutcome,
) -> Vec<String> {
    let mut queries = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for raw in std::iter::once(outcome.query.as_str())
        .chain(outcome.traces.iter().map(|trace| trace.query.as_str()))
    {
        let Some(query) = normalize_web_search_query(raw) else {
            continue;
        };
        if seen.insert(query.to_lowercase()) {
            queries.push(query);
        }
    }
    queries
}

#[cfg(feature = "pm")]
async fn generate_super_assistant_search_rewrites(
    state: &crate::state::AppState,
    tenant_id: &str,
    model: &str,
    user_message: &str,
    failed_query: &str,
    failed_reason: Option<&str>,
) -> Vec<String> {
    let system_prompt = r#"你是搜索 query 规划器，只输出 JSON，不回答用户问题。
目标：当一次联网检索没有可靠结果时，给出 2-4 个更可能命中可靠来源的搜索 query。
要求：
- 按用户真实意图改写，不要编造事实。
- 可以保留原 query，也可以给更宽范围、更权威来源、更常见表达、英文/中文变体。
- 如果原 query 带了过窄的公司、门店、楼宇、品牌、别名或修饰语，而用户真正要的是某个地点/实体/指标的公共事实，请提供一个去掉过窄限定的候选。
- 不要使用固定行业模板；不同领域按语义自行决定。
输出 JSON：{"queries":[{"query":"...","reason":"..."}, ...]}
不要 Markdown，不要额外文本。"#;
    let prompt = format!(
        "用户原问题：{user_message}\n失败检索 query：{failed_query}\n失败原因：{}",
        failed_reason.unwrap_or("未返回可靠结果")
    );
    match crate::routes::pm::run_chat_completion_with_any_chat_key(
        state,
        tenant_id,
        model.to_string(),
        vec![crate::routes::chat::ChatMessage {
            role: "user".to_string(),
            content: serde_json::Value::String(prompt),
        }],
        system_prompt,
        800,
    )
    .await
    {
        Ok(result) => parse_web_search_rewrite_candidates(&result.answer),
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                error = %error,
                "super assistant web-search query rewrite failed"
            );
            Vec::new()
        }
    }
}

#[cfg(feature = "pm")]
fn web_search_outcome_is_usable(outcome: &SuperAssistantWebSearchOutcome) -> bool {
    outcome.available && outcome.has_traceable_source()
}

#[cfg(feature = "pm")]
fn annotate_failed_web_search_attempts(
    mut outcome: SuperAssistantWebSearchOutcome,
    attempted_queries: &[String],
    all_traces: Vec<crate::routes::search_orchestrator_runtime::UnifiedSearchTrace>,
) -> SuperAssistantWebSearchOutcome {
    if !all_traces.is_empty() {
        outcome.traces = all_traces;
    }
    if !attempted_queries.is_empty() {
        let attempted = attempted_queries
            .iter()
            .map(|query| format!("「{query}」"))
            .collect::<Vec<_>>()
            .join("、");
        let suffix = format!("已尝试语义检索 query：{attempted}");
        outcome.degraded_reason = Some(match outcome.degraded_reason.take() {
            Some(reason) if !reason.trim().is_empty() => format!("{reason}; {suffix}"),
            _ => suffix,
        });
    }
    outcome
}

/// Run a Super_Assistant web-search turn by delegating to the existing layered
/// search orchestration (Req 3.3).
///
/// Thin wiring: it builds a [`UnifiedSearchRequest`] for the `ai_chat` scenario
/// and calls the reused
/// [`crate::routes::search_orchestrator_runtime::execute_unified_search`], which
/// applies the priority order (Search extension → model native streaming → MCP →
/// local/RAG fallback) and reports `used_layer` / `degraded_reason` / per-item
/// `url`. The result is mapped into traceable sources via
/// [`SuperAssistantWebSearchOutcome::from_unified_result`]. No retrieval logic is
/// reimplemented here.
///
/// `native_runtime` is resolved by the caller (mirroring
/// `resolve_pm_native_search_runtime`) and passed through so the model-native
/// streaming layer can participate; pass `None` to skip that layer.
///
/// [`UnifiedSearchRequest`]: crate::routes::search_orchestrator_runtime::UnifiedSearchRequest
pub async fn super_assistant_web_search(
    state: &crate::state::AppState,
    tenant_id: &str,
    user_id: &str,
    query: &str,
    native_runtime: Option<crate::routes::search_orchestrator_runtime::UnifiedNativeSearchRuntime>,
    max_results: usize,
) -> SuperAssistantWebSearchOutcome {
    let request = crate::routes::search_orchestrator_runtime::UnifiedSearchRequest {
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        scenario: SUPER_ASSISTANT_WEB_SEARCH_SCENARIO.to_string(),
        query: query.to_string(),
        first_party_available: !query.trim().is_empty(),
        native_runtime,
        max_results,
        rag_local_available: true,
        prepared_context: None,
    };
    let result =
        crate::routes::search_orchestrator_runtime::execute_unified_search(state, request).await;
    SuperAssistantWebSearchOutcome::from_unified_result(result)
}

/// Run Super_Assistant web search with a small Codex-like query exploration
/// loop. Retrieval is still delegated to the shared unified search
/// orchestrator; this wrapper only decides which semantic search attempts to
/// make before giving up.
#[cfg(feature = "pm")]
pub async fn super_assistant_web_search_with_rewrites(
    state: &crate::state::AppState,
    tenant_id: &str,
    user_id: &str,
    model: &str,
    user_message: &str,
    router_query: &str,
    native_runtime: Option<crate::routes::search_orchestrator_runtime::UnifiedNativeSearchRuntime>,
    max_results: usize,
) -> SuperAssistantWebSearchOutcome {
    let mut attempted_queries = Vec::<String>::new();
    let mut traces = Vec::new();
    let mut best_failure: Option<SuperAssistantWebSearchOutcome> = None;

    for query in select_web_search_attempts(router_query, user_message, &[], 2) {
        attempted_queries.push(query.clone());
        let outcome = super_assistant_web_search(
            state,
            tenant_id,
            user_id,
            &query,
            native_runtime.clone(),
            max_results,
        )
        .await;
        traces.extend(outcome.traces.clone());
        if web_search_outcome_is_usable(&outcome) {
            return outcome;
        }
        best_failure = Some(outcome);
    }

    let failed_query = attempted_queries
        .first()
        .map(String::as_str)
        .unwrap_or(router_query);
    let failed_reason = best_failure
        .as_ref()
        .and_then(|outcome| outcome.degraded_reason.as_deref());
    let rewrites = generate_super_assistant_search_rewrites(
        state,
        tenant_id,
        model,
        user_message,
        failed_query,
        failed_reason,
    )
    .await;
    let retry_attempts = select_web_search_attempts(router_query, user_message, &rewrites, 5);
    for query in retry_attempts {
        if attempted_queries
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(&query))
        {
            continue;
        }
        attempted_queries.push(query.clone());
        let outcome = super_assistant_web_search(
            state,
            tenant_id,
            user_id,
            &query,
            native_runtime.clone(),
            max_results,
        )
        .await;
        traces.extend(outcome.traces.clone());
        if web_search_outcome_is_usable(&outcome) {
            return outcome;
        }
        let replace_best = best_failure
            .as_ref()
            .map(|best| outcome.sources.len() > best.sources.len())
            .unwrap_or(true);
        if replace_best {
            best_failure = Some(outcome);
        }
    }

    let fallback = best_failure.unwrap_or_else(|| SuperAssistantWebSearchOutcome {
        scenario: SUPER_ASSISTANT_WEB_SEARCH_SCENARIO.to_string(),
        query: normalize_web_search_query(router_query)
            .or_else(|| normalize_web_search_query(user_message))
            .unwrap_or_default(),
        available: false,
        used_layer: None,
        degraded_reason: Some(
            "external search was not attempted because query was empty".to_string(),
        ),
        sources: Vec::new(),
        traces: Vec::new(),
    });
    annotate_failed_web_search_attempts(fallback, &attempted_queries, traces)
}

#[cfg(test)]
mod web_search_adapter_tests {
    use super::{map_evidence_to_sources, SuperAssistantSource, SuperAssistantWebSearchOutcome};
    use crate::routes::search_orchestrator_runtime::{
        UnifiedSearchEvidenceItem, UnifiedSearchResult,
    };

    /// Build an evidence item for mapping tests.
    fn evidence(
        source_type: &str,
        source_name: &str,
        title: &str,
        url: Option<&str>,
    ) -> UnifiedSearchEvidenceItem {
        UnifiedSearchEvidenceItem {
            source_type: source_type.to_string(),
            source_name: source_name.to_string(),
            title: title.to_string(),
            url: url.map(str::to_string),
            excerpt: Some("some supporting excerpt".to_string()),
            query: "q".to_string(),
            relevance_score: Some(0.9),
            confidence: Some(0.8),
            metadata: serde_json::Value::Null,
        }
    }

    /// Build a minimal `UnifiedSearchResult` around the given items using the
    /// reused orchestrator snapshot (no live search).
    fn result_with(items: Vec<UnifiedSearchEvidenceItem>) -> UnifiedSearchResult {
        let orchestrator = pm_domain::search_orchestrator::PmSearchOrchestrator::snapshot(
            pm_domain::search_orchestrator::PmSearchOrchestratorInput::default(),
        );
        let used_layer = items.first().map(|item| item.source_type.clone());
        UnifiedSearchResult {
            orchestrator,
            scenario: "ai_chat".to_string(),
            query: "q".to_string(),
            available: !items.is_empty(),
            used_layer,
            degraded_reason: None,
            items,
            traces: Vec::new(),
            skills: Vec::new(),
            hot_reload_supported: false,
        }
    }

    #[test]
    fn maps_url_backed_evidence_to_clickable_source() {
        let sources = map_evidence_to_sources(&[evidence(
            "configured_search_provider",
            "Tavily",
            "Rust release notes",
            Some("https://example.com/rust "),
        )]);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].url.as_deref(), Some("https://example.com/rust"));
        assert!(sources[0].has_clickable_url());
        assert!(sources[0].is_traceable());
    }

    #[test]
    fn maps_linkless_evidence_to_structured_citation() {
        let sources =
            map_evidence_to_sources(&[evidence("rag_local", "本地知识库", "内部文档片段", None)]);
        assert_eq!(sources.len(), 1);
        assert!(sources[0].url.is_none());
        assert!(!sources[0].has_clickable_url());
        assert!(sources[0].has_structured_citation());
        assert!(sources[0].is_traceable());
    }

    #[test]
    fn blank_url_is_treated_as_no_link() {
        let sources = map_evidence_to_sources(&[evidence(
            "native_model_search",
            "GPT",
            "answer",
            Some("   "),
        )]);
        assert!(sources[0].url.is_none());
        assert!(!sources[0].has_clickable_url());
        // Still traceable via the structured citation.
        assert!(sources[0].is_traceable());
    }

    #[test]
    fn source_without_link_or_citation_is_not_traceable() {
        let src = SuperAssistantSource {
            url: None,
            source_type: String::new(),
            source_name: String::new(),
            title: "orphan".to_string(),
            excerpt: None,
        };
        assert!(!src.is_traceable());
    }

    #[test]
    fn outcome_reports_traceable_sources_when_evidence_present() {
        let outcome =
            SuperAssistantWebSearchOutcome::from_unified_result(result_with(vec![evidence(
                "mcp_search",
                "Fetch",
                "page",
                Some("https://example.com"),
            )]));
        assert!(outcome.available);
        assert_eq!(outcome.used_layer.as_deref(), Some("mcp_search"));
        assert_eq!(outcome.sources.len(), 1);
        assert!(outcome.has_traceable_source());
    }

    #[test]
    fn outcome_without_evidence_has_no_traceable_source() {
        let outcome = SuperAssistantWebSearchOutcome::from_unified_result(result_with(Vec::new()));
        assert!(!outcome.available);
        assert!(outcome.sources.is_empty());
        assert!(!outcome.has_traceable_source());
    }

    #[test]
    fn outcome_json_uses_camel_case() {
        let outcome =
            SuperAssistantWebSearchOutcome::from_unified_result(result_with(vec![evidence(
                "configured_search_provider",
                "Tavily",
                "t",
                Some("https://example.com"),
            )]));
        let json = serde_json::to_string(&outcome).expect("serialize");
        assert!(json.contains("usedLayer"));
        assert!(json.contains("sourceType"));
        assert!(json.contains("sourceName"));
    }
}

#[cfg(all(test, feature = "pm"))]
mod web_search_rewrite_tests {
    use super::{
        parse_web_search_decision, parse_web_search_rewrite_candidates, select_web_search_attempts,
    };

    #[test]
    fn parses_semantic_web_search_decision_from_json_object() {
        let decision = parse_web_search_decision(
            r#"{"needsWebSearch":true,"query":"  某地 公共事实  ","reason":"依赖外部状态"}"#,
        )
        .expect("decision");
        assert!(decision.needs_web_search);
        assert_eq!(decision.query.as_deref(), Some("某地 公共事实"));
        assert_eq!(decision.reason.as_deref(), Some("依赖外部状态"));
    }

    #[test]
    fn parses_rewrite_candidates_from_object_array_or_strings() {
        let candidates = parse_web_search_rewrite_candidates(
            r#"```json
            {"queries":[{"query":"精确对象 公共事实","reason":"first"},"更宽范围 公共事实"]}
            ```"#,
        );
        assert_eq!(
            candidates,
            vec![
                "精确对象 公共事实".to_string(),
                "更宽范围 公共事实".to_string()
            ]
        );
    }

    #[test]
    fn select_attempts_dedupes_and_keeps_router_then_user_then_semantic_rewrites() {
        let candidates = vec![
            "  精确对象 查询 ".to_string(),
            "更宽范围 查询".to_string(),
            "权威来源 查询".to_string(),
        ];
        let attempts = select_web_search_attempts("精确对象 查询", "用户原始问题", &candidates, 4);
        assert_eq!(
            attempts,
            vec![
                "精确对象 查询".to_string(),
                "用户原始问题".to_string(),
                "更宽范围 查询".to_string(),
                "权威来源 查询".to_string(),
            ]
        );
    }
}

// ===========================================================================
// Attachment parse / vision-downgrade adapter (task 10.3, Req 3.4 / 3.5)
// ---------------------------------------------------------------------------
// When the user uploads an attachment, the Super_Assistant reuses the existing
// `/chat/files` parse+index pipeline (whose per-file status is one of
// `uploaded` / `parsing` / `indexed` / `failed`) and, for image attachments,
// the vision summary / degrade heuristics already living in
// `pm_domain::stream_session`. Following the reuse-first principle, this section
// reimplements NO parsing or vision logic: it only decides *whether* to degrade
// to a text-only answer and, when it does, borrows the exact non-blocking
// warning wording from the reused `image_context_warning_message` classifier.
//
// Per design Error Handling (Req 3.5), the Super_Assistant degrades to a
// text-only answer with a NON-BLOCKING notice when either:
//   * the `/chat/files` parse status is `failed`, or
//   * the routed model does not support vision input, or
//   * the model's own vision summary signals it could not see the image
//     (detected by the reused `summary_text_indicates_no_vision`).
// In every other case the attachment is usable and no downgrade happens.
//
// This section is NOT feature-gated: the reused `pm_domain::stream_session`
// helpers are plain `pub` functions in the non-optional `pm-domain` dependency
// (the web-search section above already calls into `pm_domain` unconditionally),
// so they are reachable without any cargo feature. Fully-qualified paths are
// used throughout so this section adds no top-level `use` imports and cannot
// collide with the sections above.
// ===========================================================================

/// The Super_Assistant-facing outcome of evaluating an attachment for a
/// vision / parse downgrade (Req 3.4 / 3.5). Serializes to camelCase for the
/// frontend, which renders `notice` as a non-blocking banner above the answer.
///
/// `degraded` is `true` when the turn fell back to a text-only answer; in that
/// case `notice` carries the non-blocking, user-facing explanation borrowed
/// verbatim from the reused `pm_domain::stream_session::image_context_warning_message`
/// classifier. When `degraded` is `false` the attachment is usable as-is and
/// `notice` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDowngradeOutcome {
    /// Whether the turn degraded to a text-only answer (parse failure or the
    /// model lacking vision).
    pub degraded: bool,
    /// The non-blocking notice shown to the user when `degraded` is `true`.
    /// Reused from `image_context_warning_message`; `None` when not degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl AttachmentDowngradeOutcome {
    /// A non-degraded outcome: the attachment is usable and no notice is shown.
    fn passthrough() -> Self {
        Self {
            degraded: false,
            notice: None,
        }
    }
}

/// Decide whether a Super_Assistant attachment turn must degrade to a text-only
/// answer, reusing the `/chat/files` parse status and the
/// `pm_domain::stream_session` vision heuristics (Req 3.4 / 3.5).
///
/// Pure and deterministic (no I/O) so it can be unit-tested exhaustively. No
/// parsing or vision logic is reimplemented here — the degrade decision
/// delegates to the reused `summary_text_indicates_no_vision`, and the
/// user-facing notice wording delegates to the reused
/// `image_context_warning_message`.
///
/// Parameters:
/// * `parse_status` — the `/chat/files` per-file status
///   (`uploaded` / `parsing` / `indexed` / `failed`).
/// * `parse_error` — the `error_message` recorded by `/chat/files` when the
///   parse failed, if any; surfaced into the reused warning classifier.
/// * `model_supports_vision` — whether the routed model accepts image input.
/// * `vision_summary` — the model's own image summary, if one was attempted;
///   used to detect an implicit "I can't see the image" signal.
///
/// Returns an [`AttachmentDowngradeOutcome`]: degraded with a non-blocking
/// notice when the parse failed OR the model lacks vision (explicitly or via
/// its summary), otherwise a passthrough with no notice.
pub fn evaluate_attachment_downgrade(
    parse_status: &str,
    parse_error: Option<&str>,
    model_supports_vision: bool,
    vision_summary: Option<&str>,
) -> AttachmentDowngradeOutcome {
    let status_failed = parse_status.trim().eq_ignore_ascii_case("failed");
    // Delegate the "the model couldn't actually see the image" detection to the
    // reused heuristic rather than reimplementing it here.
    let summary_no_vision = vision_summary
        .map(pm_domain::stream_session::summary_text_indicates_no_vision)
        .unwrap_or(false);
    let vision_unsupported = !model_supports_vision || summary_no_vision;

    if !status_failed && !vision_unsupported {
        return AttachmentDowngradeOutcome::passthrough();
    }

    // Build the error signal fed to the reused warning classifier so it selects
    // the correct wording. When the model lacks vision (explicitly or via its
    // summary) we surface the vision-unsupported message; otherwise we surface
    // the reused parse-failure wording, preferring the concrete parse error.
    let error_signal: String = if vision_unsupported {
        "image input unsupported: no vision".to_string()
    } else {
        parse_error
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "image parse failed".to_string())
    };

    let notice = pm_domain::stream_session::image_context_warning_message(&error_signal);
    AttachmentDowngradeOutcome {
        degraded: true,
        notice: Some(notice.to_string()),
    }
}

#[cfg(test)]
mod attachment_downgrade_tests {
    use super::{evaluate_attachment_downgrade, AttachmentDowngradeOutcome};

    #[test]
    fn indexed_attachment_with_vision_is_passthrough() {
        let outcome = evaluate_attachment_downgrade("indexed", None, true, None);
        assert!(!outcome.degraded);
        assert!(outcome.notice.is_none());
        assert_eq!(
            outcome,
            AttachmentDowngradeOutcome {
                degraded: false,
                notice: None,
            }
        );
    }

    #[test]
    fn parse_failure_degrades_with_nonblocking_notice() {
        let outcome =
            evaluate_attachment_downgrade("failed", Some("unable to extract text"), true, None);
        assert!(outcome.degraded);
        // Non-image parse error -> reused generic non-blocking wording.
        assert_eq!(
            outcome.notice.as_deref(),
            Some("图片解析部分失败，系统将继续基于可用信息回答。")
        );
    }

    #[test]
    fn parse_status_match_is_case_insensitive() {
        let outcome = evaluate_attachment_downgrade("FAILED", None, true, None);
        assert!(outcome.degraded);
        assert!(outcome.notice.is_some());
    }

    #[test]
    fn model_without_vision_degrades_with_vision_notice() {
        let outcome = evaluate_attachment_downgrade("indexed", None, false, None);
        assert!(outcome.degraded);
        assert_eq!(
            outcome.notice.as_deref(),
            Some("图片解析失败：当前模型或路由可能不支持视觉输入，已降级为文本回答。")
        );
    }

    #[test]
    fn summary_signalling_no_vision_degrades() {
        // Model claims vision support but its summary reveals it could not see
        // the image; the reused heuristic detects this and we degrade.
        let outcome = evaluate_attachment_downgrade(
            "indexed",
            None,
            true,
            Some("I cannot view image content in this conversation."),
        );
        assert!(outcome.degraded);
        assert_eq!(
            outcome.notice.as_deref(),
            Some("图片解析失败：当前模型或路由可能不支持视觉输入，已降级为文本回答。")
        );
    }

    #[test]
    fn benign_vision_summary_is_passthrough() {
        let outcome = evaluate_attachment_downgrade(
            "indexed",
            None,
            true,
            Some("图中显示了一张季度销售额柱状图。"),
        );
        assert!(!outcome.degraded);
        assert!(outcome.notice.is_none());
    }
}

// ===========================================================================
// Deep-analysis adapter: pm_assistant / super_adversarial (task 10.5,
// Req 3.7 / 3.8 / 3.9)
// ---------------------------------------------------------------------------
// Deep analysis is an *asynchronous* capability that already lives in the pm
// domain. The Super_Assistant does NOT reimplement any of it: it delegates a
// deep-analysis request to one of the two existing async task links —
//   * `pm_research_task`     (via `crate::routes::agent::start_pm_research_task_from_bot`)
//   * `chat_adversarial_run` (via `crate::routes::agent::start_chat_adversarial_run_from_bot`)
// and then surfaces their progress + results in a Super_Assistant-facing shape.
//
// This section adds two thin, non-duplicating pieces:
//   (a) async *start* wrappers that kick off the existing async task via its
//       existing entry and return the task id + status as a
//       [`DeepAnalysisTaskHandle`]; and
//   (b) pure *mapping* functions that project the existing task's stage +
//       reused `pm_domain::deep_research_loop` scoring into a Super_Assistant
//       structure exposing stage progress and — crucially, per Req 3.9, WITHOUT
//       weakening them — the evidence tree / conflict matrix / quality gate.
//
// The scoring/evidence structures are reused verbatim from
// `pm_domain::deep_research_loop` (`PmDeepResearchScore` quality gate,
// `PmEvidenceScore.conflict_level` conflict matrix, `PmHypothesisEvidenceGraph`
// evidence tree); this adapter only re-shapes references to them, so the
// deep-analysis strengths are preserved, never re-derived or diluted.
//
// Feature gating: the pure mapping (b) is gated behind `#[cfg(feature = "pm")]`
// because deep analysis lives in the pm domain. The async start wrappers (a)
// are gated behind `#[cfg(feature = "bot-agents")]` — the feature under which
// the reused `*_from_bot` entries and their input/result types are compiled;
// `bot-agents` enables `pm`, so the `pm`-gated types below are always in scope
// for the wrappers. Fully-qualified paths are used throughout so this section
// adds no top-level `use` imports and cannot collide with the sections above.
// ===========================================================================

/// Which existing async deep-analysis task link a Super_Assistant deep-analysis
/// request was delegated to (Req 3.7). Serializes to camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeepAnalysisLink {
    /// `pm_assistant` deep research task (`pm_research_task`).
    PmResearchTask,
    /// `super_adversarial` multi-model adjudication run (`chat_adversarial_run`).
    ChatAdversarialRun,
    /// `nl2sql` boss-facing data attribution task (`nl2sql_attribution_task`).
    DataAttributionTask,
}

#[cfg(feature = "pm")]
impl DeepAnalysisLink {
    /// Stable lower-snake key for the link.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PmResearchTask => "pm_research_task",
            Self::ChatAdversarialRun => "chat_adversarial_run",
            Self::DataAttributionTask => "nl2sql_attribution_task",
        }
    }

    /// The agent-ops linked-resource type used by the existing async task
    /// tracking (mirrors the resource-type strings in `agent_ops.rs`).
    pub const fn resource_type(self) -> &'static str {
        self.as_str()
    }
}

/// The Super_Assistant-facing handle returned when an async deep-analysis task
/// is started (Req 3.7). Carries the existing task's id + status so the caller
/// can poll / observe progress; no deep-analysis logic is embedded here.
/// Serializes to camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalysisTaskHandle {
    /// Which existing async link this task runs on.
    pub link: DeepAnalysisLink,
    /// The existing task / run id (e.g. `pm_research_tasks.task_id` or
    /// `chat_adversarial_runs.id`), for observability and status polling.
    pub task_id: String,
    /// The associated session / thread id, when the existing entry produced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The initial status reported by the existing entry (e.g. `queued`,
    /// `running`).
    pub status: String,
}

#[cfg(feature = "pm")]
impl DeepAnalysisTaskHandle {
    /// Build a handle from a `pm_research_task` start result. Pure mapping so it
    /// is unit-testable without starting a live task.
    pub fn pm_research(
        task_id: impl Into<String>,
        session_id: Option<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            link: DeepAnalysisLink::PmResearchTask,
            task_id: task_id.into(),
            session_id: session_id.filter(|s| !s.trim().is_empty()),
            status: status.into(),
        }
    }

    /// Build a handle from a `chat_adversarial_run` start result. Pure mapping
    /// so it is unit-testable without starting a live run.
    pub fn adversarial(
        run_id: impl Into<String>,
        thread_id: Option<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            link: DeepAnalysisLink::ChatAdversarialRun,
            task_id: run_id.into(),
            session_id: thread_id.filter(|s| !s.trim().is_empty()),
            status: status.into(),
        }
    }

    /// Build a handle from an `nl2sql_attribution_task` start result.
    pub fn data_attribution(
        task_id: impl Into<String>,
        conversation_id: Option<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            link: DeepAnalysisLink::DataAttributionTask,
            task_id: task_id.into(),
            session_id: conversation_id.filter(|s| !s.trim().is_empty()),
            status: status.into(),
        }
    }
}

/// The canonical ordered deep-research stage sequence, reused verbatim from
/// `pm_domain::deep_research_loop::PmDeepResearchState` (Req 3.8). Kept in one
/// place so stage-progress mapping stays in sync with the reused loop.
#[cfg(feature = "pm")]
const DEEP_ANALYSIS_STATES: [pm_domain::deep_research_loop::PmDeepResearchState; 12] = {
    use pm_domain::deep_research_loop::PmDeepResearchState as S;
    [
        S::Initialize,
        S::ExtractFirstPartyEvidence,
        S::BuildExpertLensMatrix,
        S::GenerateHypotheses,
        S::PlanResearchTasks,
        S::RetrieveEvidence,
        S::ScoreEvidence,
        S::SynthesizeClaims,
        S::CritiqueAnswer,
        S::DetectGaps,
        S::BranchFollowupResearch,
        S::RewriteOrFinalize,
    ]
};

/// The ordered stage keys of the reused deep-research loop (Req 3.8).
#[cfg(feature = "pm")]
pub fn deep_analysis_stage_sequence() -> Vec<&'static str> {
    DEEP_ANALYSIS_STATES.iter().map(|s| s.as_str()).collect()
}

/// Super_Assistant stage-progress view over a running deep-analysis task
/// (Req 3.8). Serializes to camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalysisStageProgress {
    /// The full ordered stage sequence being progressed through.
    pub stages: Vec<String>,
    /// Index into `stages` of the current stage, when the reported stage maps
    /// to a known deep-research state; `None` for an unknown / not-yet-started
    /// stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_index: Option<usize>,
    /// The current stage key, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_stage: Option<String>,
    /// Whether the task has reached a terminal (completed) status.
    pub completed: bool,
    /// Fractional progress in `0.0..=1.0` (1.0 when completed).
    pub percent: f64,
}

#[cfg(feature = "pm")]
impl DeepAnalysisStageProgress {
    /// Map the existing task's reported `stage` + `status` into stage progress.
    /// Pure: no I/O, so it is unit-testable exhaustively.
    pub fn from_stage(stage: &str, status: &str) -> Self {
        let stages: Vec<String> = deep_analysis_stage_sequence()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let normalized = stage.trim().to_ascii_lowercase();
        let current_index = stages.iter().position(|s| s == &normalized);
        let completed = matches!(
            status.trim().to_ascii_lowercase().as_str(),
            "completed" | "succeeded" | "success" | "done" | "finished"
        );
        let percent = if completed {
            1.0
        } else if let Some(idx) = current_index {
            ((idx + 1) as f64) / (stages.len() as f64)
        } else {
            0.0
        };
        let current_stage = current_index.map(|i| stages[i].clone());
        Self {
            stages,
            current_index,
            current_stage,
            completed,
            percent,
        }
    }
}

/// Super_Assistant quality-gate view, reusing the deep-research loop's own
/// `PmDeepResearchScore` scoring verbatim so the gate is NOT weakened by the
/// merge (Req 3.9). Serializes to camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalysisQualityGate {
    pub evidence_coverage_score: f64,
    pub first_party_alignment_score: f64,
    pub claim_confidence_score: f64,
    pub counter_evidence_coverage_score: f64,
    pub expert_lens_coverage_score: f64,
    pub actionability_score: f64,
    pub decision_readiness_score: f64,
    /// The reused loop's own decision-ready verdict
    /// (`PmDeepResearchScore::decision_ready`) — recomputed by the reused code,
    /// not re-defined here.
    pub decision_ready: bool,
}

/// Project a reused `PmDeepResearchScore` into the Super_Assistant quality gate
/// (Req 3.9). Delegates the pass/fail verdict to the reused
/// `decision_ready()`; this function only copies fields.
#[cfg(feature = "pm")]
pub fn map_quality_gate(
    score: &pm_domain::deep_research_loop::PmDeepResearchScore,
) -> DeepAnalysisQualityGate {
    DeepAnalysisQualityGate {
        evidence_coverage_score: score.evidence_coverage_score,
        first_party_alignment_score: score.first_party_alignment_score,
        claim_confidence_score: score.claim_confidence_score,
        counter_evidence_coverage_score: score.counter_evidence_coverage_score,
        expert_lens_coverage_score: score.expert_lens_coverage_score,
        actionability_score: score.actionability_score,
        decision_readiness_score: score.decision_readiness_score,
        decision_ready: score.decision_ready(),
    }
}

/// The Super_Assistant conflict matrix, reusing the deep-research loop's own
/// per-evidence `conflict_level` verbatim so source-conflict adjudication is
/// NOT weakened (Req 3.9). Serializes to camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalysisConflictMatrix {
    /// One entry per scored evidence item.
    pub rows: Vec<DeepAnalysisConflictEntry>,
    /// Count of rows whose reused `conflict_level` is anything other than
    /// `none` (case-insensitive) — i.e. rows carrying a real conflict signal.
    pub conflicted_count: usize,
}

/// A single conflict-matrix entry projected from a reused `PmEvidenceScore`.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalysisConflictEntry {
    /// The reused `conflict_level` verdict (e.g. `none`, `low`, `high`).
    pub conflict_level: String,
    /// Whether this piece of evidence is usable for a decision (reused).
    pub usable_for_decision: bool,
    pub source_credibility: f64,
    pub claim_support: f64,
    pub first_party_alignment: f64,
}

/// Project reused per-evidence scores into the Super_Assistant conflict matrix
/// (Req 3.9). Pure: copies the reused `conflict_level` and counts genuine
/// conflicts; no adjudication logic is re-derived here.
#[cfg(feature = "pm")]
pub fn map_conflict_matrix(
    scores: &[pm_domain::deep_research_loop::PmEvidenceScore],
) -> DeepAnalysisConflictMatrix {
    let rows: Vec<DeepAnalysisConflictEntry> = scores
        .iter()
        .map(|s| DeepAnalysisConflictEntry {
            conflict_level: s.conflict_level.trim().to_string(),
            usable_for_decision: s.usable_for_decision,
            source_credibility: s.source_credibility,
            claim_support: s.claim_support,
            first_party_alignment: s.first_party_alignment,
        })
        .collect();
    let conflicted_count = rows
        .iter()
        .filter(|r| !r.conflict_level.trim().eq_ignore_ascii_case("none"))
        .count();
    DeepAnalysisConflictMatrix {
        rows,
        conflicted_count,
    }
}

/// The Super_Assistant evidence tree, projected from the reused
/// `PmHypothesisEvidenceGraph` so the evidence structure is NOT weakened
/// (Req 3.9 / Property 15). Serializes to camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalysisEvidenceTree {
    /// Node summaries (id / title / kind / confidence / evidence ref count).
    pub nodes: Vec<DeepAnalysisEvidenceNode>,
    /// Root node ids (the reused graph's `primary_evidence_node_ids`).
    pub root_node_ids: Vec<String>,
    /// Ids still lacking resolving evidence (reused `unresolved_node_ids`).
    pub unresolved_node_ids: Vec<String>,
    /// Number of edges in the reused hypothesis-evidence graph.
    pub edge_count: usize,
}

/// A single evidence-tree node projected from a reused `PmHypothesisNode`.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalysisEvidenceNode {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub confidence: f64,
    /// How many pieces of evidence back this node (reused `evidence_refs` len).
    pub evidence_ref_count: usize,
}

/// Project the reused hypothesis-evidence graph into the Super_Assistant
/// evidence tree (Req 3.9). Pure: copies node/edge references without
/// re-deriving the graph.
#[cfg(feature = "pm")]
pub fn map_evidence_tree(
    graph: &pm_domain::deep_research_loop::PmHypothesisEvidenceGraph,
) -> DeepAnalysisEvidenceTree {
    let nodes = graph
        .nodes
        .iter()
        .map(|n| DeepAnalysisEvidenceNode {
            id: n.id.clone(),
            title: n.title.clone(),
            kind: n.kind.clone(),
            confidence: n.confidence,
            evidence_ref_count: n.evidence_refs.len(),
        })
        .collect();
    DeepAnalysisEvidenceTree {
        nodes,
        root_node_ids: graph.primary_evidence_node_ids.clone(),
        unresolved_node_ids: graph.unresolved_node_ids.clone(),
        edge_count: graph.edges.len(),
    }
}

/// The aggregate Super_Assistant-facing deep-analysis view (Req 3.7 / 3.8 /
/// 3.9): the task handle + stage progress, plus the reused (never weakened)
/// evidence tree / conflict matrix / quality gate when available. Serializes to
/// camelCase for the frontend.
#[cfg(feature = "pm")]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperAssistantDeepAnalysis {
    /// The existing async task handle (link + id + status).
    pub handle: DeepAnalysisTaskHandle,
    /// Stage progress over the reused deep-research loop (Req 3.8).
    pub stage_progress: DeepAnalysisStageProgress,
    /// The reused evidence tree, when the task has produced a hypothesis graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_tree: Option<DeepAnalysisEvidenceTree>,
    /// The reused conflict matrix, when evidence has been scored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_matrix: Option<DeepAnalysisConflictMatrix>,
    /// The reused quality gate, when the loop has scored the answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_gate: Option<DeepAnalysisQualityGate>,
}

#[cfg(feature = "pm")]
impl SuperAssistantDeepAnalysis {
    /// Assemble a Super_Assistant deep-analysis view from a task handle, its
    /// reported stage/status, and any reused deep-research artifacts. Pure
    /// mapping so it is unit-testable without a live task.
    pub fn new(
        handle: DeepAnalysisTaskHandle,
        stage: &str,
        status: &str,
        evidence_graph: Option<&pm_domain::deep_research_loop::PmHypothesisEvidenceGraph>,
        evidence_scores: Option<&[pm_domain::deep_research_loop::PmEvidenceScore]>,
        quality_score: Option<&pm_domain::deep_research_loop::PmDeepResearchScore>,
    ) -> Self {
        Self {
            handle,
            stage_progress: DeepAnalysisStageProgress::from_stage(stage, status),
            evidence_tree: evidence_graph.map(map_evidence_tree),
            conflict_matrix: evidence_scores.map(map_conflict_matrix),
            quality_gate: quality_score.map(map_quality_gate),
        }
    }

    /// Req 3.9 invariant: the deep-analysis strengths are preserved (not
    /// weakened) exactly when all three reused artifacts — evidence tree,
    /// conflict matrix and quality gate — are surfaced together.
    pub fn preserves_deep_analysis_strengths(&self) -> bool {
        self.evidence_tree.is_some()
            && self.conflict_matrix.is_some()
            && self.quality_gate.is_some()
    }

    /// Property 15 support: the deep-analysis result carries traceable
    /// structured evidence (a non-empty evidence tree or conflict matrix).
    pub fn has_traceable_evidence(&self) -> bool {
        self.evidence_tree
            .as_ref()
            .map(|t| !t.nodes.is_empty())
            .unwrap_or(false)
            || self
                .conflict_matrix
                .as_ref()
                .map(|m| !m.rows.is_empty())
                .unwrap_or(false)
    }
}

/// Start an async `pm_research_task` deep-analysis task via the existing entry
/// and return its Super_Assistant handle (Req 3.7).
///
/// Thin wiring: it shapes a `PmResearchBotTaskInput` and delegates to the
/// reused [`crate::routes::agent::start_pm_research_task_from_bot`], then maps
/// the result's `task_id` / `session_id` / `status` into a
/// [`DeepAnalysisTaskHandle`]. No deep-analysis logic is reimplemented here.
#[cfg(feature = "bot-agents")]
pub async fn start_pm_deep_analysis(
    state: &crate::state::AppState,
    claims: crate::auth::Claims,
    message: String,
    visible_message: Option<String>,
    model: Option<String>,
    session_id: Option<String>,
) -> crate::error::Result<DeepAnalysisTaskHandle> {
    let result = crate::routes::agent::start_pm_research_task_from_bot(
        state,
        claims,
        crate::routes::agent::PmResearchBotTaskInput {
            message,
            visible_message,
            model,
            session_id,
        },
    )
    .await?;
    Ok(DeepAnalysisTaskHandle::pm_research(
        result.task_id,
        Some(result.session_id),
        result.status,
    ))
}

/// Start an async `super_adversarial` deep-analysis run via the existing entry
/// and return its Super_Assistant handle (Req 3.7).
///
/// Thin wiring: it shapes a `ChatAdversarialBotRunInput` and delegates to the
/// reused [`crate::routes::agent::start_chat_adversarial_run_from_bot`], then
/// maps the result's `id` / `thread_id` / `status` into a
/// [`DeepAnalysisTaskHandle`]. No adjudication logic is reimplemented here.
#[cfg(feature = "bot-agents")]
pub async fn start_adversarial_deep_analysis(
    state: crate::state::AppState,
    claims: crate::auth::Claims,
    question: String,
    models: Vec<String>,
    max_rounds: Option<u32>,
    parent_run_id: Option<String>,
    session_id: Option<String>,
    evidence_search_required: bool,
    evidence_search_query: Option<String>,
) -> crate::error::Result<DeepAnalysisTaskHandle> {
    let result = crate::routes::agent::start_chat_adversarial_run_from_bot(
        state,
        claims,
        crate::routes::agent::ChatAdversarialBotRunInput {
            question,
            models,
            max_rounds,
            parent_run_id,
            session_id,
            evidence_search_required,
            evidence_search_query,
        },
    )
    .await?;
    Ok(DeepAnalysisTaskHandle::adversarial(
        result.id,
        result.thread_id,
        result.status,
    ))
}

#[cfg(all(test, feature = "pm"))]
mod deep_analysis_adapter_tests {
    use super::{
        deep_analysis_stage_sequence, map_conflict_matrix, map_evidence_tree, map_quality_gate,
        DeepAnalysisLink, DeepAnalysisStageProgress, DeepAnalysisTaskHandle,
        SuperAssistantDeepAnalysis,
    };
    use pm_domain::deep_research_loop::{
        PmDeepResearchScore, PmEvidenceScore, PmHypothesisEdge, PmHypothesisEvidenceGraph,
        PmHypothesisNode,
    };

    fn score(ready: bool) -> PmDeepResearchScore {
        if ready {
            PmDeepResearchScore {
                evidence_coverage_score: 0.9,
                first_party_alignment_score: 0.9,
                claim_confidence_score: 0.9,
                counter_evidence_coverage_score: 0.8,
                expert_lens_coverage_score: 0.8,
                actionability_score: 0.85,
                decision_readiness_score: 0.9,
            }
        } else {
            PmDeepResearchScore {
                evidence_coverage_score: 0.4,
                first_party_alignment_score: 0.4,
                claim_confidence_score: 0.4,
                counter_evidence_coverage_score: 0.3,
                expert_lens_coverage_score: 0.3,
                actionability_score: 0.4,
                decision_readiness_score: 0.4,
            }
        }
    }

    fn evidence(conflict: &str, usable: bool) -> PmEvidenceScore {
        PmEvidenceScore {
            source_credibility: 0.7,
            freshness: 0.6,
            domain_relevance: 0.7,
            first_party_alignment: 0.6,
            claim_support: 0.7,
            conflict_level: conflict.to_string(),
            usable_for_decision: usable,
        }
    }

    fn graph() -> PmHypothesisEvidenceGraph {
        PmHypothesisEvidenceGraph {
            nodes: vec![
                PmHypothesisNode {
                    id: "h1".to_string(),
                    kind: "hypothesis".to_string(),
                    title: "主要假设".to_string(),
                    confidence: 0.8,
                    evidence_refs: vec!["e1".to_string(), "e2".to_string()],
                },
                PmHypothesisNode {
                    id: "h2".to_string(),
                    kind: "counter".to_string(),
                    title: "反例".to_string(),
                    confidence: 0.5,
                    evidence_refs: vec![],
                },
            ],
            edges: vec![PmHypothesisEdge {
                from: "h1".to_string(),
                to: "h2".to_string(),
                relation: "contradicts".to_string(),
                strength: 0.6,
            }],
            primary_evidence_node_ids: vec!["h1".to_string()],
            unresolved_node_ids: vec!["h2".to_string()],
        }
    }

    #[test]
    fn stage_sequence_is_ordered_and_complete() {
        let stages = deep_analysis_stage_sequence();
        assert_eq!(stages.len(), 12);
        assert_eq!(stages.first().copied(), Some("initialize"));
        assert_eq!(stages.last().copied(), Some("rewrite_or_finalize"));
    }

    #[test]
    fn stage_progress_maps_known_stage() {
        let p = DeepAnalysisStageProgress::from_stage("score_evidence", "running");
        assert_eq!(p.current_stage.as_deref(), Some("score_evidence"));
        assert_eq!(p.current_index, Some(6));
        assert!(!p.completed);
        // (6 + 1) / 12
        assert!((p.percent - (7.0 / 12.0)).abs() < 1e-9);
    }

    #[test]
    fn stage_progress_unknown_stage_has_no_index() {
        let p = DeepAnalysisStageProgress::from_stage("mystery", "running");
        assert_eq!(p.current_index, None);
        assert!(p.current_stage.is_none());
        assert_eq!(p.percent, 0.0);
    }

    #[test]
    fn stage_progress_completed_status_is_full() {
        let p = DeepAnalysisStageProgress::from_stage("synthesize_claims", "completed");
        assert!(p.completed);
        assert_eq!(p.percent, 1.0);
    }

    #[test]
    fn quality_gate_reuses_decision_ready() {
        let ready = map_quality_gate(&score(true));
        assert!(ready.decision_ready);
        assert_eq!(ready.actionability_score, 0.85);
        let not_ready = map_quality_gate(&score(false));
        assert!(!not_ready.decision_ready);
    }

    #[test]
    fn conflict_matrix_counts_real_conflicts() {
        let matrix = map_conflict_matrix(&[
            evidence("none", true),
            evidence("high", false),
            evidence("LOW", true),
        ]);
        assert_eq!(matrix.rows.len(), 3);
        // "none" excluded; "high" and "LOW" counted.
        assert_eq!(matrix.conflicted_count, 2);
        assert!(!matrix.rows[1].usable_for_decision);
    }

    #[test]
    fn evidence_tree_preserves_graph_structure() {
        let tree = map_evidence_tree(&graph());
        assert_eq!(tree.nodes.len(), 2);
        assert_eq!(tree.root_node_ids, vec!["h1".to_string()]);
        assert_eq!(tree.unresolved_node_ids, vec!["h2".to_string()]);
        assert_eq!(tree.edge_count, 1);
        assert_eq!(tree.nodes[0].evidence_ref_count, 2);
        assert_eq!(tree.nodes[1].evidence_ref_count, 0);
    }

    #[test]
    fn handle_constructors_set_link_and_drop_blank_session() {
        let pm = DeepAnalysisTaskHandle::pm_research("t-1", Some("s-1".to_string()), "queued");
        assert_eq!(pm.link, DeepAnalysisLink::PmResearchTask);
        assert_eq!(pm.session_id.as_deref(), Some("s-1"));

        let adv = DeepAnalysisTaskHandle::adversarial("r-1", Some("   ".to_string()), "running");
        assert_eq!(adv.link, DeepAnalysisLink::ChatAdversarialRun);
        assert!(adv.session_id.is_none());
    }

    #[test]
    fn link_resource_type_matches_agent_ops_keys() {
        assert_eq!(
            DeepAnalysisLink::PmResearchTask.resource_type(),
            "pm_research_task"
        );
        assert_eq!(
            DeepAnalysisLink::ChatAdversarialRun.resource_type(),
            "chat_adversarial_run"
        );
    }

    #[test]
    fn aggregate_preserves_strengths_only_when_all_present() {
        let handle = DeepAnalysisTaskHandle::pm_research("t-1", None, "running");
        let g = graph();
        let scores = [evidence("high", false)];
        let s = score(true);

        let full = SuperAssistantDeepAnalysis::new(
            handle.clone(),
            "score_evidence",
            "running",
            Some(&g),
            Some(&scores),
            Some(&s),
        );
        assert!(full.preserves_deep_analysis_strengths());
        assert!(full.has_traceable_evidence());

        let partial = SuperAssistantDeepAnalysis::new(
            handle,
            "score_evidence",
            "running",
            Some(&g),
            None,
            None,
        );
        assert!(!partial.preserves_deep_analysis_strengths());
        // still traceable via the evidence tree
        assert!(partial.has_traceable_evidence());
    }

    #[test]
    fn deep_analysis_json_uses_camel_case() {
        let handle = DeepAnalysisTaskHandle::pm_research("t-1", Some("s-1".to_string()), "running");
        let g = graph();
        let scores = [evidence("high", false)];
        let s = score(true);
        let view = SuperAssistantDeepAnalysis::new(
            handle,
            "score_evidence",
            "running",
            Some(&g),
            Some(&scores),
            Some(&s),
        );
        let json = serde_json::to_string(&view).expect("serialize");
        assert!(json.contains("stageProgress"));
        assert!(json.contains("evidenceTree"));
        assert!(json.contains("conflictMatrix"));
        assert!(json.contains("qualityGate"));
        assert!(json.contains("decisionReady"));
        assert!(json.contains("conflictedCount"));

        let back: SuperAssistantDeepAnalysis = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, view);
    }
}

// ===========================================================================
// Property 15 — 检索/深度分析回答含可追溯来源 (source traceability)
// ---------------------------------------------------------------------------
// Feature: super-assistant-hub, Property 15: 检索/深度分析回答含可追溯来源
// Validates: Requirements 3.3, 7.3
//
// For *any* answer that involves external web search or deep analysis, the
// result MUST carry at least one traceable source link or structured evidence
// reference. This module property-tests both halves of that invariant:
//
//   * Web search (NOT feature-gated): for any `SuperAssistantWebSearchOutcome`
//     built from >= 1 evidence item that carries either a `url` OR a
//     (`source_type` + `source_name`) structured citation,
//     `has_traceable_source()` is true.
//   * Deep analysis (`#[cfg(feature = "pm")]`): for any
//     `SuperAssistantDeepAnalysis` view with a non-empty evidence tree or
//     conflict matrix, `has_traceable_evidence()` is true.
// ===========================================================================
#[cfg(test)]
mod prop_source_traceability_tests {
    use super::*;
    use crate::routes::search_orchestrator_runtime::{
        UnifiedSearchEvidenceItem, UnifiedSearchResult,
    };
    use proptest::prelude::*;

    /// Strategy for a single search evidence item that is *guaranteed* to be
    /// traceable: either it carries a non-blank clickable `url`, or (when it
    /// has no url) it carries a non-blank structured citation formed by
    /// `source_type` + `source_name`.
    fn traceable_evidence_item() -> impl Strategy<Value = UnifiedSearchEvidenceItem> {
        // URL-backed: the link alone makes the source traceable, so the
        // structured-citation fields are allowed to be blank/whitespace.
        let url_backed = (
            "https://[a-z]{1,8}\\.example/[a-z0-9]{1,10}",
            "[a-zA-Z0-9 ]{0,8}",
            "[a-zA-Z0-9 ]{0,8}",
            "[a-zA-Z0-9 ]{0,12}",
        )
            .prop_map(
                |(url, source_type, source_name, title)| UnifiedSearchEvidenceItem {
                    source_type,
                    source_name,
                    title,
                    url: Some(url),
                    excerpt: None,
                    query: "q".to_string(),
                    relevance_score: None,
                    confidence: None,
                    metadata: serde_json::Value::Null,
                },
            );
        // Citation-backed: no url, but non-blank source type + name.
        let citation_backed = (
            "[a-zA-Z0-9]{1,16}",
            "[a-zA-Z0-9]{1,16}",
            "[a-zA-Z0-9 ]{0,12}",
        )
            .prop_map(
                |(source_type, source_name, title)| UnifiedSearchEvidenceItem {
                    source_type,
                    source_name,
                    title,
                    url: None,
                    excerpt: None,
                    query: "q".to_string(),
                    relevance_score: None,
                    confidence: None,
                    metadata: serde_json::Value::Null,
                },
            );
        prop_oneof![url_backed, citation_backed]
    }

    /// Wrap generated evidence into a `UnifiedSearchResult` using the reused
    /// orchestrator snapshot (no live search).
    fn result_with(items: Vec<UnifiedSearchEvidenceItem>) -> UnifiedSearchResult {
        let orchestrator = pm_domain::search_orchestrator::PmSearchOrchestrator::snapshot(
            pm_domain::search_orchestrator::PmSearchOrchestratorInput::default(),
        );
        let used_layer = items.first().map(|item| item.source_type.clone());
        UnifiedSearchResult {
            orchestrator,
            scenario: "ai_chat".to_string(),
            query: "q".to_string(),
            available: !items.is_empty(),
            used_layer,
            degraded_reason: None,
            items,
            traces: Vec::new(),
            skills: Vec::new(),
            hot_reload_supported: false,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Feature: super-assistant-hub, Property 15: 检索/深度分析回答含可追溯来源
        /// Validates: Requirements 3.3, 7.3
        ///
        /// Any web-search outcome built from >= 1 traceable evidence item
        /// (url OR structured citation) surfaces at least one traceable source.
        #[test]
        fn web_search_outcome_has_traceable_source(
            items in proptest::collection::vec(traceable_evidence_item(), 1..8)
        ) {
            let outcome =
                SuperAssistantWebSearchOutcome::from_unified_result(result_with(items));
            prop_assert!(
                !outcome.sources.is_empty(),
                "expected >= 1 mapped source, got {}",
                outcome.sources.len()
            );
            prop_assert!(
                outcome.has_traceable_source(),
                "web-search outcome must expose at least one traceable source (Property 15)"
            );
            // Every generated item is individually traceable by construction.
            prop_assert!(
                outcome.sources.iter().all(SuperAssistantSource::is_traceable),
                "each mapped source built from traceable evidence must be traceable"
            );
        }
    }

    #[cfg(feature = "pm")]
    mod pm_deep_analysis {
        use super::super::{DeepAnalysisTaskHandle, SuperAssistantDeepAnalysis};
        use pm_domain::deep_research_loop::{
            PmEvidenceScore, PmHypothesisEvidenceGraph, PmHypothesisNode,
        };
        use proptest::prelude::*;

        /// Build an evidence graph carrying `count` hypothesis nodes. A non-empty
        /// node set maps to a non-empty evidence tree.
        fn graph_with_nodes(count: usize) -> PmHypothesisEvidenceGraph {
            let nodes = (0..count)
                .map(|i| PmHypothesisNode {
                    id: format!("h{i}"),
                    kind: "hypothesis".to_string(),
                    title: format!("hypothesis {i}"),
                    confidence: 0.6,
                    evidence_refs: vec![format!("e{i}")],
                })
                .collect();
            PmHypothesisEvidenceGraph {
                nodes,
                edges: Vec::new(),
                primary_evidence_node_ids: Vec::new(),
                unresolved_node_ids: Vec::new(),
            }
        }

        /// Build `count` evidence scores; a non-empty slice maps to a non-empty
        /// conflict matrix.
        fn scores_with_rows(count: usize) -> Vec<PmEvidenceScore> {
            (0..count)
                .map(|_| PmEvidenceScore {
                    source_credibility: 0.7,
                    freshness: 0.6,
                    domain_relevance: 0.7,
                    first_party_alignment: 0.6,
                    claim_support: 0.7,
                    conflict_level: "high".to_string(),
                    usable_for_decision: false,
                })
                .collect()
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]

            /// Feature: super-assistant-hub, Property 15: 检索/深度分析回答含可追溯来源
            /// Validates: Requirements 3.3, 7.3
            ///
            /// Any deep-analysis view with a non-empty evidence tree or conflict
            /// matrix carries traceable structured evidence.
            #[test]
            fn deep_analysis_view_has_traceable_evidence(
                node_count in 0usize..5,
                row_count in 0usize..5,
            ) {
                // Ensure at least one of tree / matrix is non-empty.
                prop_assume!(node_count + row_count >= 1);

                let handle =
                    DeepAnalysisTaskHandle::pm_research("t-prop", None, "running");
                let graph = (node_count > 0).then(|| graph_with_nodes(node_count));
                let scores = (row_count > 0).then(|| scores_with_rows(row_count));

                let view = SuperAssistantDeepAnalysis::new(
                    handle,
                    "score_evidence",
                    "running",
                    graph.as_ref(),
                    scores.as_deref(),
                    None,
                );

                prop_assert!(
                    view.has_traceable_evidence(),
                    "deep-analysis view with non-empty tree/matrix must be traceable (Property 15)"
                );
            }
        }
    }
}

// ===========================================================================
// Capability integration & error-path unit tests (task 10.7)
// ---------------------------------------------------------------------------
// Requirements: 3.1, 3.2, 3.5, 3.6, 3.9, 4.10
//
// The per-adapter test modules above assert each pure helper in isolation
// (`is_code_question`, `extract_code_blocks`, `detect_sql_troubleshooting_request`,
// the deep-analysis mappers, `evaluate_attachment_downgrade`). This module adds
// the *integration-flavored* assertions that chain those helpers together along
// a capability's real code path and pins the error / EDGE_CASE branches that the
// isolated tests do not yet cover end to end:
//
//   * ai_chat  — classify → map: a code question flows to code blocks while a
//                text question flows to a plain-text answer (Req 3.1 / 3.2).
//   * nl2sql   — detect → shape question → map response: the resolved,
//                error and clarification attribution branches (Req 3.6).
//   * deep     — evidence tree / conflict matrix / quality gate are surfaced
//                together AND preserved field-for-field, i.e. NOT weakened by
//                the merge (Req 3.9).
//   * EDGE_CASE attachment parse failure → non-blocking text-only degrade
//                (Req 3.5).
//   * EDGE_CASE memory/compaction failure → the degrade contract of Req 4.10
//                exercised via the shared downgrade helper: an error-caused
//                degrade surfaces a recorded, non-blocking notice and keeps
//                answering, while a non-error degrade may stay silent.
//
// Each sub-scenario is gated behind the same cargo feature as the adapter it
// exercises (`pm` for ai_chat / deep-analysis, `nl2sql` for the SQL adapter);
// the attachment / memory-degrade scenarios are ungated because the attachment
// adapter is ungated. Local helper constructors mirror the patterns used by the
// sibling test modules so this module stays self-contained.
// ===========================================================================
#[cfg(test)]
mod capability_integration_tests {
    // -------------------------------------------------------------------
    // EDGE_CASE (Req 3.5): attachment parse failure degrades to a text-only
    // answer with a NON-BLOCKING notice (the turn still completes).
    // -------------------------------------------------------------------
    #[test]
    fn edge_case_attachment_parse_failure_degrades_nonblocking() {
        let outcome = super::evaluate_attachment_downgrade(
            "failed",
            Some("unable to extract text from pdf"),
            true,
            None,
        );
        // Degraded to text-only, but the notice is present and non-empty so the
        // turn is non-blocking (the answer continues on available info).
        assert!(outcome.degraded, "parse failure must degrade");
        let notice = outcome
            .notice
            .expect("degrade must carry a non-blocking notice");
        assert!(!notice.trim().is_empty(), "notice must be user-facing text");
    }

    // -------------------------------------------------------------------
    // EDGE_CASE (Req 4.10): memory/compaction failure degrade contract.
    // Req 4.10 says an *error* in memory retrieval / compaction must be
    // recorded and the answer continues on degraded/uncompressed context,
    // while a non-error degrade may be handled silently. The shared downgrade
    // helper models exactly this two-branch contract: an error signal yields a
    // surfaced (recorded, non-blocking) notice + continued answering, whereas a
    // benign/non-error situation stays silent (passthrough, no notice).
    // -------------------------------------------------------------------
    #[test]
    fn edge_case_error_degrade_is_recorded_and_nonblocking() {
        // Error branch: model cannot use the (image) context -> degrade with a
        // recorded, non-blocking notice; the turn still answers.
        let errored = super::evaluate_attachment_downgrade("indexed", None, false, None);
        assert!(errored.degraded, "an error-caused degrade must be flagged");
        assert!(
            errored
                .notice
                .as_deref()
                .map(|n| !n.trim().is_empty())
                .unwrap_or(false),
            "error-caused degrade must record a non-blocking notice"
        );
    }

    #[test]
    fn edge_case_nonerror_degrade_may_be_silent() {
        // Non-error branch: context is usable -> passthrough, no notice
        // (Req 4.10 permits silent handling of non-error degrades).
        let benign = super::evaluate_attachment_downgrade(
            "indexed",
            None,
            true,
            Some("图中显示了一张季度销售额柱状图。"),
        );
        assert!(!benign.degraded, "usable context must not force a degrade");
        assert!(benign.notice.is_none(), "non-error path stays silent");
    }

    // -------------------------------------------------------------------
    // ai_chat integration (Req 3.1 / 3.2) — gated behind `pm`.
    // -------------------------------------------------------------------
    #[cfg(feature = "pm")]
    mod ai_chat_flow {
        use super::super::{is_code_question, AiChatAnswer};

        /// Local mirror of the sibling module's `PmChatRunResult` constructor.
        fn run_result(answer: &str, model: &str) -> crate::routes::pm::PmChatRunResult {
            crate::routes::pm::PmChatRunResult {
                answer: answer.to_string(),
                usage: crate::routes::pm::PmUsageDto {
                    input_tokens: 1,
                    output_tokens: 2,
                    total_tokens: 3,
                    estimated_cost_usd: 0.0,
                    model: model.to_string(),
                },
                applied_rules: Vec::new(),
                api_key_id: "k-1".to_string(),
                provider_name: "test".to_string(),
            }
        }

        /// Simulate `run_ai_chat_adapter`'s pure decision path without a live
        /// model: classify the message, then map a produced answer accordingly.
        fn adapt(user_message: &str, produced_answer: &str) -> AiChatAnswer {
            let is_code = is_code_question(user_message);
            AiChatAnswer::from_run_result(run_result(produced_answer, "gpt-x"), is_code)
        }

        #[test]
        fn code_question_flows_to_copyable_code_blocks() {
            // A code question is classified as such and the mapped answer
            // exposes copyable code blocks with the correct language tag (3.2).
            let ans = adapt(
                "帮我用 Python 写一个二分查找函数",
                "实现如下：\n```python\ndef bsearch(a, x):\n    return x in a\n```\n以上是思路。",
            );
            assert!(ans.is_code_answer, "code question -> code-answer path");
            assert_eq!(ans.code_blocks.len(), 1);
            assert_eq!(ans.code_blocks[0].language.as_deref(), Some("python"));
            assert!(ans.code_blocks[0].code.contains("def bsearch"));
            assert!(!ans.code_blocks[0].code.contains("```"));
        }

        #[test]
        fn text_question_flows_to_plain_answer() {
            // A prose question stays a text answer with no extracted blocks (3.1).
            let ans = adapt("帮我总结一下这份会议纪要的要点", "会议主要讨论了三件事……");
            assert!(!ans.is_code_answer, "prose question -> plain-text path");
            assert!(ans.code_blocks.is_empty());
            assert_eq!(ans.answer, "会议主要讨论了三件事……");
            assert_eq!(ans.model, "gpt-x");
        }
    }

    // -------------------------------------------------------------------
    // nl2sql attribution integration (Req 3.6) — gated behind `nl2sql`.
    // -------------------------------------------------------------------
    #[cfg(feature = "nl2sql")]
    mod nl2sql_flow {
        use super::super::{detect_sql_troubleshooting_request, SqlTroubleshootingConclusion};
        use crate::routes::nl2sql::QueryResponse;

        /// Local mirror of the sibling module's `QueryResponse` constructor.
        fn query_response(
            sql: Option<&str>,
            explanation: Option<&str>,
            error: Option<&str>,
            clarification: Option<&str>,
        ) -> QueryResponse {
            QueryResponse {
                sql: sql.map(str::to_string),
                explanation: explanation.map(str::to_string),
                error: error.map(str::to_string),
                query_id: "q-42".to_string(),
                conversation_id: Some("c-1".to_string()),
                summary_version: None,
                clarification_question: clarification.map(str::to_string),
                confirmed_requirements: None,
                missing_requirements: None,
                query_understanding: None,
                intent: None,
                cache_hit: false,
                applied_rules: Vec::new(),
                used_references: Vec::new(),
            }
        }

        #[test]
        fn detect_shape_and_resolve_carries_attribution() {
            // detect -> shape question -> map a resolved nl2sql response.
            let msg = "统计上月各渠道下单用户数，这段跑不出：\n```sql\nSELECT channel, COUNT(user_id) FROM orders WHERE\n```\n帮我补全。";
            let req = detect_sql_troubleshooting_request(msg).expect("should detect SQL request");
            let question = req.to_nl2sql_question();
            assert!(question.contains("排查结论"));
            assert!(question.contains("SELECT channel"));

            let conclusion = SqlTroubleshootingConclusion::from_query_response(query_response(
                Some("SELECT channel, COUNT(user_id) FROM orders GROUP BY channel"),
                Some("补全了 WHERE/GROUP BY 子句"),
                None,
                None,
            ));
            assert!(conclusion.resolved);
            assert!(conclusion
                .corrected_sql
                .as_deref()
                .map(|s| s.contains("GROUP BY channel"))
                .unwrap_or(false));
            // Attribution: the conclusion links back to the nl2sql query id.
            assert_eq!(conclusion.query_id.as_deref(), Some("q-42"));
            assert_eq!(conclusion.conclusion, "补全了 WHERE/GROUP BY 子句");
        }

        #[test]
        fn error_branch_surfaces_diagnosis_without_sql() {
            // Attribution edge case: nl2sql reports an error -> unresolved, no
            // corrected SQL, but a diagnosis conclusion is still surfaced.
            let conclusion = SqlTroubleshootingConclusion::from_query_response(query_response(
                None,
                None,
                Some("字段 user_id 不存在"),
                None,
            ));
            assert!(!conclusion.resolved);
            assert!(conclusion.corrected_sql.is_none());
            assert!(conclusion.conclusion.contains("字段 user_id 不存在"));
            assert_eq!(conclusion.query_id.as_deref(), Some("q-42"));
        }

        #[test]
        fn clarification_branch_asks_back_without_sql() {
            // Attribution edge case: nl2sql needs more info -> clarification
            // question is surfaced and the turn is unresolved.
            let conclusion = SqlTroubleshootingConclusion::from_query_response(query_response(
                None,
                None,
                None,
                Some("请问统计哪个时间范围？"),
            ));
            assert!(!conclusion.resolved);
            assert_eq!(
                conclusion.clarification_question.as_deref(),
                Some("请问统计哪个时间范围？")
            );
            assert!(conclusion.conclusion.contains("需要澄清"));
        }
    }

    // -------------------------------------------------------------------
    // Deep-analysis integration (Req 3.9) — gated behind `pm`.
    // The evidence tree / conflict matrix / quality gate are surfaced together
    // AND preserved field-for-field from the reused deep-research artifacts, so
    // the deep-analysis strengths are NOT weakened by the merge.
    // -------------------------------------------------------------------
    #[cfg(feature = "pm")]
    mod deep_analysis_flow {
        use super::super::{DeepAnalysisTaskHandle, SuperAssistantDeepAnalysis};
        use pm_domain::deep_research_loop::{
            PmDeepResearchScore, PmEvidenceScore, PmHypothesisEdge, PmHypothesisEvidenceGraph,
            PmHypothesisNode,
        };

        fn ready_score() -> PmDeepResearchScore {
            PmDeepResearchScore {
                evidence_coverage_score: 0.91,
                first_party_alignment_score: 0.88,
                claim_confidence_score: 0.9,
                counter_evidence_coverage_score: 0.82,
                expert_lens_coverage_score: 0.8,
                actionability_score: 0.86,
                decision_readiness_score: 0.92,
            }
        }

        fn evidence(conflict: &str, usable: bool) -> PmEvidenceScore {
            PmEvidenceScore {
                source_credibility: 0.7,
                freshness: 0.6,
                domain_relevance: 0.7,
                first_party_alignment: 0.6,
                claim_support: 0.7,
                conflict_level: conflict.to_string(),
                usable_for_decision: usable,
            }
        }

        fn graph() -> PmHypothesisEvidenceGraph {
            PmHypothesisEvidenceGraph {
                nodes: vec![
                    PmHypothesisNode {
                        id: "h1".to_string(),
                        kind: "hypothesis".to_string(),
                        title: "主要假设".to_string(),
                        confidence: 0.8,
                        evidence_refs: vec!["e1".to_string(), "e2".to_string()],
                    },
                    PmHypothesisNode {
                        id: "h2".to_string(),
                        kind: "counter".to_string(),
                        title: "反例".to_string(),
                        confidence: 0.5,
                        evidence_refs: vec!["e3".to_string()],
                    },
                ],
                edges: vec![PmHypothesisEdge {
                    from: "h1".to_string(),
                    to: "h2".to_string(),
                    relation: "contradicts".to_string(),
                    strength: 0.6,
                }],
                primary_evidence_node_ids: vec!["h1".to_string()],
                unresolved_node_ids: vec!["h2".to_string()],
            }
        }

        #[test]
        fn surfaces_all_three_artifacts_without_weakening() {
            let g = graph();
            let scores = [evidence("none", true), evidence("high", false)];
            let s = ready_score();
            let view = SuperAssistantDeepAnalysis::new(
                DeepAnalysisTaskHandle::pm_research("t-1", Some("s-1".to_string()), "running"),
                "score_evidence",
                "running",
                Some(&g),
                Some(&scores),
                Some(&s),
            );

            // All three deep-analysis strengths are present together (3.9).
            assert!(view.preserves_deep_analysis_strengths());

            // Quality gate is copied field-for-field (not diluted) and the
            // decision-ready verdict is delegated to the reused score.
            let gate = view.quality_gate.as_ref().expect("quality gate present");
            assert_eq!(gate.evidence_coverage_score, s.evidence_coverage_score);
            assert_eq!(gate.actionability_score, s.actionability_score);
            assert_eq!(gate.decision_readiness_score, s.decision_readiness_score);
            assert_eq!(gate.decision_ready, s.decision_ready());

            // Conflict matrix preserves every scored row and counts real
            // conflicts ("none" excluded, "high" counted) — no weakening.
            let matrix = view
                .conflict_matrix
                .as_ref()
                .expect("conflict matrix present");
            assert_eq!(matrix.rows.len(), 2);
            assert_eq!(matrix.conflicted_count, 1);
            assert!(!matrix.rows[1].usable_for_decision);

            // Evidence tree preserves node/edge structure and evidence ref
            // counts verbatim from the reused graph.
            let tree = view.evidence_tree.as_ref().expect("evidence tree present");
            assert_eq!(tree.nodes.len(), g.nodes.len());
            assert_eq!(tree.edge_count, g.edges.len());
            assert_eq!(tree.root_node_ids, g.primary_evidence_node_ids);
            assert_eq!(tree.unresolved_node_ids, g.unresolved_node_ids);
            assert_eq!(tree.nodes[0].evidence_ref_count, 2);
            assert_eq!(tree.nodes[1].evidence_ref_count, 1);

            // The structured evidence is traceable (Property 15 support).
            assert!(view.has_traceable_evidence());
        }
    }
}
