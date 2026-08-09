use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::{auth::Claims, error::AppError, state::AppState};

use super::{
    ensure_task_access, rd_runtime_timeout_secs, stale_tasks::reconcile_stale_rd_running_tasks,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RdTaskEventDto {
    id: u64,
    stage: String,
    status: String,
    message: Option<String>,
    detail_json: Option<Value>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdTaskEventListQuery {
    #[serde(alias = "perPage")]
    per_page: Option<u32>,
    #[serde(alias = "cursorBefore")]
    cursor_before: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskEventListResponse {
    events: Vec<RdTaskEventDto>,
    has_more: bool,
    next_cursor: Option<u64>,
    page_size: u32,
}

pub(super) async fn list_task_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<RdTaskEventListQuery>,
) -> Result<Json<RdTaskEventListResponse>, AppError> {
    ensure_task_access(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    reconcile_stale_rd_running_tasks(
        &state.db,
        &claims.tenant_id,
        Some(&claims.sub),
        Some(&task_id),
        rd_runtime_timeout_secs(),
    )
    .await?;
    let page_size = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = i64::from(page_size) + 1;
    let ids = fetch_task_event_page_ids(
        &state.db,
        &claims.tenant_id,
        &task_id,
        query.cursor_before,
        limit,
    )
    .await?;
    let has_more = ids.len() > page_size as usize;
    let page_ids = ids
        .iter()
        .take(page_size as usize)
        .copied()
        .collect::<Vec<_>>();
    let rows =
        fetch_task_event_rows_by_ids(&state.db, &claims.tenant_id, &task_id, &page_ids).await?;
    let events = rows.iter().map(row_to_event).collect::<Vec<_>>();
    let next_cursor = if has_more {
        events.last().map(|event| event.id)
    } else {
        None
    };
    Ok(Json(RdTaskEventListResponse {
        events,
        has_more,
        next_cursor,
        page_size,
    }))
}

pub(super) async fn task_token_diagnostics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
) -> Result<Json<RdTaskEventListResponse>, AppError> {
    ensure_task_access(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    reconcile_stale_rd_running_tasks(
        &state.db,
        &claims.tenant_id,
        Some(&claims.sub),
        Some(&task_id),
        rd_runtime_timeout_secs(),
    )
    .await?;
    let stages = [
        ("context_cache_usage", 3_i64),
        ("context_retrieval_evidence", 3_i64),
        ("runtime_tool_governance", 5_i64),
        ("runtime_usage", 10_i64),
        ("context_plan_llm", 10_i64),
        ("runtime", 5_i64),
        ("summary", 5_i64),
    ];
    let mut seen_ids = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for (stage, limit) in stages {
        for row in fetch_latest_task_event_rows_by_stage(
            &state.db,
            &claims.tenant_id,
            &task_id,
            stage,
            limit,
        )
        .await?
        {
            let id: u64 = row.get("id");
            if seen_ids.insert(id) {
                rows.push(row);
            }
        }
    }
    rows.sort_by(|left, right| {
        let left_id: u64 = left.get("id");
        let right_id: u64 = right.get("id");
        right_id.cmp(&left_id)
    });
    let page_size = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let events = rows.iter().map(row_to_event).collect::<Vec<_>>();
    Ok(Json(RdTaskEventListResponse {
        events,
        has_more: false,
        next_cursor: None,
        page_size,
    }))
}

async fn fetch_task_event_page_ids(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    cursor_before: Option<u64>,
    limit: i64,
) -> Result<Vec<u64>, AppError> {
    let rows = if let Some(cursor_before) = cursor_before {
        sqlx::query::<sqlx::Sqlite>(
            "SELECT id \
             FROM rd_task_events \
             WHERE task_id = ? AND tenant_id = ? AND id < ? \
             ORDER BY id DESC LIMIT ?",
        )
        .bind(task_id)
        .bind(tenant_id)
        .bind(crate::sqlite_i64(cursor_before))
        .bind(limit)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query::<sqlx::Sqlite>(
            "SELECT id \
             FROM rd_task_events \
             WHERE task_id = ? AND tenant_id = ? \
             ORDER BY id DESC LIMIT ?",
        )
        .bind(task_id)
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(db)
        .await?
    };
    Ok(rows.iter().map(|row| row.get("id")).collect())
}

async fn fetch_latest_task_event_rows_by_stage(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    stage: &str,
    limit: i64,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, AppError> {
    Ok(sqlx::query::<sqlx::Sqlite>(
        "SELECT id, stage, status, message, CAST(detail_json AS TEXT) detail_json, CAST(created_at AS TEXT) created_at \
         FROM rd_task_events \
         WHERE task_id = ? AND tenant_id = ? AND stage = ? \
         ORDER BY id DESC LIMIT ?",
    )
    .bind(task_id)
    .bind(tenant_id)
    .bind(stage)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

async fn fetch_task_event_rows_by_ids(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    ids: &[u64],
) -> Result<Vec<sqlx::sqlite::SqliteRow>, AppError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = QueryBuilder::<Sqlite>::new(
        "SELECT id, stage, status, message, CAST(detail_json AS TEXT) detail_json, CAST(created_at AS TEXT) created_at \
         FROM rd_task_events WHERE tenant_id = ",
    );
    builder
        .push_bind(tenant_id)
        .push(" AND task_id = ")
        .push_bind(task_id)
        .push(" AND id IN (");
    let mut separated = builder.separated(", ");
    for id in ids {
        separated.push_bind(crate::sqlite_i64(*id));
    }
    separated.push_unseparated(") ORDER BY id DESC");
    Ok(builder.build().fetch_all(db).await?)
}

fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> RdTaskEventDto {
    RdTaskEventDto {
        id: row.get("id"),
        stage: row.get("stage"),
        status: row.get("status"),
        message: row.get("message"),
        detail_json: parse_optional_json_text(row, "detail_json"),
        created_at: row.get("created_at"),
    }
}

fn parse_optional_json_text(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<Value> {
    let raw = match row.try_get::<Option<String>, _>(column) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                column,
                error = %error,
                "failed to decode optional JSON text column"
            );
            return Some(json!({
                "decodeWarning": "json_column_decode_failed",
                "column": column,
                "message": error.to_string(),
            }));
        }
    };
    let Some(raw) = raw else {
        return None;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
        return None;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Null) => None,
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(
                column,
                error = %error,
                "failed to parse optional JSON text column"
            );
            Some(json!({
                "decodeWarning": "json_column_parse_failed",
                "column": column,
                "message": error.to_string(),
                "rawPreview": trimmed.chars().take(500).collect::<String>(),
            }))
        }
    }
}
