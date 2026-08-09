//! Sessions API — read-only session history from aos data directory.

use std::fs;

use axum::{
    extract::{Extension, Path, Query, State},
    routing::get as routing_get,
    Json, Router,
};
use serde::Serialize;
use walkdir::WalkDir;

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::PaginationParams;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub path: String,
    pub message_count: usize,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub model: Option<String>,
    pub compact_threshold: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionSummary>,
    pub total: usize,
}

#[derive(Debug)]
struct SessionBuildResult {
    session_id: String,
    path: String,
    message_count: usize,
    created_at: Option<String>,
    updated_at: Option<String>,
    model: Option<String>,
    compact_threshold: Option<u32>,
}

impl SessionBuildResult {
    fn from_jsonl_line(line: &str, path: &std::path::Path) -> Option<Self> {
        let data = serde_json::from_str::<serde_json::Value>(line).ok()?;
        let messages = data
            .get("messages")
            .and_then(|m| m.as_array())
            .map_or(0, Vec::len);
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        Some(SessionBuildResult {
            session_id: file_stem.to_string(),
            path: path.to_string_lossy().to_string(),
            message_count: messages,
            created_at: data
                .get("created_at_ms")
                .and_then(serde_json::Value::as_i64)
                .and_then(|ts| {
                    chrono::DateTime::from_timestamp_millis(ts).map(|dt| dt.to_rfc3339())
                }),
            updated_at: data
                .get("updated_at_ms")
                .and_then(serde_json::Value::as_i64)
                .and_then(|ts| {
                    chrono::DateTime::from_timestamp_millis(ts).map(|dt| dt.to_rfc3339())
                }),
            model: data.get("model").and_then(|v| v.as_str()).map(String::from),
            compact_threshold: data
                .get("compact_threshold")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
        })
    }

    fn from_json_content(content: &str, path: &std::path::Path) -> Option<Self> {
        let data = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let messages = data
            .get("messages")
            .and_then(|m| m.as_array())
            .map_or(0, Vec::len);
        let session_id = data
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
            });
        Some(SessionBuildResult {
            session_id: session_id.to_string(),
            path: path.to_string_lossy().to_string(),
            message_count: messages,
            created_at: data
                .get("created_at_ms")
                .and_then(serde_json::Value::as_i64)
                .and_then(|ts| {
                    chrono::DateTime::from_timestamp_millis(ts).map(|dt| dt.to_rfc3339())
                }),
            updated_at: data
                .get("updated_at_ms")
                .and_then(serde_json::Value::as_i64)
                .and_then(|ts| {
                    chrono::DateTime::from_timestamp_millis(ts).map(|dt| dt.to_rfc3339())
                }),
            model: data.get("model").and_then(|v| v.as_str()).map(String::from),
            compact_threshold: data
                .get("compact_threshold")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
        })
    }

    fn into_summary(self) -> SessionSummary {
        SessionSummary {
            session_id: self.session_id,
            path: self.path,
            message_count: self.message_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
            model: self.model,
            compact_threshold: self.compact_threshold,
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<SessionListResponse>> {
    // Sessions are stored per-tenant per-user at .aos/{tenant_id}/{user_id}/
    // This mirrors the layout used by chat.rs for write operations.
    let sessions_root = state
        .data_dir
        .join(".aos")
        .join(&claims.tenant_id)
        .join(&claims.sub);

    if !sessions_root.exists() {
        return Ok(Json(SessionListResponse {
            sessions: Vec::new(),
            total: 0,
        }));
    }

    let mut sessions = Vec::new();

    for entry in WalkDir::new(&sessions_root)
        .max_depth(2)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        let is_jsonl = path.extension().is_some_and(|e| e == "jsonl");
        let is_json = path.extension().is_some_and(|e| e == "json");
        if path.is_file() && (is_jsonl || is_json) {
            if let Ok(content) = fs::read_to_string(path) {
                if is_jsonl {
                    if let Some(result) = SessionBuildResult::from_jsonl_line(
                        content.lines().next().unwrap_or(""),
                        path,
                    ) {
                        sessions.push(result.into_summary());
                    }
                } else if let Some(result) = SessionBuildResult::from_json_content(&content, path) {
                    sessions.push(result.into_summary());
                }
            }
        }
    }

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let total = sessions.len();
    let offset = usize::try_from(pagination.offset()).unwrap_or(0);
    let limit = usize::try_from(pagination.limit()).unwrap_or(0);
    let page = sessions.into_iter().skip(offset).take(limit).collect();

    Ok(Json(SessionListResponse {
        sessions: page,
        total,
    }))
}

async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    // Sessions are stored per-tenant per-user at .aos/{tenant_id}/{user_id}/
    let sessions_root = state
        .data_dir
        .join(".aos")
        .join(&claims.tenant_id)
        .join(&claims.sub);

    // Try .jsonl first (new format), then .json (legacy).
    let jsonl_path = sessions_root.join(format!("{session_id}.jsonl"));
    if jsonl_path.exists() {
        // .jsonl: read all lines as separate JSON objects and reconstruct as array.
        let content = fs::read_to_string(&jsonl_path)?;
        let messages: Vec<serde_json::Value> = content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        return Ok(Json(serde_json::json!({
            "session_id": session_id,
            "messages": messages,
        })));
    }

    let json_path = sessions_root.join(format!("{session_id}.json"));
    if json_path.exists() {
        let content = fs::read_to_string(&json_path)?;
        let data: serde_json::Value = serde_json::from_str(&content)?;
        return Ok(Json(data));
    }

    // Fallback: search recursively in case the session exists elsewhere.
    for entry in WalkDir::new(&sessions_root)
        .max_depth(2)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if entry.path().is_file() && entry.path().extension().is_some_and(|e| e == "json") {
            if let Ok(content) = fs::read_to_string(entry.path()) {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                    if data.get("session_id").and_then(|v| v.as_str()) == Some(&session_id) {
                        return Ok(Json(data));
                    }
                }
            }
        }
    }

    Err(AppError::NotFound(format!(
        "session '{session_id}' not found"
    )))
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/{session_id}", routing_get(get))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}
