//! Aggregated Code Studio workbench view.

use axum::{
    extract::{Extension, Path as AxumPath, State},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::path::Path;

use crate::{auth::Claims, error::AppError, state::AppState};

use super::{
    complete_rd_task_if_no_pending_applyable_changes, ensure_task_access, get_task_row,
    language_for_path, rd_file_change_is_applyable, rd_runtime_timeout_secs, repository_root,
    row_to_change, row_to_test_run, should_skip_path,
    stale_tasks::reconcile_stale_rd_running_tasks, RdFileChangeDto, RdTaskDto, RdTestRunDto,
};

const WORKBENCH_FILE_TREE_LIMIT: usize = 800;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdWorkbenchResponse {
    task: RdTaskDto,
    agent_task: Option<Value>,
    runtime_session: Option<Value>,
    runtime_processes: Vec<Value>,
    runtime_artifacts: Vec<Value>,
    trace_events: Vec<Value>,
    rd_events: Vec<Value>,
    file_changes: Vec<RdFileChangeDto>,
    test_runs: Vec<RdTestRunDto>,
    file_tree: Vec<Value>,
    changed_file_groups: Vec<Value>,
    active_runtime_command: Option<Value>,
    terminal_output_preview: Option<Value>,
    linked_spec: Option<Value>,
    latest_answer: Option<String>,
    suggested_actions: Vec<String>,
}

pub(super) async fn get_task_workbench(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
) -> Result<Json<RdWorkbenchResponse>, AppError> {
    ensure_task_access(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    reconcile_stale_rd_running_tasks(
        &state.db,
        &claims.tenant_id,
        Some(&claims.sub),
        Some(&task_id),
        rd_runtime_timeout_secs(),
    )
    .await?;
    complete_rd_task_if_no_pending_applyable_changes(&state.db, &claims.tenant_id, &task_id)
        .await?;

    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let agent_task = load_agent_task(&state, &claims.tenant_id, &task_id).await?;
    let runtime_session = load_runtime_session(&state, &claims.tenant_id, &task_id).await?;
    let runtime_session_id = runtime_session
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let runtime_processes = match runtime_session_id.as_deref() {
        Some(session_id) => load_runtime_processes(&state, &claims.tenant_id, session_id).await?,
        None => Vec::new(),
    };
    let runtime_artifacts = match runtime_session_id.as_deref() {
        Some(session_id) => load_runtime_artifacts(&state, &claims.tenant_id, session_id).await?,
        None => Vec::new(),
    };
    let agent_task_id = agent_task
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let trace_events = match agent_task_id.as_deref() {
        Some(agent_task_id) => {
            load_agent_trace_events(&state, &claims.tenant_id, agent_task_id).await?
        }
        None => Vec::new(),
    };
    let rd_events = load_rd_events(&state, &claims.tenant_id, &task_id).await?;
    let file_changes = load_file_changes(&state, &claims.tenant_id, &task_id).await?;
    let test_runs = load_test_runs(&state, &claims.tenant_id, &task_id).await?;
    let file_tree = match task.repository_id.as_deref() {
        Some(repository_id) => load_repository_file_tree(&state, &claims, repository_id)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    task_id,
                    repository_id,
                    "failed to load workbench repository file tree, using changed summary fallback: {}",
                    error
                );
                build_file_tree_summary(&file_changes)
            }),
        None => build_file_tree_summary(&file_changes),
    };
    let changed_file_groups = build_changed_file_groups(&file_changes);
    let active_runtime_command = runtime_processes
        .iter()
        .find(|process| {
            process
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| matches!(status, "queued" | "running" | "cancelling"))
        })
        .cloned()
        .or_else(|| runtime_processes.first().cloned());
    let terminal_output_preview = active_runtime_command
        .as_ref()
        .map(build_terminal_output_preview);
    let linked_spec =
        load_linked_spec(&state, &claims.tenant_id, &task_id, task.spec_id.as_deref()).await?;
    let suggested_actions = suggested_actions_for(&task, &file_changes, &test_runs);
    let latest_answer = task
        .answer_md
        .clone()
        .or_else(|| task.review_md.clone())
        .or_else(|| task.plan_md.clone());

    Ok(Json(RdWorkbenchResponse {
        task,
        agent_task,
        runtime_session,
        runtime_processes,
        runtime_artifacts,
        trace_events,
        rd_events,
        file_changes,
        test_runs,
        file_tree,
        changed_file_groups,
        active_runtime_command,
        terminal_output_preview,
        linked_spec,
        latest_answer,
        suggested_actions,
    }))
}

async fn load_repository_file_tree(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
) -> Result<Vec<Value>, AppError> {
    let root = repository_root(state, claims, repository_id).await?;
    let mut budget = WORKBENCH_FILE_TREE_LIMIT;
    build_repository_tree_values(&root, &root, &mut budget)
}

async fn load_agent_task(
    state: &AppState,
    tenant_id: &str,
    rd_task_id: &str,
) -> Result<Option<Value>, AppError> {
    let row = sqlx::query(
        r"
        SELECT id, source, source_ref, source_label, capability_key, agent_id, agent_name,
               status, phase, progress_percent, title, summary, owner_user_id,
               linked_resource_type, linked_resource_id, error_message, last_event,
               CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
        FROM agent_tasks
        WHERE tenant_id = ? AND linked_resource_type = 'rd_task' AND linked_resource_id = ?
        ORDER BY updated_at DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(rd_task_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|row| {
        json!({
            "id": row.get::<String, _>("id"),
            "source": row.get::<String, _>("source"),
            "sourceRef": row.get::<Option<String>, _>("source_ref"),
            "sourceLabel": row.get::<Option<String>, _>("source_label"),
            "capabilityKey": row.get::<String, _>("capability_key"),
            "agentId": row.get::<Option<String>, _>("agent_id"),
            "agentName": row.get::<Option<String>, _>("agent_name"),
            "status": row.get::<String, _>("status"),
            "phase": row.get::<String, _>("phase"),
            "progressPercent": row.get::<i32, _>("progress_percent"),
            "title": row.get::<String, _>("title"),
            "summary": row.get::<Option<String>, _>("summary"),
            "ownerUserId": row.get::<Option<String>, _>("owner_user_id"),
            "linkedResourceType": row.get::<Option<String>, _>("linked_resource_type"),
            "linkedResourceId": row.get::<Option<String>, _>("linked_resource_id"),
            "errorMessage": row.get::<Option<String>, _>("error_message"),
            "lastEvent": row.get::<Option<String>, _>("last_event"),
            "createdAt": row.get::<String, _>("created_at"),
            "updatedAt": row.get::<String, _>("updated_at"),
        })
    }))
}

async fn load_runtime_session(
    state: &AppState,
    tenant_id: &str,
    rd_task_id: &str,
) -> Result<Option<Value>, AppError> {
    let row = sqlx::query(
        r"
        SELECT ars.id, ars.status, ars.workspace_root, ars.isolation_mode, ars.cancel_requested,
               CAST(ars.heartbeat_at AS TEXT) AS heartbeat_at,
               CAST(ars.started_at AS TEXT) AS started_at,
               CAST(ars.completed_at AS TEXT) AS completed_at
        FROM agent_runtime_sessions ars
        JOIN agent_tasks at ON at.tenant_id = ars.tenant_id AND at.id = ars.agent_task_id
        WHERE ars.tenant_id = ?
          AND at.linked_resource_type = 'rd_task'
          AND at.linked_resource_id = ?
        ORDER BY ars.created_at DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(rd_task_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|row| {
        json!({
            "id": row.get::<String, _>("id"),
            "status": row.get::<String, _>("status"),
            "workspaceRoot": row.get::<String, _>("workspace_root"),
            "isolationMode": row.get::<String, _>("isolation_mode"),
            "cancelRequested": row.get::<bool, _>("cancel_requested"),
            "heartbeatAt": row.get::<Option<String>, _>("heartbeat_at"),
            "startedAt": row.get::<Option<String>, _>("started_at"),
            "completedAt": row.get::<Option<String>, _>("completed_at"),
        })
    }))
}

async fn load_runtime_processes(
    state: &AppState,
    tenant_id: &str,
    runtime_session_id: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        r"
        SELECT id, command, cwd, status, pid, process_group_id, exit_code,
               stdout_preview, stderr_preview,
               CAST(started_at AS TEXT) AS started_at,
               CAST(completed_at AS TEXT) AS completed_at,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_runtime_processes
        WHERE tenant_id = ? AND runtime_session_id = ?
        ORDER BY created_at DESC
        LIMIT 50
        ",
    )
    .bind(tenant_id)
    .bind(runtime_session_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "command": row.get::<String, _>("command"),
                "cwd": row.get::<String, _>("cwd"),
                "status": row.get::<String, _>("status"),
                "pid": row.get::<Option<i64>, _>("pid"),
                "processGroupId": row.get::<Option<i64>, _>("process_group_id"),
                "exitCode": row.get::<Option<i32>, _>("exit_code"),
                "stdoutPreview": row.get::<Option<String>, _>("stdout_preview"),
                "stderrPreview": row.get::<Option<String>, _>("stderr_preview"),
                "startedAt": row.get::<Option<String>, _>("started_at"),
                "completedAt": row.get::<Option<String>, _>("completed_at"),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect())
}

async fn load_runtime_artifacts(
    state: &AppState,
    tenant_id: &str,
    runtime_session_id: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        r"
        SELECT id, artifact_type, path, content_text, content_hash, size_bytes,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_runtime_artifacts
        WHERE tenant_id = ? AND runtime_session_id = ?
        ORDER BY created_at DESC
        LIMIT 50
        ",
    )
    .bind(tenant_id)
    .bind(runtime_session_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "artifactType": row.get::<String, _>("artifact_type"),
                "path": row.get::<Option<String>, _>("path"),
                "contentText": row.get::<Option<String>, _>("content_text"),
                "contentHash": row.get::<Option<String>, _>("content_hash"),
                "sizeBytes": row.try_get::<u64, _>("size_bytes").ok().unwrap_or_default(),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect())
}

async fn load_linked_spec(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    task_spec_id: Option<&str>,
) -> Result<Option<Value>, AppError> {
    let row = sqlx::query(
        r"
        SELECT s.id, s.title, s.current_stage, s.status,
               l.task_item_id, l.status AS link_status,
               CAST(s.updated_at AS TEXT) AS updated_at
        FROM rd_specs s
        LEFT JOIN rd_spec_task_links l
          ON l.tenant_id = s.tenant_id
         AND l.spec_id = s.id
         AND l.rd_task_id = ?
        WHERE s.tenant_id = ?
          AND (
            s.id = ?
            OR l.rd_task_id = ?
          )
        ORDER BY l.updated_at DESC, s.updated_at DESC
        LIMIT 1
        ",
    )
    .bind(task_id)
    .bind(tenant_id)
    .bind(task_spec_id)
    .bind(task_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|row| {
        json!({
            "specId": row.get::<String, _>("id"),
            "title": row.get::<String, _>("title"),
            "stage": row.get::<String, _>("current_stage"),
            "status": row.get::<String, _>("status"),
            "taskItemId": row.get::<Option<String>, _>("task_item_id"),
            "linkStatus": row.get::<Option<String>, _>("link_status"),
            "updatedAt": row.get::<String, _>("updated_at"),
        })
    }))
}

async fn load_agent_trace_events(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        r"
        SELECT id, event_type, phase, status, severity, message,
               CAST(metadata_json AS TEXT) AS metadata_json,
               artifact_id, runtime_session_id, runtime_process_id,
               token_input, token_output, CAST(cost_usd AS DOUBLE) AS cost_usd,
               duration_ms,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_trace_events
        WHERE tenant_id = ? AND task_id = ?
        ORDER BY created_at DESC
        LIMIT 100
        ",
    )
    .bind(tenant_id)
    .bind(agent_task_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(agent_event_json).collect())
}

async fn load_rd_events(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        r"
        SELECT id, stage, status, message, CAST(detail_json AS TEXT) AS detail_json,
               CAST(created_at AS TEXT) AS created_at
        FROM rd_task_events
        WHERE tenant_id = ? AND task_id = ?
        ORDER BY id DESC
        LIMIT 100
        ",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<u64, _>("id"),
                "stage": row.get::<String, _>("stage"),
                "status": row.get::<String, _>("status"),
                "message": row.get::<Option<String>, _>("message"),
                "detailJson": parse_json_opt(row.get::<Option<String>, _>("detail_json")),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect())
}

async fn load_file_changes(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
) -> Result<Vec<RdFileChangeDto>, AppError> {
    let rows = sqlx::query("SELECT id, task_id, repository_id, file_path, change_type, diff_patch, applied, CAST(applied_at AS TEXT) applied_at, CAST(created_at AS TEXT) created_at FROM rd_file_changes WHERE task_id = ? AND tenant_id = ? ORDER BY created_at ASC")
        .bind(task_id)
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.iter().map(row_to_change).collect())
}

async fn load_test_runs(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
) -> Result<Vec<RdTestRunDto>, AppError> {
    let rows = sqlx::query("SELECT id, task_id, repository_id, command, status, exit_code, stdout_text, stderr_text, duration_ms, CAST(created_at AS TEXT) created_at FROM rd_test_runs WHERE task_id = ? AND tenant_id = ? ORDER BY created_at DESC LIMIT 50")
        .bind(task_id)
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.iter().map(row_to_test_run).collect())
}

fn agent_event_json(row: sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "id": row.get::<String, _>("id"),
        "eventType": row.get::<String, _>("event_type"),
        "phase": row.get::<Option<String>, _>("phase"),
        "status": row.get::<Option<String>, _>("status"),
        "severity": row.get::<String, _>("severity"),
        "message": row.get::<String, _>("message"),
        "metadataJson": parse_json_opt(row.get::<Option<String>, _>("metadata_json")),
        "artifactId": row.get::<Option<String>, _>("artifact_id"),
        "runtimeSessionId": row.get::<Option<String>, _>("runtime_session_id"),
        "runtimeProcessId": row.get::<Option<String>, _>("runtime_process_id"),
        "tokenInput": row.try_get::<Option<u64>, _>("token_input").ok().flatten(),
        "tokenOutput": row.try_get::<Option<u64>, _>("token_output").ok().flatten(),
        "costUsd": row.get::<Option<f64>, _>("cost_usd"),
        "durationMs": row.try_get::<Option<u64>, _>("duration_ms").ok().flatten(),
        "createdAt": row.get::<String, _>("created_at"),
    })
}

fn build_file_tree_summary(changes: &[RdFileChangeDto]) -> Vec<Value> {
    let mut roots = std::collections::BTreeMap::<String, (usize, usize)>::new();
    for change in changes {
        let root = change
            .file_path
            .split('/')
            .next()
            .filter(|part| !part.is_empty())
            .unwrap_or(&change.file_path)
            .to_string();
        let entry = roots.entry(root).or_insert((0, 0));
        entry.0 += 1;
        if !change.applied
            && rd_file_change_is_applyable(
                &change.change_type,
                &change.file_path,
                &change.diff_patch,
            )
        {
            entry.1 += 1;
        }
    }
    roots
        .into_iter()
        .map(|(name, (change_count, pending_count))| {
            json!({
                "name": name,
                "path": name,
                "nodeType": "directory",
                "changeCount": change_count,
                "pendingCount": pending_count,
            })
        })
        .collect()
}

fn build_repository_tree_values(
    root: &Path,
    dir: &Path,
    budget: &mut usize,
) -> Result<Vec<Value>, AppError> {
    if *budget == 0 {
        return Ok(Vec::new());
    }
    let mut entries = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|entry| !should_skip_path(&entry.path()))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    let mut nodes = Vec::new();
    for entry in entries {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let meta = entry.metadata()?;
        if meta.is_dir() {
            nodes.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "path": rel,
                "nodeType": "dir",
                "children": build_repository_tree_values(root, &path, budget)?,
            }));
        } else if meta.is_file() {
            nodes.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "path": rel,
                "nodeType": "file",
                "sizeBytes": meta.len(),
                "language": language_for_path(&path),
            }));
        }
    }
    Ok(nodes)
}

fn build_changed_file_groups(changes: &[RdFileChangeDto]) -> Vec<Value> {
    let mut groups = std::collections::BTreeMap::<String, Vec<&RdFileChangeDto>>::new();
    for change in changes {
        groups
            .entry(change.change_type.clone())
            .or_default()
            .push(change);
    }
    groups
        .into_iter()
        .map(|(change_type, items)| {
            let pending_count = items
                .iter()
                .filter(|change| {
                    !change.applied
                        && rd_file_change_is_applyable(
                            &change.change_type,
                            &change.file_path,
                            &change.diff_patch,
                        )
                })
                .count();
            json!({
                "changeType": change_type,
                "count": items.len(),
                "pendingCount": pending_count,
                "files": items.into_iter().map(|change| json!({
                    "id": change.id,
                    "filePath": change.file_path,
                    "applied": change.applied,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn build_terminal_output_preview(process: &Value) -> Value {
    json!({
        "processId": process.get("id").cloned().unwrap_or(Value::Null),
        "command": process.get("command").cloned().unwrap_or(Value::Null),
        "status": process.get("status").cloned().unwrap_or(Value::Null),
        "stdoutPreview": process.get("stdoutPreview").cloned().unwrap_or(Value::Null),
        "stderrPreview": process.get("stderrPreview").cloned().unwrap_or(Value::Null),
        "exitCode": process.get("exitCode").cloned().unwrap_or(Value::Null),
    })
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn suggested_actions_for(
    task: &RdTaskDto,
    changes: &[RdFileChangeDto],
    tests: &[RdTestRunDto],
) -> Vec<String> {
    let mut actions = Vec::new();
    if matches!(
        task.status.as_str(),
        "queued" | "running" | "waiting_approval"
    ) {
        actions.push("cancel".to_string());
    }
    if task.status == "failed" {
        actions.push("retry".to_string());
    }
    if changes.iter().any(|change| {
        !change.applied
            && rd_file_change_is_applyable(
                &change.change_type,
                &change.file_path,
                &change.diff_patch,
            )
    }) {
        actions.push("review_diff".to_string());
    }
    if tests.first().is_some_and(|test| test.status != "passed") {
        actions.push("fix_tests".to_string());
    }
    actions
}
