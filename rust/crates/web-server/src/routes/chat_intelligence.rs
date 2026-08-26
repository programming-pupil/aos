use axum::{
    extract::{Extension, Path, Query, State},
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
#[cfg(feature = "nl2sql")]
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path as FsPath, PathBuf};
use zip::ZipArchive;

use crate::auth::Claims;
use crate::error::AppError;
#[cfg(feature = "nl2sql")]
use crate::nl2sql::embedding_failover::{
    embed_batch_for_config, embed_batch_with_failover, record_embedding_fallback_alert,
};
#[cfg(feature = "nl2sql")]
use crate::nl2sql::{EmbeddingProfileKind, EmbeddingTenantConfig};
// Task 20.3: `/chat/memories` is a thin view/wrapper over the single source of
// truth (`/memory/items` in `memory_continuity`). All chat-memory reads and
// writes converge onto the unified `agent_memory_items` store instead of the
// parallel `chat_memories` table.
use crate::routes::memory_continuity::{
    create_memory_item_internal, delete_memory_item_internal, list_legacy_memory_items,
    list_unified_memory_items_lightweight, update_memory_item_internal, AgentMemoryItem,
    MemoryPatchRequest, MemoryUpsertRequest,
};
use crate::state::AppState;

/// Legacy chat-memory row ids surfaced through the unified list carry this
/// prefix (see `memory_continuity::legacy_chat_row_to_item`). Edits/deletes of
/// such rows fall back to the legacy table for backward compatibility until
/// migration (task 11.1) folds them into the unified store.
const LEGACY_CHAT_ID_PREFIX: &str = "legacy-chat-";

/// Map a legacy chat memory type onto the unified `agent_memory_items`
/// vocabulary accepted by `memory_continuity::normalize_memory_type`.
fn chat_type_to_unified(chat_type: &str) -> &'static str {
    match chat_type {
        "user_preference" => "preference",
        "project_fact" => "project_fact",
        _ => "note",
    }
}

/// Reconstruct the chat memory type for API responses. The original chat type
/// is preserved in metadata (`chatMemoryType`) on write; fall back to a reverse
/// map for items created directly via `/memory/items` or legacy rows.
fn unified_type_to_chat(memory_type: &str) -> String {
    match memory_type {
        // Already a legacy chat vocabulary value (e.g. legacy rows).
        "user_preference" | "project_fact" | "long_term" | "pinned" => memory_type.to_string(),
        "preference" => "user_preference".to_string(),
        _ => "long_term".to_string(),
    }
}

/// Attach the original chat memory type to metadata so the response shape is
/// preserved through the unified store round-trip.
fn with_chat_type_metadata(metadata: Option<Value>, chat_type: &str) -> Value {
    let mut obj = match metadata {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "chatMemoryType".to_string(),
        Value::String(chat_type.to_string()),
    );
    Value::Object(obj)
}

/// Project a unified [`AgentMemoryItem`] back onto the legacy [`ChatMemoryRecord`]
/// response shape.
fn unified_item_to_chat_record(item: AgentMemoryItem) -> ChatMemoryRecord {
    let memory_type = item
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("chatMemoryType"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| unified_type_to_chat(&item.memory_type));
    ChatMemoryRecord {
        id: item.id,
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

const CHAT_FILE_MAX_INDEX_BYTES: u64 = 50 * 1024 * 1024;
const CHAT_ARCHIVE_MAX_ENTRIES: usize = 512;
const CHAT_ARCHIVE_MAX_EXTRACTED_BYTES: usize = 16 * 1024 * 1024;
const CHAT_ARCHIVE_MAX_RENDERED_BYTES: usize = 16 * 1024 * 1024;
const CHAT_FILE_CHUNK_CHARS: usize = 1800;
const CHAT_FILE_CHUNK_OVERLAP_CHARS: usize = agent_gateway::UNIFIED_WORKSPACE_UPLOAD_OVERLAP_CHARS;
const CHAT_FILE_SEARCH_LIMIT: usize = 8;
#[cfg(feature = "nl2sql")]
const CHAT_FILE_EMBED_BATCH_SIZE: usize = 16;
const CHAT_PDF_UNSUPPORTED_MESSAGE: &str =
    "PDF upload is temporarily disabled because scanned PDF OCR is not implemented yet";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMemoryRecord {
    pub id: String,
    pub memory_type: String,
    pub content: String,
    pub source: String,
    pub confidence: f64,
    pub pinned: bool,
    pub enabled: bool,
    pub metadata: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMemoryUpsertRequest {
    memory_type: Option<String>,
    content: String,
    pinned: Option<bool>,
    enabled: Option<bool>,
    metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMemoryPauseRequest {
    paused: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatMemoryListResponse {
    items: Vec<ChatMemoryRecord>,
    paused: bool,
    default_mode: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatFileRegisterRequest {
    pub(crate) file_id: String,
    pub(crate) filename: String,
    pub(crate) media_type: String,
    pub(crate) size: Option<u64>,
    pub(crate) url: String,
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatFileListQuery {
    session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatFileRecord {
    pub(crate) id: String,
    pub(crate) file_id: String,
    pub(crate) filename: String,
    pub(crate) media_type: String,
    pub(crate) size: u64,
    pub(crate) url: String,
    pub(crate) status: String,
    pub(crate) error_message: Option<String>,
    pub(crate) chunk_count: i32,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatFileSearchRequest {
    query: String,
    file_ids: Option<Vec<String>>,
    limit: Option<usize>,
    strict_grounding: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatFileCitation {
    pub file_id: String,
    pub filename: String,
    pub citation: String,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub sheet_name: Option<String>,
    pub excerpt: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_mode: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatFileSearchResponse {
    items: Vec<ChatFileCitation>,
    strict_grounding: bool,
    degraded_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatArtifactResponse {
    session_id: String,
    items: Vec<Value>,
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/files", get(list_chat_files).post(register_chat_file))
        .route("/files/search", post(search_chat_files))
        .route("/files/{file_id}/status", get(get_chat_file_status))
        .route(
            "/memories",
            get(list_chat_memories).post(create_chat_memory),
        )
        .route(
            "/memories/pause",
            get(get_chat_memory_pause).post(pause_chat_memory),
        )
        .route(
            "/memories/{memory_id}",
            patch(update_chat_memory).delete(delete_chat_memory),
        )
        .route(
            "/sessions/{session_id}/evidence",
            get(get_chat_session_evidence),
        )
        .route("/sessions/{session_id}/trace", get(get_chat_session_trace))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

fn parse_metadata(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str::<Value>(&value).ok())
}

fn row_to_memory(row: &sqlx::sqlite::SqliteRow) -> ChatMemoryRecord {
    ChatMemoryRecord {
        id: row.get("id"),
        memory_type: row.get("memory_type"),
        content: row.get("content"),
        source: row.get("source"),
        confidence: row
            .try_get::<f64, _>("confidence")
            .or_else(|_| {
                row.try_get::<String, _>("confidence")
                    .map(|s| s.parse().unwrap_or(1.0))
            })
            .unwrap_or(1.0),
        pinned: row.get::<i8, _>("pinned") != 0,
        enabled: row.get::<i8, _>("enabled") != 0,
        metadata: parse_metadata(row.get("metadata_json")),
        created_at: row
            .get::<chrono::NaiveDateTime, _>("created_at")
            .to_string(),
        updated_at: row
            .get::<chrono::NaiveDateTime, _>("updated_at")
            .to_string(),
    }
}

fn memory_type_or_default(raw: Option<&str>) -> String {
    match raw.unwrap_or("long_term").trim() {
        "user_preference" | "project_fact" | "long_term" | "pinned" => {
            raw.unwrap_or("long_term").trim().to_string()
        }
        _ => "long_term".to_string(),
    }
}

fn memory_is_sensitive(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let sensitive_needles = [
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "token=",
        "bearer ",
        "private key",
        "access key",
        "sk-",
    ];
    sensitive_needles
        .iter()
        .any(|needle| lower.contains(needle))
}

async fn memory_paused(state: &AppState, tenant_id: &str, user_id: &str) -> bool {
    sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT COALESCE(paused, 0) FROM chat_memory_preferences WHERE tenant_id = ? AND user_id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0)
        != 0
}

async fn list_chat_memories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ChatMemoryListResponse>, AppError> {
    // Read from the single source of truth (`agent_memory_items`, app=chat,
    // scope=global) and fold in legacy `chat_memories` rows as a compatibility
    // view, exactly like `/memory/items` does. This unifies the retrieval
    // ordering/scoring with `/memory/items` instead of a divergent query.
    let mut items = list_unified_memory_items_lightweight(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        Some("global"),
        Some("chat"),
        None,
        true,
        200,
    )
    .await?;
    items.extend(
        list_legacy_memory_items(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            Some("chat"),
            None,
            200,
        )
        .await
        .unwrap_or_default(),
    );
    items.truncate(200);
    let paused = memory_paused(&state, &claims.tenant_id, &claims.sub).await;
    Ok(Json(ChatMemoryListResponse {
        items: items.into_iter().map(unified_item_to_chat_record).collect(),
        paused,
        default_mode: "auto",
    }))
}

async fn create_chat_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChatMemoryUpsertRequest>,
) -> Result<Json<ChatMemoryRecord>, AppError> {
    let content = req.content.trim();
    if content.is_empty() {
        return Err(AppError::ValidationError(
            "memory content cannot be empty".into(),
        ));
    }
    if memory_is_sensitive(content) {
        return Err(AppError::ValidationError(
            "memory looks sensitive; secrets and credentials are not stored".into(),
        ));
    }
    let chat_type = memory_type_or_default(req.memory_type.as_deref());
    // Write through the single source of truth. Dedup, embeddings and
    // sensitivity screening are all handled by `create_memory_item_internal`.
    let upsert = MemoryUpsertRequest {
        scope: Some("global".to_string()),
        app: Some("chat".to_string()),
        session_id: None,
        memory_type: Some(chat_type_to_unified(&chat_type).to_string()),
        content: content.to_string(),
        source_type: Some("manual".to_string()),
        confidence: Some(1.0),
        pinned: req.pinned,
        enabled: req.enabled,
        stale_at: None,
        verified_at: None,
        metadata: Some(with_chat_type_metadata(req.metadata, &chat_type)),
    };
    let item = create_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, upsert)
        .await
        .map_err(|e| match e {
            AppError::ValidationError(_) => e,
            other => AppError::Internal(format!("failed to create chat memory: {other}")),
        })?;
    Ok(Json(unified_item_to_chat_record(item)))
}

async fn update_chat_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(memory_id): Path<String>,
    Json(req): Json<ChatMemoryUpsertRequest>,
) -> Result<Json<ChatMemoryRecord>, AppError> {
    let content = req.content.trim();
    if content.is_empty() {
        return Err(AppError::ValidationError(
            "memory content cannot be empty".into(),
        ));
    }
    if memory_is_sensitive(content) {
        return Err(AppError::ValidationError(
            "memory looks sensitive; secrets and credentials are not stored".into(),
        ));
    }
    // Backward-compat: legacy `chat_memories` rows are surfaced through the
    // unified list with a `legacy-chat-` id prefix. Edits of such rows still
    // target the legacy table (they have not been migrated yet); new rows live
    // only in the unified store.
    if let Some(legacy_id) = memory_id.strip_prefix(LEGACY_CHAT_ID_PREFIX) {
        return update_legacy_chat_memory(&state, &claims, legacy_id, &req, content).await;
    }
    let chat_type = memory_type_or_default(req.memory_type.as_deref());
    let patch = MemoryPatchRequest {
        scope: Some("global".to_string()),
        app: Some("chat".to_string()),
        session_id: None,
        memory_type: Some(chat_type_to_unified(&chat_type).to_string()),
        content: Some(content.to_string()),
        source_type: None,
        confidence: None,
        pinned: req.pinned,
        enabled: req.enabled,
        stale_at: None,
        verified_at: None,
        metadata: Some(with_chat_type_metadata(req.metadata, &chat_type)),
    };
    let item =
        update_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, &memory_id, patch)
            .await
            .map_err(|e| match e {
                AppError::ValidationError(_) | AppError::NotFound(_) => e,
                other => AppError::Internal(format!("failed to update chat memory: {other}")),
            })?;
    Ok(Json(unified_item_to_chat_record(item)))
}

/// Legacy-row edit shim: update a pre-migration `chat_memories` row in place.
async fn update_legacy_chat_memory(
    state: &AppState,
    claims: &Claims,
    legacy_id: &str,
    req: &ChatMemoryUpsertRequest,
    content: &str,
) -> Result<Json<ChatMemoryRecord>, AppError> {
    let memory_type = memory_type_or_default(req.memory_type.as_deref());
    let metadata_json = req
        .metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| AppError::ValidationError(format!("invalid metadata: {e}")))?;
    let affected = sqlx::query::<sqlx::Sqlite>(
        r#"
        UPDATE chat_memories
        SET memory_type = ?, content = ?, pinned = ?, enabled = ?, metadata_json = json(?)
        WHERE tenant_id = ? AND user_id = ? AND id = ?
        "#,
    )
    .bind(&memory_type)
    .bind(content)
    .bind(req.pinned.unwrap_or(false))
    .bind(req.enabled.unwrap_or(true))
    .bind(metadata_json)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(legacy_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to update chat memory: {e}")))?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("chat memory not found".to_string()));
    }
    let row = sqlx::query::<sqlx::Sqlite>(
        r#"
        SELECT id, memory_type, content, source, CAST(confidence AS DOUBLE) AS confidence,
               pinned, enabled, CAST(metadata_json AS TEXT) AS metadata_json, created_at, updated_at
        FROM chat_memories
        WHERE tenant_id = ? AND user_id = ? AND id = ?
        "#,
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(legacy_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to load chat memory: {e}")))?;
    row.as_ref()
        .map(|row| {
            let mut record = row_to_memory(row);
            record.id = format!("{LEGACY_CHAT_ID_PREFIX}{}", record.id);
            record
        })
        .map(Json)
        .ok_or_else(|| AppError::NotFound("chat memory not found".to_string()))
}

async fn delete_chat_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(memory_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    // Legacy-row delete shim (see `update_chat_memory`).
    if let Some(legacy_id) = memory_id.strip_prefix(LEGACY_CHAT_ID_PREFIX) {
        let affected = sqlx::query::<sqlx::Sqlite>(
            "DELETE FROM chat_memories WHERE tenant_id = ? AND user_id = ? AND id = ?",
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(legacy_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete chat memory: {e}")))?
        .rows_affected();
        return Ok(Json(json!({ "deleted": affected > 0 })));
    }
    let deleted =
        delete_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, &memory_id).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

async fn pause_chat_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChatMemoryPauseRequest>,
) -> Result<Json<Value>, AppError> {
    sqlx::query::<sqlx::Sqlite>(
        r#"
        INSERT INTO chat_memory_preferences (tenant_id, user_id, paused, paused_at)
        VALUES (?, ?, ?, IIF(? = 1, CURRENT_TIMESTAMP, NULL))
        ON CONFLICT DO UPDATE SET paused = excluded.paused, paused_at = excluded.paused_at
        "#,
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(req.paused)
    .bind(req.paused)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to update chat memory preference: {e}")))?;
    Ok(Json(json!({ "paused": req.paused, "defaultMode": "auto" })))
}

async fn get_chat_memory_pause(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let paused = memory_paused(&state, &claims.tenant_id, &claims.sub).await;
    Ok(Json(json!({ "paused": paused, "defaultMode": "auto" })))
}

fn uploads_dir(data_dir: &FsPath, user_id: &str) -> PathBuf {
    data_dir.join(".aos").join("uploads").join(user_id)
}

fn parse_upload_url(raw_url: &str) -> Option<(String, String)> {
    let marker = "/api/v1/uploads/";
    let idx = raw_url.find(marker)?;
    let rest = &raw_url[idx + marker.len()..];
    let (user_id, filename) = rest.split_once('/')?;
    if user_id.is_empty() || filename.is_empty() || filename.contains("..") {
        return None;
    }
    Some((user_id.to_string(), filename.to_string()))
}

fn strip_basic_markup(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn validate_office_archive(bytes: &[u8]) -> Result<(), String> {
    const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    let search_start = bytes.len().saturating_sub(65_557);
    let Some(relative_offset) = bytes[search_start..]
        .windows(EOCD_SIGNATURE.len())
        .rposition(|window| window == EOCD_SIGNATURE)
    else {
        return Err("office archive is missing a valid ZIP directory".to_string());
    };
    let offset = search_start + relative_offset;
    let entry_count_bytes = bytes
        .get(offset + 10..offset + 12)
        .ok_or_else(|| "office archive ZIP directory is truncated".to_string())?;
    let entry_count = usize::from(u16::from_le_bytes([
        entry_count_bytes[0],
        entry_count_bytes[1],
    ]));
    if entry_count == usize::from(u16::MAX) {
        return Err("ZIP64 office archives are not supported for file Q&A".to_string());
    }
    if entry_count > CHAT_ARCHIVE_MAX_ENTRIES {
        return Err(format!(
            "office archive contains too many entries: {entry_count} (max {CHAT_ARCHIVE_MAX_ENTRIES})"
        ));
    }
    Ok(())
}

fn read_bounded_archive_text<R: std::io::Read>(
    reader: R,
    declared_size: u64,
    remaining: &mut usize,
    label: &str,
) -> Result<String, String> {
    let allowed = *remaining;
    if declared_size > u64::try_from(allowed).unwrap_or(u64::MAX) {
        return Err(format!(
            "{label} expands beyond the remaining office extraction limit ({allowed} bytes)"
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(declared_size)
            .unwrap_or(allowed)
            .min(allowed),
    );
    reader
        .take(u64::try_from(allowed).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {label} failed: {error}"))?;
    if bytes.len() > allowed {
        return Err(format!(
            "{label} expands beyond the office extraction limit ({CHAT_ARCHIVE_MAX_EXTRACTED_BYTES} bytes)"
        ));
    }
    *remaining = remaining.saturating_sub(bytes.len());
    String::from_utf8(bytes).map_err(|error| format!("{label} is not valid UTF-8 XML: {error}"))
}

fn decode_xml_text(input: &str) -> String {
    strip_basic_markup(input)
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn xml_tag_content<'a>(input: &'a str, tag: &str) -> Option<&'a str> {
    let marker = format!("<{tag}");
    let start = input.find(&marker)?;
    let after_open = &input[start..];
    let body_start = after_open.find('>')? + 1;
    let body = &after_open[body_start..];
    let end_marker = format!("</{tag}>");
    let body_end = body.find(&end_marker)?;
    Some(&body[..body_end])
}

fn extract_xlsx_shared_strings(raw: &str) -> Vec<String> {
    raw.split("<si")
        .skip(1)
        .filter_map(|part| part.split_once("</si>").map(|(body, _)| body))
        .map(decode_xml_text)
        .collect()
}

fn xlsx_cell_type_is(opening_tag: &str, expected: &str) -> bool {
    opening_tag.contains(&format!("t=\"{expected}\""))
        || opening_tag.contains(&format!("t='{expected}'"))
}

fn extract_xlsx_sheet_text(
    raw: &str,
    shared: &[String],
    max_output_bytes: usize,
) -> Result<String, String> {
    let mut output = String::new();
    for row in raw.split("<row").skip(1) {
        let Some((row_body, _)) = row.split_once("</row>") else {
            continue;
        };
        let mut values = Vec::new();
        for cell in row_body.split("<c").skip(1) {
            let Some((opening_tag, after_open)) = cell.split_once('>') else {
                continue;
            };
            let Some((cell_body, _)) = after_open.split_once("</c>") else {
                continue;
            };
            let value = if xlsx_cell_type_is(opening_tag, "inlineStr") {
                xml_tag_content(cell_body, "t")
                    .map(decode_xml_text)
                    .unwrap_or_default()
            } else {
                let raw_value = xml_tag_content(cell_body, "v")
                    .map(decode_xml_text)
                    .unwrap_or_default();
                if xlsx_cell_type_is(opening_tag, "s") {
                    raw_value
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| shared.get(index))
                        .cloned()
                        .unwrap_or(raw_value)
                } else {
                    raw_value
                }
            };
            if !value.is_empty() {
                values.push(value);
            }
        }
        if values.is_empty() {
            continue;
        }
        let line = values.join("\t");
        if output.len().saturating_add(line.len()).saturating_add(1) > max_output_bytes {
            return Err(format!(
                "xlsx rendered text exceeds {CHAT_ARCHIVE_MAX_RENDERED_BYTES} bytes"
            ));
        }
        output.push_str(&line);
        output.push('\n');
    }
    Ok(output.trim().to_string())
}

fn extract_docx_text(bytes: &[u8]) -> Result<String, String> {
    validate_office_archive(bytes)?;
    let reader = std::io::Cursor::new(bytes);
    let mut archive = ZipArchive::new(reader).map_err(|e| format!("open docx failed: {e}"))?;
    let mut remaining = CHAT_ARCHIVE_MAX_EXTRACTED_BYTES;
    let document_file = archive
        .by_name("word/document.xml")
        .map_err(|e| format!("read docx document.xml failed: {e}"))?;
    let declared_size = document_file.size();
    let document = read_bounded_archive_text(
        document_file,
        declared_size,
        &mut remaining,
        "docx document.xml",
    )?;
    Ok(strip_basic_markup(&document))
}

pub(crate) fn extract_xlsx_text(bytes: &[u8]) -> Result<String, String> {
    validate_office_archive(bytes)?;
    let reader = std::io::Cursor::new(bytes);
    let mut archive = ZipArchive::new(reader).map_err(|e| format!("open xlsx failed: {e}"))?;
    let mut remaining = CHAT_ARCHIVE_MAX_EXTRACTED_BYTES;
    let mut shared = Vec::<String>::new();
    if let Ok(mut file) = archive.by_name("xl/sharedStrings.xml") {
        let declared_size = file.size();
        let raw = read_bounded_archive_text(
            &mut file,
            declared_size,
            &mut remaining,
            "xlsx sharedStrings.xml",
        )?;
        shared = extract_xlsx_shared_strings(&raw);
    }
    let mut sections = Vec::new();
    let mut rendered_bytes = 0usize;
    for idx in 1..=50 {
        let name = format!("xl/worksheets/sheet{idx}.xml");
        let Ok(mut file) = archive.by_name(&name) else {
            continue;
        };
        let declared_size = file.size();
        let raw = read_bounded_archive_text(&mut file, declared_size, &mut remaining, &name)?;
        let sheet_text = extract_xlsx_sheet_text(
            &raw,
            &shared,
            CHAT_ARCHIVE_MAX_RENDERED_BYTES.saturating_sub(rendered_bytes),
        )?;
        if !sheet_text.is_empty() {
            let section = format!("[sheet:{idx}]\n{sheet_text}");
            rendered_bytes = rendered_bytes.saturating_add(section.len());
            if rendered_bytes > CHAT_ARCHIVE_MAX_RENDERED_BYTES {
                return Err(format!(
                    "xlsx rendered text exceeds {CHAT_ARCHIVE_MAX_RENDERED_BYTES} bytes"
                ));
            }
            sections.push(section);
        }
    }
    if sections.is_empty() {
        Err("xlsx contains no extractable sheet text".to_string())
    } else {
        Ok(sections.join("\n\n"))
    }
}

#[cfg(test)]
mod office_parser_tests {
    use super::*;

    #[test]
    fn xlsx_shared_string_mapping_does_not_replace_plain_numbers() {
        let sheet = r#"<worksheet><sheetData><row r="1">
            <c r="A1" t="s"><v>0</v></c>
            <c r="B1"><v>0</v></c>
        </row></sheetData></worksheet>"#;
        let text =
            extract_xlsx_sheet_text(sheet, &["Revenue".to_string()], 1024).expect("sheet text");

        assert_eq!(text, "Revenue\t0");
    }

    #[test]
    fn xlsx_rendering_is_bounded_even_when_shared_values_expand() {
        let sheet =
            r#"<worksheet><sheetData><row><c t="s"><v>0</v></c></row></sheetData></worksheet>"#;
        let error = extract_xlsx_sheet_text(sheet, &["large-value".to_string()], 4)
            .expect_err("render limit should reject expanded text");

        assert!(error.contains("rendered text exceeds"));
    }

    #[test]
    fn office_archive_entry_count_is_bounded_before_extraction() {
        let mut eocd = vec![0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0];
        eocd.extend_from_slice(
            &u16::try_from(CHAT_ARCHIVE_MAX_ENTRIES + 1)
                .unwrap()
                .to_le_bytes(),
        );
        eocd.extend_from_slice(&[0; 10]);

        let error = validate_office_archive(&eocd).expect_err("entry count should be rejected");
        assert!(error.contains("too many entries"));
    }
}

fn document_bytes_to_text(
    media_type: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, String> {
    let media_type = media_type.trim().to_ascii_lowercase();
    let filename = filename.trim().to_ascii_lowercase();
    let text_like_extension = [
        ".txt",
        ".md",
        ".markdown",
        ".csv",
        ".json",
        ".jsonl",
        ".sql",
        ".css",
        ".js",
        ".ts",
        ".tsx",
        ".jsx",
        ".xml",
        ".log",
        ".rtf",
    ]
    .iter()
    .any(|ext| filename.ends_with(ext));
    match media_type.as_str() {
        "text/plain"
        | "text/markdown"
        | "text/csv"
        | "text/css"
        | "text/javascript"
        | "application/javascript"
        | "application/json"
        | "application/x-ndjson"
        | "application/sql"
        | "application/xml"
        | "text/xml" => String::from_utf8(bytes.to_vec())
            .map_err(|e| format!("document is not valid utf-8: {e}")),
        "text/html" => String::from_utf8(bytes.to_vec())
            .map(|raw| strip_basic_markup(&raw))
            .map_err(|e| format!("html document is not valid utf-8: {e}")),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            extract_docx_text(bytes)
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            extract_xlsx_text(bytes)
        }
        "application/pdf" => Err(CHAT_PDF_UNSUPPORTED_MESSAGE.to_string()),
        _ if filename.ends_with(".docx") => extract_docx_text(bytes),
        _ if filename.ends_with(".xlsx") => extract_xlsx_text(bytes),
        _ if filename.ends_with(".html") || filename.ends_with(".htm") => {
            String::from_utf8(bytes.to_vec())
                .map(|raw| strip_basic_markup(&raw))
                .map_err(|e| format!("html document is not valid utf-8: {e}"))
        }
        _ if text_like_extension => String::from_utf8(bytes.to_vec())
            .map_err(|e| format!("document is not valid utf-8: {e}")),
        _ => String::from_utf8(bytes.to_vec())
            .map_err(|_| format!("unsupported document media type: {media_type}")),
    }
}

fn chunk_text(text: &str) -> Vec<(usize, usize, String)> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut start_line = 1usize;
    let mut line_no = 1usize;
    for line in lines {
        if current.is_empty() {
            start_line = line_no;
        }
        current.push_str(line);
        current.push('\n');
        if current.chars().count() >= CHAT_FILE_CHUNK_CHARS {
            chunks.push((start_line, line_no, current.clone()));
            let overlap = current
                .chars()
                .rev()
                .take(CHAT_FILE_CHUNK_OVERLAP_CHARS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>();
            current = overlap;
            start_line = line_no;
        }
        line_no += 1;
    }
    if !current.trim().is_empty() {
        chunks.push((start_line, line_no.saturating_sub(1), current));
    }
    chunks
}

#[cfg(feature = "nl2sql")]
fn serialize_embedding_vector(vector: &[f32]) -> Option<String> {
    serde_json::to_string(vector).ok()
}

fn deserialize_embedding_vector(raw: Option<String>) -> Option<Vec<f32>> {
    let raw = raw?;
    serde_json::from_str::<Vec<f32>>(&raw)
        .ok()
        .filter(|vector| !vector.is_empty())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (left, right) in a.iter().zip(b.iter()) {
        let left = f64::from(*left);
        let right = f64::from(*right);
        dot += left * right;
        norm_a += left * left;
        norm_b += right * right;
    }
    if norm_a <= f64::EPSILON || norm_b <= f64::EPSILON {
        return None;
    }
    Some((dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0))
}

#[cfg(feature = "nl2sql")]
async fn embed_chat_file_chunks_best_effort(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    file_id: &str,
    chunk_ids: &[(String, String)],
) {
    if chunk_ids.is_empty() {
        return;
    }
    let mut selected_config = None::<EmbeddingTenantConfig>;
    for batch in chunk_ids.chunks(CHAT_FILE_EMBED_BATCH_SIZE) {
        let texts = batch
            .iter()
            .map(|(_, content)| content.clone())
            .collect::<Vec<_>>();
        let result = if let Some(config) = selected_config.as_ref() {
            embed_batch_for_config(config, &texts, true, None)
                .await
                .map(|(vectors, _)| (config.clone(), vectors))
        } else {
            embed_batch_with_failover(&state.db, tenant_id, "chat", &texts, true, None)
                .await
                .map(|outcome| (outcome.config, outcome.vectors))
        };
        let (config, vectors) = match result {
            Ok(result) => result,
            Err(error) => {
                if let Some(config) = selected_config
                    .as_ref()
                    .filter(|config| config.profile_kind == EmbeddingProfileKind::Api)
                {
                    let _ = record_embedding_fallback_alert(
                        &state.db,
                        tenant_id,
                        "chat",
                        config,
                        &error.to_string(),
                    )
                    .await;
                    let local = crate::nl2sql::resolve_embedding_profiles(
                        &state.db,
                        tenant_id,
                        Some("chat"),
                    )
                    .await
                    .local;
                    if let Err(local_error) = embed_chat_file_chunks_with_config(
                        state, tenant_id, user_id, file_id, chunk_ids, &local,
                    )
                    .await
                    {
                        tracing::warn!(tenant_id, file_id, error = %local_error, "chat file local embedding rebuild failed; keyword retrieval remains available");
                    }
                } else {
                    tracing::warn!(
                        tenant_id,
                        file_id,
                        error = %error,
                        "chat file semantic indexing failed; keyword retrieval remains available"
                    );
                }
                return;
            }
        };
        selected_config.get_or_insert_with(|| config.clone());
        if let Err(error) = persist_chat_file_embedding_batch(
            state,
            tenant_id,
            user_id,
            file_id,
            batch,
            &config.model,
            &vectors,
        )
        .await
        {
            tracing::warn!(tenant_id, file_id, error = %error, "failed to persist chat file embedding batch");
            return;
        }
    }
}

#[cfg(feature = "nl2sql")]
async fn embed_chat_file_chunks_with_config(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    file_id: &str,
    chunk_ids: &[(String, String)],
    config: &EmbeddingTenantConfig,
) -> anyhow::Result<()> {
    for batch in chunk_ids.chunks(CHAT_FILE_EMBED_BATCH_SIZE) {
        let texts = batch
            .iter()
            .map(|(_, content)| content.clone())
            .collect::<Vec<_>>();
        let (vectors, _) = embed_batch_for_config(config, &texts, true, None).await?;
        persist_chat_file_embedding_batch(
            state,
            tenant_id,
            user_id,
            file_id,
            batch,
            &config.model,
            &vectors,
        )
        .await?;
    }
    Ok(())
}

#[cfg(feature = "nl2sql")]
async fn persist_chat_file_embedding_batch(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    file_id: &str,
    batch: &[(String, String)],
    model: &str,
    vectors: &[Vec<f32>],
) -> anyhow::Result<()> {
    for ((chunk_id, _), vector) in batch.iter().zip(vectors) {
        let Some(raw_vector) = serialize_embedding_vector(vector) else {
            continue;
        };
        sqlx::query::<sqlx::Sqlite>(
            r#"
            UPDATE chat_file_workspace_chunks
            SET embedding_model = ?, embedding_dimensions = ?, embedding_json = ?
            WHERE tenant_id = ? AND user_id = ? AND file_id = ? AND id = ?
            "#,
        )
        .bind(model)
        .bind(i32::try_from(vector.len()).unwrap_or(i32::MAX))
        .bind(raw_vector)
        .bind(tenant_id)
        .bind(user_id)
        .bind(file_id)
        .bind(chunk_id)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

#[cfg(feature = "nl2sql")]
pub(crate) fn schedule_tenant_chat_embedding_rebuild(state: AppState, tenant_id: String) {
    tokio::spawn(async move {
        let rows = match sqlx::query(
            "SELECT c.user_id, c.file_id, c.id, c.content
             FROM chat_file_workspace_chunks AS c
             JOIN chat_file_workspace_files AS f
               ON f.tenant_id = c.tenant_id AND f.user_id = c.user_id
              AND f.file_id = c.file_id
             WHERE c.tenant_id = ? AND f.status = 'indexed'
             ORDER BY c.user_id, c.file_id, c.chunk_index",
        )
        .bind(&tenant_id)
        .fetch_all(&state.db)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(tenant_id, error = %error, "failed to load chat embeddings for provider migration");
                return;
            }
        };
        let mut files = BTreeMap::<(String, String), Vec<(String, String)>>::new();
        for row in rows {
            files
                .entry((row.get("user_id"), row.get("file_id")))
                .or_default()
                .push((row.get("id"), row.get("content")));
        }
        for ((user_id, file_id), chunks) in files {
            embed_chat_file_chunks_best_effort(&state, &tenant_id, &user_id, &file_id, &chunks)
                .await;
            tokio::task::yield_now().await;
        }
    });
}

#[cfg(not(feature = "nl2sql"))]
pub(crate) fn schedule_tenant_chat_embedding_rebuild(_state: AppState, _tenant_id: String) {}

#[cfg(not(feature = "nl2sql"))]
async fn embed_chat_file_chunks_best_effort(
    _state: &AppState,
    _tenant_id: &str,
    _user_id: &str,
    _file_id: &str,
    _chunk_ids: &[(String, String)],
) {
}

#[cfg(feature = "nl2sql")]
async fn embed_chat_file_query(
    state: &AppState,
    tenant_id: &str,
    query: &str,
) -> Option<(String, Vec<f32>)> {
    match embed_batch_with_failover(
        &state.db,
        tenant_id,
        "chat",
        &[query.to_string()],
        false,
        None,
    )
    .await
    {
        Ok(mut outcome) => outcome
            .vectors
            .pop()
            .map(|vector: Vec<f32>| (outcome.config.model, vector)),
        Err(error) => {
            tracing::debug!(
                tenant_id,
                error = %error,
                "chat file semantic query embedding failed; using keyword fallback"
            );
            None
        }
    }
}

#[cfg(not(feature = "nl2sql"))]
async fn embed_chat_file_query(
    _state: &AppState,
    _tenant_id: &str,
    _query: &str,
) -> Option<(String, Vec<f32>)> {
    None
}

async fn index_chat_file(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    file_id: &str,
    filename: &str,
    media_type: &str,
    url: &str,
) -> Result<i32, String> {
    let (url_user_id, saved_filename) =
        parse_upload_url(url).ok_or_else(|| "invalid upload url".to_string())?;
    if url_user_id != user_id {
        return Err("upload url user mismatch".to_string());
    }
    let path = uploads_dir(&state.data_dir, user_id).join(saved_filename);
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("read file metadata failed: {e}"))?;
    if metadata.len() > CHAT_FILE_MAX_INDEX_BYTES {
        return Err(format!(
            "file too large for chat workspace indexing: {} bytes",
            metadata.len()
        ));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("read file failed: {e}"))?;
    let raw_text = document_bytes_to_text(media_type, filename, &bytes)?;
    let chunks = chunk_text(&raw_text);
    sqlx::query::<sqlx::Sqlite>("DELETE FROM chat_file_workspace_chunks WHERE tenant_id = ? AND user_id = ? AND file_id = ?")
        .bind(tenant_id)
        .bind(user_id)
        .bind(file_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("clear existing chunks failed: {e}"))?;
    let mut inserted_chunks = Vec::<(String, String)>::new();
    for (idx, (line_start, line_end, content)) in chunks.iter().enumerate() {
        let chunk_id = uuid::Uuid::new_v4().to_string();
        sqlx::query::<sqlx::Sqlite>(
            r#"
            INSERT INTO chat_file_workspace_chunks
              (id, tenant_id, user_id, file_id, chunk_index, line_start, line_end, content, metadata_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, json(?))
            "#,
        )
        .bind(&chunk_id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(file_id)
        .bind(i32::try_from(idx).unwrap_or(i32::MAX))
        .bind(i32::try_from(*line_start).unwrap_or(i32::MAX))
        .bind(i32::try_from(*line_end).unwrap_or(i32::MAX))
        .bind(content)
        .bind(Some(json!({ "filename": filename, "mediaType": media_type }).to_string()))
        .execute(&state.db)
        .await
        .map_err(|e| format!("insert chunk failed: {e}"))?;
        inserted_chunks.push((chunk_id, content.clone()));
    }
    embed_chat_file_chunks_best_effort(state, tenant_id, user_id, file_id, &inserted_chunks).await;
    Ok(i32::try_from(chunks.len()).unwrap_or(i32::MAX))
}

async fn register_chat_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChatFileRegisterRequest>,
) -> Result<Json<ChatFileRecord>, AppError> {
    Ok(Json(
        register_chat_file_internal(&state, &claims.tenant_id, &claims.sub, req).await?,
    ))
}

pub(crate) async fn register_chat_file_internal(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    req: ChatFileRegisterRequest,
) -> Result<ChatFileRecord, AppError> {
    let file_id = req.file_id.trim();
    if file_id.is_empty() || req.url.trim().is_empty() {
        return Err(AppError::ValidationError(
            "fileId and url are required".into(),
        ));
    }
    let is_visual_attachment = req
        .media_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/");
    let is_pdf_attachment = req
        .media_type
        .trim()
        .eq_ignore_ascii_case("application/pdf")
        || req.filename.trim().to_ascii_lowercase().ends_with(".pdf");
    let is_metadata_only_attachment = is_visual_attachment || is_pdf_attachment;
    let existing = sqlx::query::<sqlx::Sqlite>(
        r#"
        SELECT status, url
        FROM chat_file_workspace_files
        WHERE tenant_id = ? AND user_id = ? AND file_id = ?
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(file_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to inspect chat file: {e}")))?;
    if existing.as_ref().is_some_and(|row| {
        let status = row.get::<String, _>("status");
        (status == "indexed" || (is_metadata_only_attachment && status == "uploaded"))
            && row.get::<String, _>("url") == req.url
    }) {
        sqlx::query::<sqlx::Sqlite>(
            r#"
            UPDATE chat_file_workspace_files
            SET session_id = COALESCE(?, session_id),
                filename = ?,
                media_type = ?,
                size_bytes = ?,
                error_message = NULL
            WHERE tenant_id = ? AND user_id = ? AND file_id = ?
            "#,
        )
        .bind(req.session_id.as_deref())
        .bind(&req.filename)
        .bind(&req.media_type)
        .bind(crate::sqlite_i64(req.size.unwrap_or(0)))
        .bind(tenant_id)
        .bind(user_id)
        .bind(file_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to bind chat file session: {e}")))?;
        return get_chat_file_record(state, tenant_id, user_id, file_id).await;
    }
    let row_id = uuid::Uuid::new_v4().to_string();
    sqlx::query::<sqlx::Sqlite>(
        r#"
        INSERT INTO chat_file_workspace_files
          (id, tenant_id, user_id, session_id, file_id, filename, media_type, size_bytes, url, status)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT DO UPDATE SET
          session_id = excluded.session_id,
          filename = excluded.filename,
          media_type = excluded.media_type,
          size_bytes = excluded.size_bytes,
          url = excluded.url,
          status = excluded.status,
          error_message = NULL
        "#,
    )
    .bind(row_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(req.session_id.as_deref())
    .bind(file_id)
    .bind(&req.filename)
    .bind(&req.media_type)
    .bind(crate::sqlite_i64(req.size.unwrap_or(0)))
    .bind(&req.url)
    .bind(if is_metadata_only_attachment {
        "uploaded"
    } else {
        "parsing"
    })
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to register chat file: {e}")))?;

    // Keep original images and PDFs available even when this service cannot
    // derive reliable text chunks. The multimodal path reads the upload.
    if is_metadata_only_attachment {
        sqlx::query::<sqlx::Sqlite>(
            "DELETE FROM chat_file_workspace_chunks WHERE tenant_id = ? AND user_id = ? AND file_id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(file_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to clear stale image chunks: {e}")))?;
        return get_chat_file_record(state, tenant_id, user_id, file_id).await;
    }

    match index_chat_file(
        state,
        tenant_id,
        user_id,
        file_id,
        &req.filename,
        &req.media_type,
        &req.url,
    )
    .await
    {
        Ok(chunk_count) => {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE chat_file_workspace_files SET status = 'indexed', chunk_count = ?, error_message = NULL WHERE tenant_id = ? AND user_id = ? AND file_id = ?",
            )
            .bind(chunk_count)
            .bind(tenant_id)
            .bind(user_id)
            .bind(file_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to update chat file status: {e}")))?;
        }
        Err(reason) => {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE chat_file_workspace_files SET status = 'failed', error_message = ?, chunk_count = 0 WHERE tenant_id = ? AND user_id = ? AND file_id = ?",
            )
            .bind(reason)
            .bind(tenant_id)
            .bind(user_id)
            .bind(file_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to update chat file status: {e}")))?;
        }
    }
    get_chat_file_record(state, tenant_id, user_id, file_id).await
}

fn row_to_chat_file(row: &sqlx::sqlite::SqliteRow) -> ChatFileRecord {
    ChatFileRecord {
        id: row.get("id"),
        file_id: row.get("file_id"),
        filename: row.get("filename"),
        media_type: row.get("media_type"),
        size: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
        url: row.get("url"),
        status: row.get("status"),
        error_message: row.get("error_message"),
        chunk_count: row.get("chunk_count"),
        created_at: row
            .get::<chrono::NaiveDateTime, _>("created_at")
            .to_string(),
        updated_at: row
            .get::<chrono::NaiveDateTime, _>("updated_at")
            .to_string(),
    }
}

async fn get_chat_file_record(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    file_id: &str,
) -> Result<ChatFileRecord, AppError> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r#"
        SELECT id, file_id, filename, media_type, size_bytes, url, status, error_message,
               chunk_count, created_at, updated_at
        FROM chat_file_workspace_files
        WHERE tenant_id = ? AND user_id = ? AND file_id = ?
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(file_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to load chat file: {e}")))?;
    row.as_ref()
        .map(row_to_chat_file)
        .ok_or_else(|| AppError::NotFound("chat file not found".to_string()))
}

async fn get_chat_file_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(file_id): Path<String>,
) -> Result<Json<ChatFileRecord>, AppError> {
    Ok(Json(
        get_chat_file_record(&state, &claims.tenant_id, &claims.sub, &file_id).await?,
    ))
}

async fn list_chat_files(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ChatFileListQuery>,
) -> Result<Json<Value>, AppError> {
    let rows = if let Some(session_id) = query.session_id.as_deref() {
        sqlx::query::<sqlx::Sqlite>(
            r#"
            SELECT id, file_id, filename, media_type, size_bytes, url, status, error_message,
                   chunk_count, created_at, updated_at
            FROM chat_file_workspace_files
            WHERE tenant_id = ? AND user_id = ? AND (session_id = ? OR session_id IS NULL)
            ORDER BY updated_at DESC
            LIMIT 200
            "#,
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(session_id)
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query::<sqlx::Sqlite>(
            r#"
            SELECT id, file_id, filename, media_type, size_bytes, url, status, error_message,
                   chunk_count, created_at, updated_at
            FROM chat_file_workspace_files
            WHERE tenant_id = ? AND user_id = ?
            ORDER BY updated_at DESC
            LIMIT 200
            "#,
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .fetch_all(&state.db)
        .await
    }
    .map_err(|e| AppError::Internal(format!("failed to list chat files: {e}")))?;
    let items = rows.iter().map(row_to_chat_file).collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "total": items.len() })))
}

fn tokenize_query(query: &str) -> Vec<String> {
    query
        .split(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .map(str::trim)
        .filter(|token| token.chars().count() >= 2)
        .map(|token| token.to_ascii_lowercase())
        .take(16)
        .collect()
}

fn score_chunk(query_tokens: &[String], content: &str) -> f64 {
    if query_tokens.is_empty() {
        return 0.0;
    }
    let lower = content.to_ascii_lowercase();
    let hits = query_tokens
        .iter()
        .filter(|token| lower.contains(token.as_str()))
        .count();
    hits as f64 / query_tokens.len() as f64
}

fn excerpt_for_query(content: &str, query_tokens: &[String]) -> String {
    let lower = content.to_ascii_lowercase();
    let pos = query_tokens
        .iter()
        .filter_map(|token| lower.find(token))
        .min()
        .unwrap_or(0);
    let start = pos.saturating_sub(220);
    content
        .chars()
        .skip(start)
        .take(520)
        .collect::<String>()
        .trim()
        .to_string()
}

// Exposed `pub(crate)` so the Super_Assistant orchestration layer
// (`super_assistant::build_injection_bundle`) can re-retrieve attachment chunks
// through the *same* attachment index rather than reimplementing chunk search
// (task 6.2, Req 4.7). This is the internal search behind `POST /chat/files/search`.
pub(crate) async fn search_chat_files_internal(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_id: Option<&str>,
    query: &str,
    file_ids: &[String],
    limit: usize,
) -> Result<Vec<ChatFileCitation>, AppError> {
    let mut rows_query = String::from(
        r#"
        SELECT c.file_id, f.filename, c.line_start, c.line_end, c.sheet_name, c.content,
               c.embedding_model, c.embedding_json
        FROM chat_file_workspace_chunks c
        JOIN chat_file_workspace_files f
          ON f.tenant_id = c.tenant_id AND f.user_id = c.user_id AND f.file_id = c.file_id
        WHERE c.tenant_id = ? AND c.user_id = ? AND f.status = 'indexed'
        "#,
    );
    if session_id.is_some() {
        rows_query.push_str(" AND (f.session_id = ? OR f.session_id IS NULL)");
    }
    if !file_ids.is_empty() {
        rows_query.push_str(" AND c.file_id IN (");
        rows_query.push_str(&vec!["?"; file_ids.len()].join(","));
        rows_query.push(')');
    }
    rows_query.push_str(" ORDER BY f.updated_at DESC, c.chunk_index ASC LIMIT 500");
    let mut query_builder = sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(rows_query))
        .bind(tenant_id)
        .bind(user_id);
    if let Some(session_id) = session_id {
        query_builder = query_builder.bind(session_id);
    }
    for file_id in file_ids {
        query_builder = query_builder.bind(file_id);
    }
    let rows = query_builder
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to search chat files: {e}")))?;
    let query_tokens = tokenize_query(query);
    let query_embedding: Option<(String, Vec<f32>)> =
        embed_chat_file_query(state, tenant_id, query).await;
    let mut scored = rows
        .into_iter()
        .filter_map(|row| {
            let content: String = row.get("content");
            let lexical_score = score_chunk(&query_tokens, &content);
            let semantic_score: Option<f64> =
                query_embedding.as_ref().and_then(|(model, query_vector)| {
                    let row_model: Option<String> = row.get("embedding_model");
                    if row_model.as_deref() != Some(model.as_str()) {
                        return None;
                    }
                    let vector = deserialize_embedding_vector(row.get("embedding_json"));
                    vector
                        .as_deref()
                        .and_then(|chunk_vector| cosine_similarity(query_vector, chunk_vector))
                        .map(|score| ((score + 1.0) / 2.0).clamp(0.0, 1.0))
                });
            let (score, retrieval_mode) = match semantic_score {
                Some(semantic) => {
                    let combined = semantic.mul_add(0.72, lexical_score * 0.28);
                    (combined, "semantic_rerank".to_string())
                }
                None => (lexical_score, "keyword".to_string()),
            };
            if score <= 0.0 && !query_tokens.is_empty() {
                return None;
            }
            let file_id: String = row.get("file_id");
            let filename: String = row.get("filename");
            let line_start: Option<i32> = row.get("line_start");
            let line_end: Option<i32> = row.get("line_end");
            let sheet_name: Option<String> = row.get("sheet_name");
            let citation = match (line_start, line_end) {
                (Some(start), Some(end)) => format!("[{filename} L{start}-L{end}]"),
                _ => format!("[{filename}]"),
            };
            Some(ChatFileCitation {
                file_id,
                filename,
                citation,
                line_start,
                line_end,
                sheet_name,
                excerpt: excerpt_for_query(&content, &query_tokens),
                score,
                retrieval_mode: Some(retrieval_mode),
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(limit.max(1).min(20));
    Ok(scored)
}

async fn search_chat_files(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChatFileSearchRequest>,
) -> Result<Json<ChatFileSearchResponse>, AppError> {
    let query = req.query.trim();
    if query.is_empty() {
        return Err(AppError::ValidationError("query cannot be empty".into()));
    }
    let file_ids = req.file_ids.unwrap_or_default();
    let items = search_chat_files_internal(
        &state,
        &claims.tenant_id,
        &claims.sub,
        None,
        query,
        &file_ids,
        req.limit.unwrap_or(CHAT_FILE_SEARCH_LIMIT),
    )
    .await?;
    let strict = req.strict_grounding.unwrap_or(false);
    let degraded_reason = (strict && items.is_empty())
        .then(|| "strict grounding requested but no matching file evidence was found".to_string());
    Ok(Json(ChatFileSearchResponse {
        items,
        strict_grounding: strict,
        degraded_reason,
    }))
}

async fn get_chat_session_evidence(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> Result<Json<ChatArtifactResponse>, AppError> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        r#"
        SELECT CAST(payload_json AS TEXT) AS payload_json
        FROM chat_turn_artifacts
        WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND artifact_type = 'evidence'
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to load chat evidence: {e}")))?;
    let items = rows
        .into_iter()
        .filter_map(|row| row.get::<Option<String>, _>("payload_json"))
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .collect();
    Ok(Json(ChatArtifactResponse { session_id, items }))
}

async fn get_chat_session_trace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> Result<Json<ChatArtifactResponse>, AppError> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        r#"
        SELECT CAST(payload_json AS TEXT) AS payload_json
        FROM chat_turn_artifacts
        WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND artifact_type = 'trace'
        ORDER BY created_at DESC
        LIMIT 100
        "#,
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to load chat trace: {e}")))?;
    let items = rows
        .into_iter()
        .filter_map(|row| row.get::<Option<String>, _>("payload_json"))
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .collect();
    Ok(Json(ChatArtifactResponse { session_id, items }))
}

#[cfg(feature = "agent")]
pub async fn build_chat_workspace_file_prompt(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    message: &str,
    file_ids: &[String],
    strict_grounding: bool,
) -> Option<(String, Vec<ChatFileCitation>)> {
    let citations = search_chat_files_internal(
        state,
        tenant_id,
        user_id,
        None,
        message,
        file_ids,
        CHAT_FILE_SEARCH_LIMIT,
    )
    .await
    .ok()?;
    if citations.is_empty() {
        return None;
    }
    let strict = if strict_grounding {
        "Strict file grounding is enabled: answer only with the cited file excerpts. If evidence is insufficient, say so."
    } else {
        "Use file excerpts as primary evidence and clearly separate general knowledge."
    };
    let excerpts = citations
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            format!(
                "[file-evidence:{}] {}\n{}\n",
                idx + 1,
                item.citation,
                item.excerpt
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some((
        format!("[File workspace evidence]\n{strict}\n{excerpts}\n[/File workspace evidence]"),
        citations,
    ))
}

#[cfg(feature = "agent")]
fn extract_explicit_memory_candidate(message: &str) -> Option<(String, String)> {
    let trimmed = message.trim();
    if trimmed.chars().count() < 6 || memory_is_sensitive(trimmed) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let explicit = [
        "以后",
        "今后",
        "记住",
        "默认",
        "偏好",
        "以后都",
        "以后请",
        "always",
        "from now on",
        "remember that",
        "remember this",
        "by default",
        "prefer",
        "my preference",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !explicit {
        return None;
    }

    let memory_type = if lower.contains("中文")
        || lower.contains("英文")
        || lower.contains("简洁")
        || lower.contains("详细")
        || lower.contains("格式")
        || lower.contains("language")
        || lower.contains("format")
        || lower.contains("concise")
        || lower.contains("detailed")
    {
        "user_preference"
    } else {
        "long_term"
    };

    let mut content = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if content.chars().count() > 360 {
        content = content.chars().take(360).collect::<String>();
    }
    if content.is_empty() || memory_is_sensitive(&content) {
        return None;
    }
    Some((memory_type.to_string(), content))
}

#[cfg(feature = "agent")]
pub async fn persist_chat_memory_candidate(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    user_message: &str,
) {
    if memory_paused(state, tenant_id, user_id).await {
        return;
    }
    let Some((memory_type, content)) = extract_explicit_memory_candidate(user_message) else {
        return;
    };
    // Write through the single source of truth. `create_memory_item_internal`
    // dedups by content hash (ON CONFLICT DO UPDATE SET), so the previous manual
    // duplicate probe against `chat_memories` is no longer needed.
    let metadata = with_chat_type_metadata(
        Some(json!({
            "source": "explicit_user_instruction",
            "createdFrom": "chat_turn"
        })),
        &memory_type,
    );
    let upsert = MemoryUpsertRequest {
        scope: Some("global".to_string()),
        app: Some("chat".to_string()),
        session_id: None,
        memory_type: Some(chat_type_to_unified(&memory_type).to_string()),
        content,
        source_type: Some("explicit_user".to_string()),
        confidence: Some(0.92),
        pinned: Some(false),
        enabled: Some(true),
        stale_at: None,
        verified_at: None,
        metadata: Some(metadata),
    };
    if let Err(error) = create_memory_item_internal(&state.db, tenant_id, user_id, upsert).await {
        tracing::warn!(tenant_id, user_id, error = %error, "failed to persist chat memory candidate");
    }
}

#[cfg(feature = "agent")]
pub async fn persist_chat_artifact(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    artifact_type: &str,
    payload: &Value,
) {
    let payload_json = match serde_json::to_string(payload) {
        Ok(value) => value,
        Err(_) => return,
    };
    let _ = sqlx::query::<sqlx::Sqlite>(
        r#"
        INSERT INTO chat_turn_artifacts (id, tenant_id, user_id, session_id, artifact_type, payload_json)
        VALUES (?, ?, ?, ?, ?, json(?))
        "#,
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(artifact_type)
    .bind(payload_json)
    .execute(&state.db)
    .await;
}
