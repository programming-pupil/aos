use super::*;
// Task 20.3: `pm-memories` is a thin view/wrapper over the single source of
// truth (`/memory/items` in `memory_continuity`). PM session-memory reads and
// writes converge onto the unified `agent_memory_items` store (app=pm,
// scope=session) instead of the parallel `pm_session_memories` table.
use crate::routes::memory_continuity::{
    create_memory_item_internal, delete_memory_item_internal, list_legacy_memory_items,
    list_unified_memory_items_lightweight, update_memory_item_internal, AgentMemoryItem,
    MemoryPatchRequest, MemoryUpsertRequest,
};

const PM_MEMORY_INJECTION_LIMIT: usize = 8;

/// Legacy `pm_session_memories` rows surfaced through the unified list carry
/// this id prefix (see `memory_continuity::legacy_pm_row_to_item`).
const LEGACY_PM_ID_PREFIX: &str = "legacy-pm-";

/// Map a PM memory type onto the unified `agent_memory_items` vocabulary.
fn pm_type_to_unified(pm_type: &str) -> &'static str {
    match pm_type {
        "preference" => "preference",
        "project_fact" => "project_fact",
        "business_context" => "business_context",
        // `analysis_style` / `session_summary` have no unified equivalent; keep
        // the original in metadata and store as a generic note.
        _ => "note",
    }
}

/// Reconstruct the PM memory type for API responses (metadata round-trip first).
fn unified_type_to_pm(memory_type: &str) -> String {
    match memory_type {
        "preference" | "project_fact" | "business_context" | "analysis_style"
        | "session_summary" => memory_type.to_string(),
        _ => "project_fact".to_string(),
    }
}

/// Attach the original PM memory type to metadata for round-trip fidelity.
fn with_pm_type_metadata(metadata: Option<serde_json::Value>, pm_type: &str) -> serde_json::Value {
    let mut obj = match metadata {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "pmMemoryType".to_string(),
        serde_json::Value::String(pm_type.to_string()),
    );
    serde_json::Value::Object(obj)
}

/// Project a unified [`AgentMemoryItem`] back onto the legacy
/// [`PmSessionMemoryRecord`] response shape.
fn unified_item_to_pm_record(item: AgentMemoryItem, session_id: &str) -> PmSessionMemoryRecord {
    let memory_type = item
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("pmMemoryType"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| unified_type_to_pm(&item.memory_type));
    PmSessionMemoryRecord {
        id: item.id,
        session_id: item.session_id.unwrap_or_else(|| session_id.to_string()),
        memory_type,
        content: item.content,
        source: item.source_type,
        confidence: item.confidence,
        pinned: item.pinned,
        enabled: item.enabled,
        metadata: item.metadata,
        created_at: item.created_at,
        updated_at: item.updated_at,
    }
}
const PM_SESSION_SUMMARY_MAX_CHARS: usize = 12_000;
const PM_MEMORY_MAX_CONTENT_CHARS: usize = 600;
const PM_RECENT_HISTORY_SCAN_LIMIT: i64 = 24;
const PM_RECENT_HISTORY_EARLIEST_LIMIT: i64 = 4;
const PM_RECENT_HISTORY_DEFAULT_TOTAL_CHARS: usize = 18_000;
const PM_RECENT_HISTORY_MAX_TOTAL_CHARS: usize = 40_000;
const PM_RECENT_HISTORY_MAX_ITEM_CHARS: usize = 7_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmSessionMemoryRecord {
    pub id: String,
    pub session_id: String,
    pub memory_type: String,
    pub content: String,
    pub source: String,
    pub confidence: f64,
    pub pinned: bool,
    pub enabled: bool,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmSessionMemoryUpsertRequest {
    pub content: String,
    pub memory_type: Option<String>,
    pub pinned: Option<bool>,
    pub enabled: Option<bool>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmSessionMemoryPauseRequest {
    pub paused: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PmSessionMemoryListResponse {
    items: Vec<PmSessionMemoryRecord>,
    paused: bool,
    summary: Option<PmSessionSummaryRecord>,
    default_mode: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PmSessionSummaryRecord {
    summary: String,
    turn_count: i32,
    source_task_id: Option<String>,
    last_compacted_removed_messages: Option<i32>,
    metadata: Option<serde_json::Value>,
    updated_at: String,
}

fn pm_memory_metadata(raw: Option<String>) -> Option<serde_json::Value> {
    raw.and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
}

fn pm_memory_row_to_record(row: &sqlx::sqlite::SqliteRow) -> PmSessionMemoryRecord {
    PmSessionMemoryRecord {
        id: row.get("id"),
        session_id: row.get("session_id"),
        memory_type: row.get("memory_type"),
        content: row.get("content"),
        source: row.get("source"),
        confidence: row
            .try_get::<f64, _>("confidence")
            .or_else(|_| {
                row.try_get::<String, _>("confidence")
                    .map(|value| value.parse().unwrap_or(1.0))
            })
            .unwrap_or(1.0),
        pinned: row.get::<i8, _>("pinned") != 0,
        enabled: row.get::<i8, _>("enabled") != 0,
        metadata: pm_memory_metadata(row.get("metadata_json")),
        created_at: row
            .get::<chrono::NaiveDateTime, _>("created_at")
            .to_string(),
        updated_at: row
            .get::<chrono::NaiveDateTime, _>("updated_at")
            .to_string(),
    }
}

fn pm_summary_row_to_record(row: &sqlx::sqlite::SqliteRow) -> PmSessionSummaryRecord {
    PmSessionSummaryRecord {
        summary: row.get("summary"),
        turn_count: row.get("turn_count"),
        source_task_id: row.get("source_task_id"),
        last_compacted_removed_messages: row.get("last_compacted_removed_messages"),
        metadata: pm_memory_metadata(row.get("metadata_json")),
        updated_at: row
            .get::<chrono::NaiveDateTime, _>("updated_at")
            .to_string(),
    }
}

fn normalize_pm_memory_type(raw: Option<&str>) -> String {
    match raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("project_fact")
    {
        "preference" | "project_fact" | "business_context" | "analysis_style"
        | "session_summary" => raw.unwrap_or("project_fact").trim().to_string(),
        _ => "project_fact".to_string(),
    }
}

fn pm_memory_is_sensitive(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    let sensitive_markers = [
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "token=",
        "bearer ",
        "private key",
        "ssh-rsa",
        "-----begin",
        "密钥",
        "密码",
        "口令",
        "身份证",
        "银行卡",
    ];
    sensitive_markers
        .iter()
        .any(|marker| lower.contains(marker) || content.contains(marker))
}

fn compact_pm_memory_text(input: &str, max_chars: usize) -> String {
    let mut out = input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect::<String>();
    }
    out
}

fn tokenize_pm_memory_query(input: &str) -> Vec<String> {
    let mut tokens = input
        .split(|ch: char| {
            !ch.is_alphanumeric()
                && ch != '_'
                && ch != '-'
                && !('\u{4e00}'..='\u{9fff}').contains(&ch)
        })
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .filter(|token| token.chars().count() >= 2)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn score_pm_memory(tokens: &[String], content: &str) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let lower = content.to_ascii_lowercase();
    let hits = tokens
        .iter()
        .filter(|token| lower.contains(token.as_str()))
        .count();
    (hits as f64 / tokens.len().max(1) as f64).min(1.0)
}

fn pm_recent_history_total_char_budget() -> usize {
    std::env::var("PM_RECENT_HISTORY_CONTEXT_CHARS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(PM_RECENT_HISTORY_DEFAULT_TOTAL_CHARS)
        .min(PM_RECENT_HISTORY_MAX_TOTAL_CHARS)
}

fn pm_recent_history_query_indicates_reference(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    let compact: String = trimmed.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.chars().count() <= 4 {
        let followup_markers = [
            "?",
            "？",
            "继续",
            "接着",
            "展开",
            "然后",
            "呢",
            "为什么",
            "咋办",
            "怎么改",
            "改下",
            "再说",
            "more",
            "why",
            "how",
        ];
        return followup_markers
            .iter()
            .any(|marker| lower.contains(marker) || compact.contains(marker));
    }
    let markers = [
        "之前",
        "刚才",
        "上次",
        "前面",
        "第一次",
        "历史",
        "记得",
        "还记得",
        "我发",
        "我给",
        "那段",
        "这段",
        "附件",
        "文件",
        "表格",
        "数据",
        "字段",
        "表结构",
        "sql",
        "csv",
        "markdown",
        "md",
        "previous",
        "earlier",
        "last time",
        "first message",
        "remember",
        "sent you",
        "the sql",
        "schema",
        "table",
        "file",
        "attachment",
        "dataset",
    ];
    if markers
        .iter()
        .any(|marker| lower.contains(marker) || trimmed.contains(marker))
    {
        return true;
    }
    false
}

fn pm_recent_history_text_has_structured_payload(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    if lower.contains("create table")
        || lower.contains("select ")
        || lower.contains("with ")
        || lower.contains("insert into")
        || lower.contains("group by")
        || lower.contains("order by")
        || lower.contains("```")
    {
        return true;
    }
    let lines = input.lines().take(24).collect::<Vec<_>>();
    let comma_rows = lines
        .iter()
        .filter(|line| line.split(',').count() >= 4)
        .count();
    let pipe_rows = lines
        .iter()
        .filter(|line| line.matches('|').count() >= 3)
        .count();
    comma_rows >= 2 || pipe_rows >= 2
}

fn pm_recent_history_is_useful_message(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() || pm_memory_is_sensitive(trimmed) {
        return false;
    }
    let char_count = trimmed.chars().count();
    char_count >= 80 || pm_recent_history_text_has_structured_payload(trimmed)
}

fn pm_recent_history_excerpt(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let head_budget = max_chars.saturating_mul(3) / 5;
    let tail_budget = max_chars.saturating_sub(head_budget).saturating_sub(80);
    let head = trimmed.chars().take(head_budget).collect::<String>();
    let tail_vec = trimmed.chars().rev().take(tail_budget).collect::<Vec<_>>();
    let tail = tail_vec.into_iter().rev().collect::<String>();
    format!("{head}\n\n...[middle omitted for context budget]...\n\n{tail}")
}

#[derive(Debug, Clone)]
struct PmRecentHistorySnippet {
    source_id: String,
    source_kind: String,
    position: String,
    role: String,
    score: f64,
    content: String,
}

fn pm_recent_history_score(
    query_tokens: &[String],
    query_references_history: bool,
    position_score: f64,
    content: &str,
) -> f64 {
    let lexical = score_pm_memory(query_tokens, content);
    let structured_bonus = if pm_recent_history_text_has_structured_payload(content) {
        0.22
    } else {
        0.0
    };
    let reference_bonus = if query_references_history { 0.18 } else { 0.0 };
    (lexical * 0.58 + position_score * 0.22 + structured_bonus + reference_bonus).min(1.0)
}

fn push_pm_recent_history_snippet(
    snippets: &mut Vec<PmRecentHistorySnippet>,
    query_tokens: &[String],
    query_references_history: bool,
    source_id: String,
    source_kind: &str,
    position: String,
    role: &str,
    position_score: f64,
    content: &str,
) {
    if !pm_recent_history_is_useful_message(content) {
        return;
    }
    let score = pm_recent_history_score(
        query_tokens,
        query_references_history,
        position_score,
        content,
    );
    if !query_references_history && score < 0.18 {
        return;
    }
    let excerpt = pm_recent_history_excerpt(content, PM_RECENT_HISTORY_MAX_ITEM_CHARS);
    snippets.push(PmRecentHistorySnippet {
        source_id,
        source_kind: source_kind.to_string(),
        position,
        role: role.to_string(),
        score,
        content: excerpt,
    });
}

async fn load_pm_recent_task_message_snippets(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    user_message: &str,
) -> Vec<PmRecentHistorySnippet> {
    let query_tokens = tokenize_pm_memory_query(user_message);
    let query_references_history = pm_recent_history_query_indicates_reference(user_message);
    let mut snippets = Vec::new();

    let recent_rows = sqlx::query(
        "SELECT task_id, message, created_at
         FROM pm_research_tasks
         WHERE tenant_id = ? AND user_id = ? AND session_id = ?
         ORDER BY created_at DESC, updated_at DESC, task_id DESC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(PM_RECENT_HISTORY_SCAN_LIMIT)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for (idx, row) in recent_rows.iter().enumerate() {
        let message = row.try_get::<String, _>("message").unwrap_or_default();
        if message.trim() == user_message.trim() {
            continue;
        }
        let task_id = row.try_get::<String, _>("task_id").unwrap_or_default();
        let created_at = row
            .try_get::<chrono::NaiveDateTime, _>("created_at")
            .map(|value| value.to_string())
            .unwrap_or_default();
        let position_score = 1.0 - (idx as f64 / PM_RECENT_HISTORY_SCAN_LIMIT.max(1) as f64);
        push_pm_recent_history_snippet(
            &mut snippets,
            &query_tokens,
            query_references_history,
            task_id,
            "pm_research_task",
            format!("recent-{} · {}", idx + 1, created_at),
            "user",
            position_score.max(0.08),
            &message,
        );
    }

    if query_references_history {
        let earliest_rows = sqlx::query(
            "SELECT task_id, message, created_at
             FROM pm_research_tasks
             WHERE tenant_id = ? AND user_id = ? AND session_id = ?
             ORDER BY created_at ASC, updated_at ASC, task_id ASC
             LIMIT ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(PM_RECENT_HISTORY_EARLIEST_LIMIT)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        for (idx, row) in earliest_rows.iter().enumerate() {
            let message = row.try_get::<String, _>("message").unwrap_or_default();
            if message.trim() == user_message.trim() {
                continue;
            }
            let task_id = row.try_get::<String, _>("task_id").unwrap_or_default();
            let created_at = row
                .try_get::<chrono::NaiveDateTime, _>("created_at")
                .map(|value| value.to_string())
                .unwrap_or_default();
            push_pm_recent_history_snippet(
                &mut snippets,
                &query_tokens,
                true,
                task_id,
                "pm_research_task",
                format!("earliest-{} · {}", idx + 1, created_at),
                "user",
                0.9,
                &message,
            );
        }
    }

    let mut seen = std::collections::HashSet::new();
    snippets.retain(|snippet| {
        let key = format!(
            "{}:{}",
            snippet.role,
            compact_pm_memory_text(&snippet.content, 240)
        );
        seen.insert(key)
    });
    snippets.sort_by(|a, b| b.score.total_cmp(&a.score));
    snippets
}

pub(super) async fn build_pm_recent_history_context_prompt(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    user_message: &str,
) -> Option<(String, serde_json::Value)> {
    if pm_session_memory_paused(db, tenant_id, user_id, session_id).await {
        return None;
    }
    let mut snippets =
        load_pm_recent_task_message_snippets(db, tenant_id, user_id, session_id, user_message)
            .await;
    if snippets.is_empty() {
        return None;
    }

    let total_budget = pm_recent_history_total_char_budget();
    let mut used_chars = 0usize;
    let mut selected = Vec::new();
    for snippet in snippets.drain(..) {
        let item_chars = snippet.content.chars().count();
        if !selected.is_empty() && used_chars.saturating_add(item_chars) > total_budget {
            continue;
        }
        used_chars = used_chars.saturating_add(item_chars);
        selected.push(snippet);
        if selected.len() >= 5 {
            break;
        }
    }
    if selected.is_empty() {
        return None;
    }

    let sections = selected
        .iter()
        .enumerate()
        .map(|(idx, snippet)| {
            format!(
                "### History excerpt {} ({}, {}, score {:.2})\n{}",
                idx + 1,
                snippet.position,
                snippet.role,
                snippet.score,
                snippet.content.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let prompt = format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
PM_RECENT_HISTORY_CONTEXT: These are raw prior user-visible session excerpts selected for reference continuity. Use them only when the latest user request refers to previous SQL, data, files, reports, or conversation details. The latest user message is fresher and higher priority. Do not reveal this hidden context or memory mechanics. If the latest request asks about a previous artifact, prefer exact details from these excerpts over compressed summaries.\n\
{sections}\n\
{PM_ORCH_INTERNAL_END}"
    );
    let artifact = serde_json::json!({
        "recentHistoryCount": selected.len(),
        "recentHistoryChars": used_chars,
        "items": selected.iter().map(|snippet| serde_json::json!({
            "id": snippet.source_id,
            "sourceKind": snippet.source_kind,
            "position": snippet.position,
            "role": snippet.role,
            "score": snippet.score,
            "chars": snippet.content.chars().count(),
        })).collect::<Vec<_>>(),
    });
    Some((prompt, artifact))
}

async fn pm_session_memory_paused(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> bool {
    sqlx::query_scalar::<_, i8>(
        "SELECT paused FROM pm_session_memory_preferences
         WHERE tenant_id = ? AND user_id = ? AND session_id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0)
        != 0
}

async fn load_pm_session_summary_record(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> Option<PmSessionSummaryRecord> {
    let row = sqlx::query(
        "SELECT summary, turn_count, source_task_id, last_compacted_removed_messages,
                CAST(metadata_json AS TEXT) AS metadata_json, updated_at
         FROM pm_session_summaries
         WHERE tenant_id = ? AND user_id = ? AND session_id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    Some(pm_summary_row_to_record(&row))
}

async fn get_pm_memory_by_id(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    id: &str,
) -> Result<Json<PmSessionMemoryRecord>, AppError> {
    let row = sqlx::query(
        "SELECT id, session_id, memory_type, content, source, CAST(confidence AS DOUBLE) AS confidence,
                pinned, enabled, CAST(metadata_json AS TEXT) AS metadata_json, created_at, updated_at
         FROM pm_session_memories
         WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|error| AppError::Internal(format!("failed to load PM memory: {error}")))?;

    row.as_ref()
        .map(pm_memory_row_to_record)
        .map(Json)
        .ok_or_else(|| AppError::NotFound("PM memory not found".to_string()))
}

fn validate_pm_session_owner(
    handle: &SessionHandle,
    claims: &Claims,
    session_id: &str,
) -> Result<(), AppError> {
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    if !handle.source.eq_ignore_ascii_case("pm") {
        return Err(AppError::ValidationError(format!(
            "session {session_id} is not a PM session"
        )));
    }
    Ok(())
}

async fn require_pm_session(
    state: &AppState,
    claims: &Claims,
    session_id: &str,
) -> Result<SessionHandle, AppError> {
    let Some(handle) = get_agent_manager(state).get_session(session_id).await else {
        return Err(AppError::NotFound(format!(
            "session {session_id} not found"
        )));
    };
    validate_pm_session_owner(&handle, claims, session_id)?;
    Ok(handle)
}

pub(super) async fn list_pm_session_memories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    if let Err(error) = require_pm_session(&state, &claims, &session_id).await {
        return error.into_response();
    }

    // Read from the single source of truth (`agent_memory_items`, app=pm,
    // scope=session) and fold in legacy `pm_session_memories` rows as a
    // compatibility view, matching `/memory/items` retrieval semantics.
    let mut items = match list_unified_memory_items_lightweight(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        Some("session"),
        Some("pm"),
        Some(&session_id),
        true,
        200,
    )
    .await
    {
        Ok(items) => items,
        Err(error) => return error.into_response(),
    };
    items.extend(
        list_legacy_memory_items(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            Some("pm"),
            Some(&session_id),
            200,
        )
        .await
        .unwrap_or_default(),
    );
    items.truncate(200);
    let paused =
        pm_session_memory_paused(&state.db, &claims.tenant_id, &claims.sub, &session_id).await;
    let summary =
        load_pm_session_summary_record(&state.db, &claims.tenant_id, &claims.sub, &session_id)
            .await;
    Json(PmSessionMemoryListResponse {
        items: items
            .into_iter()
            .map(|item| unified_item_to_pm_record(item, &session_id))
            .collect(),
        paused,
        summary,
        default_mode: "auto",
    })
    .into_response()
}

pub(super) async fn create_pm_session_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    Json(req): Json<PmSessionMemoryUpsertRequest>,
) -> impl IntoResponse {
    if let Err(error) = require_pm_session(&state, &claims, &session_id).await {
        return error.into_response();
    }
    let content = compact_pm_memory_text(&req.content, PM_MEMORY_MAX_CONTENT_CHARS);
    if content.is_empty() {
        return AppError::ValidationError("memory content cannot be empty".to_string())
            .into_response();
    }
    if pm_memory_is_sensitive(&content) {
        return AppError::ValidationError(
            "memory looks sensitive; secrets and credentials are not stored".to_string(),
        )
        .into_response();
    }
    let pm_type = normalize_pm_memory_type(req.memory_type.as_deref());
    // Write through the single source of truth. Dedup, embeddings and
    // sensitivity screening are handled by `create_memory_item_internal`.
    let upsert = MemoryUpsertRequest {
        scope: Some("session".to_string()),
        app: Some("pm".to_string()),
        session_id: Some(session_id.clone()),
        memory_type: Some(pm_type_to_unified(&pm_type).to_string()),
        content,
        source_type: Some("manual".to_string()),
        confidence: Some(1.0),
        pinned: req.pinned,
        enabled: req.enabled,
        stale_at: None,
        verified_at: None,
        metadata: Some(with_pm_type_metadata(req.metadata, &pm_type)),
    };
    match create_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, upsert).await {
        Ok(item) => Json(unified_item_to_pm_record(item, &session_id)).into_response(),
        Err(error) => error.into_response(),
    }
}

pub(super) async fn update_pm_session_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((session_id, memory_id)): Path<(String, String)>,
    Json(req): Json<PmSessionMemoryUpsertRequest>,
) -> impl IntoResponse {
    if let Err(error) = require_pm_session(&state, &claims, &session_id).await {
        return error.into_response();
    }
    let content = compact_pm_memory_text(&req.content, PM_MEMORY_MAX_CONTENT_CHARS);
    if content.is_empty() {
        return AppError::ValidationError("memory content cannot be empty".to_string())
            .into_response();
    }
    if pm_memory_is_sensitive(&content) {
        return AppError::ValidationError(
            "memory looks sensitive; secrets and credentials are not stored".to_string(),
        )
        .into_response();
    }
    // Backward-compat: pre-migration `pm_session_memories` rows are surfaced
    // through the unified list with a `legacy-pm-` id prefix; edits target the
    // legacy table. New rows live only in the unified store.
    if let Some(legacy_id) = memory_id.strip_prefix(LEGACY_PM_ID_PREFIX) {
        return match update_legacy_pm_memory(
            &state,
            &claims,
            &session_id,
            legacy_id,
            &req,
            &content,
        )
        .await
        {
            Ok(value) => value.into_response(),
            Err(error) => error.into_response(),
        };
    }
    let pm_type = normalize_pm_memory_type(req.memory_type.as_deref());
    let patch = MemoryPatchRequest {
        scope: Some("session".to_string()),
        app: Some("pm".to_string()),
        session_id: Some(session_id.clone()),
        memory_type: Some(pm_type_to_unified(&pm_type).to_string()),
        content: Some(content),
        source_type: None,
        confidence: None,
        pinned: req.pinned,
        enabled: req.enabled,
        stale_at: None,
        verified_at: None,
        metadata: Some(with_pm_type_metadata(req.metadata, &pm_type)),
    };
    match update_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, &memory_id, patch)
        .await
    {
        Ok(item) => Json(unified_item_to_pm_record(item, &session_id)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// Legacy-row edit shim: update a pre-migration `pm_session_memories` row and
/// re-surface it with the `legacy-pm-` id prefix.
async fn update_legacy_pm_memory(
    state: &AppState,
    claims: &Claims,
    session_id: &str,
    legacy_id: &str,
    req: &PmSessionMemoryUpsertRequest,
    content: &str,
) -> Result<Json<PmSessionMemoryRecord>, AppError> {
    let memory_type = normalize_pm_memory_type(req.memory_type.as_deref());
    let metadata_json = req
        .metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| AppError::ValidationError(format!("invalid memory metadata: {error}")))?;
    let affected = sqlx::query(
        "UPDATE pm_session_memories
         SET memory_type = ?, content = ?, pinned = ?, enabled = ?, metadata_json = json(?)
         WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND id = ?",
    )
    .bind(memory_type)
    .bind(content)
    .bind(req.pinned.unwrap_or(false))
    .bind(req.enabled.unwrap_or(true))
    .bind(metadata_json)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(session_id)
    .bind(legacy_id)
    .execute(&state.db)
    .await
    .map_err(|error| AppError::Internal(format!("failed to update PM memory: {error}")))?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("PM memory not found".to_string()));
    }
    let mut record = get_pm_memory_by_id(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        session_id,
        legacy_id,
    )
    .await?;
    record.0.id = format!("{LEGACY_PM_ID_PREFIX}{}", record.0.id);
    Ok(record)
}

pub(super) async fn delete_pm_session_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((session_id, memory_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(error) = require_pm_session(&state, &claims, &session_id).await {
        return error.into_response();
    }
    // Legacy-row delete shim (see `update_pm_session_memory`).
    if let Some(legacy_id) = memory_id.strip_prefix(LEGACY_PM_ID_PREFIX) {
        let affected = match sqlx::query(
            "DELETE FROM pm_session_memories
             WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND id = ?",
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&session_id)
        .bind(legacy_id)
        .execute(&state.db)
        .await
        {
            Ok(result) => result.rows_affected(),
            Err(error) => {
                return AppError::Internal(format!("failed to delete PM memory: {error}"))
                    .into_response()
            }
        };
        return Json(serde_json::json!({ "deleted": affected > 0 })).into_response();
    }
    match delete_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, &memory_id).await {
        Ok(deleted) => Json(serde_json::json!({ "deleted": deleted })).into_response(),
        Err(error) => error.into_response(),
    }
}

pub(super) async fn pause_pm_session_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    Json(req): Json<PmSessionMemoryPauseRequest>,
) -> impl IntoResponse {
    if let Err(error) = require_pm_session(&state, &claims, &session_id).await {
        return error.into_response();
    }
    if let Err(error) = sqlx::query(
        "INSERT INTO pm_session_memory_preferences (tenant_id, user_id, session_id, paused, paused_at)
         VALUES (?, ?, ?, ?, IIF(? = 1, CURRENT_TIMESTAMP, NULL))
         ON CONFLICT DO UPDATE SET paused = excluded.paused, paused_at = excluded.paused_at",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&session_id)
    .bind(req.paused)
    .bind(req.paused)
    .execute(&state.db)
    .await
    {
        return AppError::Internal(format!("failed to update PM memory preference: {error}"))
            .into_response();
    }
    Json(serde_json::json!({
        "paused": req.paused,
        "defaultMode": "auto",
    }))
    .into_response()
}

pub(super) async fn get_pm_session_memory_pause(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    if let Err(error) = require_pm_session(&state, &claims, &session_id).await {
        return error.into_response();
    }
    let paused =
        pm_session_memory_paused(&state.db, &claims.tenant_id, &claims.sub, &session_id).await;
    Json(serde_json::json!({
        "paused": paused,
        "defaultMode": "auto",
    }))
    .into_response()
}

pub(super) async fn build_pm_session_memory_prompt(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    user_message: &str,
) -> Option<(String, serde_json::Value)> {
    if pm_session_memory_paused(db, tenant_id, user_id, session_id).await {
        return None;
    }
    let summary = load_pm_session_summary_record(db, tenant_id, user_id, session_id).await;
    let rows = sqlx::query(
        "SELECT id, session_id, memory_type, content, source, CAST(confidence AS DOUBLE) AS confidence,
                pinned, enabled, CAST(metadata_json AS TEXT) AS metadata_json, created_at, updated_at
         FROM pm_session_memories
         WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND enabled = 1
         ORDER BY pinned DESC, updated_at DESC
         LIMIT 80",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .fetch_all(db)
    .await
    .ok()?;

    let tokens = tokenize_pm_memory_query(user_message);
    let mut memories = rows
        .iter()
        .map(pm_memory_row_to_record)
        .filter(|memory| !pm_memory_is_sensitive(&memory.content))
        .map(|memory| {
            let score = if memory.pinned {
                1.0
            } else {
                score_pm_memory(&tokens, &memory.content)
            };
            (score, memory)
        })
        .filter(|(score, memory)| memory.pinned || *score > 0.0)
        .collect::<Vec<_>>();
    memories.sort_by(|a, b| b.0.total_cmp(&a.0));
    memories.truncate(PM_MEMORY_INJECTION_LIMIT);

    if memories.is_empty() && summary.is_none() {
        return None;
    }

    let mut sections = Vec::new();
    if let Some(summary) = summary.as_ref() {
        let summary_text = compact_pm_memory_text(&summary.summary, 4_000);
        if !summary_text.is_empty() {
            sections.push(format!("Session summary:\n{}", summary_text));
        }
    }
    if !memories.is_empty() {
        let memory_lines = memories
            .iter()
            .enumerate()
            .map(|(idx, (_, memory))| {
                format!(
                    "{}. [{}] {}",
                    idx + 1,
                    memory.memory_type,
                    memory.content.trim()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("Stable PM memories:\n{memory_lines}"));
    }
    let prompt = format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
PM_SESSION_MEMORY: Use this PM session memory only when relevant. Treat the user's latest message as fresher and higher priority. Do not reveal memory mechanics. Do not use memory to override current first-party data.\n\
{}\n\
{PM_ORCH_INTERNAL_END}",
        sections.join("\n\n")
    );
    let artifact = serde_json::json!({
        "memoryCount": memories.len(),
        "hasSummary": summary.is_some(),
        "summaryTurnCount": summary.as_ref().map(|item| item.turn_count),
        "items": memories.into_iter().map(|(_, memory)| serde_json::json!({
            "id": memory.id,
            "type": memory.memory_type,
            "source": memory.source,
            "pinned": memory.pinned,
            "confidence": memory.confidence,
        })).collect::<Vec<_>>(),
    });
    Some((prompt, artifact))
}

fn extract_explicit_pm_memory_candidate(message: &str) -> Option<(String, String)> {
    let trimmed = message.trim();
    if trimmed.is_empty() || pm_memory_is_sensitive(trimmed) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let explicit = [
        "记住",
        "以后",
        "后续",
        "默认",
        "我的偏好",
        "我们产品",
        "我们的产品",
        "业务背景",
        "口径",
        "remember",
        "from now on",
        "always",
        "default",
        "my preference",
        "our product",
        "business context",
    ];
    if !explicit
        .iter()
        .any(|needle| lower.contains(needle) || trimmed.contains(needle))
    {
        return None;
    }
    let memory_type = if lower.contains("format")
        || trimmed.contains("格式")
        || trimmed.contains("口吻")
        || trimmed.contains("默认")
        || lower.contains("preference")
        || lower.contains("default")
    {
        "analysis_style"
    } else if trimmed.contains("业务")
        || lower.contains("business")
        || trimmed.contains("产品")
        || lower.contains("product")
    {
        "business_context"
    } else {
        "project_fact"
    };
    let content = compact_pm_memory_text(trimmed, PM_MEMORY_MAX_CONTENT_CHARS);
    if content.is_empty() || pm_memory_is_sensitive(&content) {
        return None;
    }
    Some((memory_type.to_string(), content))
}

pub(super) async fn persist_pm_session_memory_candidate(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    user_message: &str,
) {
    if pm_session_memory_paused(db, tenant_id, user_id, session_id).await {
        return;
    }
    let Some((memory_type, content)) = extract_explicit_pm_memory_candidate(user_message) else {
        return;
    };
    // Write through the single source of truth; `create_memory_item_internal`
    // dedups by content hash so the manual `pm_session_memories` probe is gone.
    let metadata = with_pm_type_metadata(
        Some(serde_json::json!({
            "source": "explicit_user_instruction",
            "createdFrom": "pm_turn",
        })),
        &memory_type,
    );
    let upsert = MemoryUpsertRequest {
        scope: Some("session".to_string()),
        app: Some("pm".to_string()),
        session_id: Some(session_id.to_string()),
        memory_type: Some(pm_type_to_unified(&memory_type).to_string()),
        content,
        source_type: Some("explicit_user".to_string()),
        confidence: Some(0.92),
        pinned: Some(false),
        enabled: Some(true),
        stale_at: None,
        verified_at: None,
        metadata: Some(metadata),
    };
    if let Err(error) = create_memory_item_internal(db, tenant_id, user_id, upsert).await {
        tracing::warn!(
            tenant_id,
            user_id,
            session_id,
            error = %error,
            "failed to persist PM session memory candidate"
        );
    }
}

fn build_pm_session_summary(
    previous_summary: Option<&str>,
    question: &str,
    answer: &str,
    quality: &PmAnswerQualityDto,
) -> String {
    let mut lines = Vec::new();
    if let Some(previous) = previous_summary
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push("Previous PM session summary:".to_string());
        lines.push(compact_pm_memory_text(previous, 6_000));
    }
    lines.push("Latest PM turn:".to_string());
    lines.push(format!(
        "- User request: {}",
        compact_pm_memory_text(question, 1_200)
    ));
    let visible_answer = extract_pm_visible_answer_text(answer);
    let answer_preview = compact_pm_memory_text(&visible_answer, 3_600);
    if !answer_preview.is_empty() {
        lines.push(format!("- Assistant answer: {answer_preview}"));
    }
    lines.push(format!(
        "- Quality: level={}, deliverable={}, citations={}, domains={}, claims={}",
        quality.quality_level,
        quality.deliverable,
        quality.citation_count,
        quality.domain_count,
        quality.claim_count,
    ));
    if !quality.citations.is_empty() {
        lines.push(format!(
            "- Key sources: {}",
            quality
                .citations
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !quality.missing.is_empty() {
        lines.push(format!(
            "- Known gaps: {}",
            quality
                .missing
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut summary = lines.join("\n");
    if summary.chars().count() > PM_SESSION_SUMMARY_MAX_CHARS {
        summary = summary
            .chars()
            .skip(
                summary
                    .chars()
                    .count()
                    .saturating_sub(PM_SESSION_SUMMARY_MAX_CHARS),
            )
            .collect::<String>();
        summary = format!("...older summary truncated...\n{summary}");
    }
    summary
}

pub(super) async fn persist_pm_session_summary_after_turn(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    task_id: &str,
    question: &str,
    answer: &str,
    quality: &PmAnswerQualityDto,
    compacted: Option<&agent_gateway::CompactionRecord>,
) {
    if pm_session_memory_paused(db, tenant_id, user_id, session_id).await {
        return;
    }
    let previous = load_pm_session_summary_record(db, tenant_id, user_id, session_id).await;
    let next_summary = build_pm_session_summary(
        previous.as_ref().map(|item| item.summary.as_str()),
        question,
        answer,
        quality,
    );
    let next_turn_count = previous
        .as_ref()
        .map(|item| item.turn_count.saturating_add(1))
        .unwrap_or(1);
    let metadata = serde_json::json!({
        "qualityLevel": quality.quality_level,
        "deliverable": quality.deliverable,
        "citationCount": quality.citation_count,
        "domainCount": quality.domain_count,
        "claimCount": quality.claim_count,
        "runtimeCompaction": compacted.map(|item| serde_json::json!({
            "removedMessages": item.removed_messages,
            "count": item.count,
            "summary": item.summary,
        })),
    })
    .to_string();
    let removed_messages_i32 = compacted.and_then(|item| i32::try_from(item.removed_messages).ok());
    if let Err(error) = sqlx::query(
        "INSERT INTO pm_session_summaries
          (tenant_id, user_id, session_id, summary, turn_count, source_task_id,
           last_compacted_removed_messages, metadata_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, json(?))
         ON CONFLICT DO UPDATE SET
           summary = excluded.summary,
           turn_count = excluded.turn_count,
           source_task_id = excluded.source_task_id,
           last_compacted_removed_messages = excluded.last_compacted_removed_messages,
           metadata_json = excluded.metadata_json,
           updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(next_summary)
    .bind(next_turn_count)
    .bind(task_id)
    .bind(removed_messages_i32)
    .bind(Some(metadata))
    .execute(db)
    .await
    {
        tracing::warn!(
            tenant_id,
            user_id,
            session_id,
            task_id,
            error = %error,
            "failed to persist PM session summary"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_memory_candidate_detects_pm_context() {
        let candidate = extract_explicit_pm_memory_candidate(
            "记住：我们产品是印尼网赚休闲游戏，回答默认给实验和保护指标。",
        )
        .expect("candidate");
        assert_eq!(candidate.0, "analysis_style");
        assert!(candidate.1.contains("印尼网赚休闲游戏"));
    }

    #[test]
    fn sensitive_memory_is_rejected() {
        assert!(extract_explicit_pm_memory_candidate("记住 api_key=abc").is_none());
    }

    #[test]
    fn summary_is_bounded() {
        let quality = PmAnswerQualityDto {
            passed: true,
            deliverable: true,
            quality_level: "high".to_string(),
            has_tool_calls: false,
            tool_call_count: 0,
            citation_count: 0,
            domain_count: 0,
            claim_count: 0,
            claim_alignment_ok: true,
            triad_total_claims: 0,
            triad_aligned_claims: 0,
            triad_coverage: 1.0,
            conflict_adjudicated: true,
            conflict_confidence: 1.0,
            conflict_reason: String::new(),
            citations: Vec::new(),
            domains: Vec::new(),
            claim_alignment: Vec::new(),
            evidence_tree: Vec::new(),
            conflict_matrix: Vec::new(),
            conflict_graph: PmConflictGraphDto {
                topic_count: 0,
                edge_count: 0,
                adjudicated_count: 0,
                unresolved_count: 0,
                avg_confidence: 0.0,
                edges: Vec::new(),
            },
            missing: Vec::new(),
            suggestions: Vec::new(),
        };
        let summary =
            build_pm_session_summary(Some(&"old ".repeat(10_000)), "question", "answer", &quality);
        assert!(summary.chars().count() <= PM_SESSION_SUMMARY_MAX_CHARS + 40);
    }

    #[test]
    fn recent_history_reference_detects_sql_followup() {
        assert!(pm_recent_history_query_indicates_reference(
            "还记得我之前发你的SQL吗？"
        ));
        assert!(pm_recent_history_query_indicates_reference("?"));
        assert!(pm_recent_history_query_indicates_reference("继续"));
        assert!(!pm_recent_history_query_indicates_reference("你好"));
        assert!(pm_recent_history_text_has_structured_payload(
            "CREATE TABLE orders (id bigint, amount decimal(10,2));"
        ));
    }

    #[test]
    fn recent_history_skips_sensitive_payload() {
        assert!(!pm_recent_history_is_useful_message(
            "记住我的 api_key=sk-abc123456789000000"
        ));
    }
}
