use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    Json,
};
use sqlx::Row;

use crate::auth::Claims;
use crate::error::AppError;
use crate::state::AppState;

use super::{
    complete_rd_task_if_no_pending_applyable_changes, ensure_task_access, get_task_row,
    rd_runtime_timeout_secs, row_to_change, row_to_task, row_to_test_run,
    stale_tasks::reconcile_stale_rd_running_tasks, RdFileChangeDto, RdTaskDto, RdTaskListQuery,
    RdTaskListResponse, RdTestRunDto,
};

pub(super) async fn list_tasks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<RdTaskListQuery>,
) -> Result<Json<RdTaskListResponse>, AppError> {
    reconcile_stale_rd_running_tasks(
        &state.db,
        &claims.tenant_id,
        Some(&claims.sub),
        None,
        rd_runtime_timeout_secs(),
    )
    .await?;
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let offset = i64::from((page - 1) * per_page);
    let limit = i64::from(per_page);
    let mut where_sql = "tenant_id = ? AND user_id = ?".to_string();
    if query.status.is_some() {
        where_sql.push_str(" AND status = ?");
    }
    if query.repository_id.is_some() {
        where_sql.push_str(" AND repository_id = ?");
    }
    if query.mode.is_some() {
        where_sql.push_str(" AND mode = ?");
    }
    let count_sql = format!("SELECT COUNT(*) AS c FROM rd_tasks WHERE {where_sql}");
    let mut count_query = sqlx::query(sqlx::AssertSqlSafe(count_sql))
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    if let Some(v) = &query.status {
        count_query = count_query.bind(v);
    }
    if let Some(v) = &query.repository_id {
        count_query = count_query.bind(v);
    }
    if let Some(v) = &query.mode {
        count_query = count_query.bind(v);
    }
    let total = count_query
        .fetch_one(&state.db)
        .await?
        .get::<i64, _>("c")
        .max(0) as u64;
    let list_sql = format!(
        "SELECT id, thread_id, parent_task_id, iteration_no, thread_title, repository_id, spec_id, agent_profile_id, workflow_id, runtime_session_id, mode, context_profile, context_depth, should_deep_scan, status, title, prompt, model, plan_md, answer_md, review_md, pr_title, pr_description, error_message, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at, CAST(completed_at AS TEXT) completed_at FROM rd_tasks WHERE {where_sql} ORDER BY created_at DESC LIMIT ? OFFSET ?"
    );
    let mut list_query = sqlx::query(sqlx::AssertSqlSafe(list_sql))
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    if let Some(v) = &query.status {
        list_query = list_query.bind(v);
    }
    if let Some(v) = &query.repository_id {
        list_query = list_query.bind(v);
    }
    if let Some(v) = &query.mode {
        list_query = list_query.bind(v);
    }
    let rows = list_query
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(RdTaskListResponse {
        tasks: rows.iter().map(row_to_task).collect(),
        total,
    }))
}

pub(super) async fn get_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
) -> Result<Json<RdTaskDto>, AppError> {
    reconcile_stale_rd_running_tasks(
        &state.db,
        &claims.tenant_id,
        Some(&claims.sub),
        Some(&task_id),
        rd_runtime_timeout_secs(),
    )
    .await?;
    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    if task.status == "waiting_approval" {
        complete_rd_task_if_no_pending_applyable_changes(&state.db, &claims.tenant_id, &task_id)
            .await?;
        return get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id)
            .await
            .map(Json);
    }
    Ok(Json(task))
}

pub(super) async fn task_changes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
) -> Result<Json<Vec<RdFileChangeDto>>, AppError> {
    ensure_task_access(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    complete_rd_task_if_no_pending_applyable_changes(&state.db, &claims.tenant_id, &task_id)
        .await?;
    let rows = sqlx::query("SELECT id, task_id, repository_id, file_path, change_type, diff_patch, applied, CAST(applied_at AS TEXT) applied_at, CAST(created_at AS TEXT) created_at FROM rd_file_changes WHERE task_id = ? AND tenant_id = ? ORDER BY created_at ASC")
        .bind(&task_id)
        .bind(&claims.tenant_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows.iter().map(row_to_change).collect()))
}

pub(super) async fn task_tests(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
) -> Result<Json<Vec<RdTestRunDto>>, AppError> {
    ensure_task_access(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let rows = sqlx::query("SELECT id, task_id, repository_id, command, status, exit_code, stdout_text, stderr_text, duration_ms, CAST(created_at AS TEXT) created_at FROM rd_test_runs WHERE task_id = ? AND tenant_id = ? ORDER BY created_at DESC LIMIT 50")
        .bind(&task_id)
        .bind(&claims.tenant_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows.iter().map(row_to_test_run).collect()))
}
