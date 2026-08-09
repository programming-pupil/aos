//! Task action handlers for applying diffs, rolling back, hunks, and tests.

use super::*;

async fn record_git_worktree_snapshot_before_apply(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: &str,
    root: &Path,
    target_paths: &[String],
) -> Result<(), AppError> {
    let dirty_paths = match git_dirty_paths(root).await {
        Ok(paths) => paths,
        Err(error) => {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "git_worktree",
                "failed",
                "读取 Git 工作区状态失败，已继续依赖 git apply --check",
                json!({
                    "repositoryId": repository_id,
                    "targetPaths": target_paths,
                    "error": error.to_string(),
                }),
            )
            .await?;
            return Ok(());
        }
    };
    if dirty_paths.is_empty() {
        return Ok(());
    }
    let target_dirty = target_paths
        .iter()
        .filter(|path| dirty_paths.contains(path.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "git_worktree",
        "completed",
        if target_dirty.is_empty() {
            "应用前检测到仓库存在未提交变更，目标文件未命中"
        } else {
            "应用前检测到目标文件存在未提交变更"
        },
        json!({
            "repositoryId": repository_id,
            "targetPaths": target_paths,
            "targetDirtyPaths": target_dirty,
            "dirtyPathCount": dirty_paths.len(),
            "dirtyPathsSample": dirty_paths.iter().take(30).cloned().collect::<Vec<_>>(),
            "blocking": false,
        }),
    )
    .await?;
    Ok(())
}

pub(super) async fn apply_task_changes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
    Json(req): Json<RdTaskApplyRequest>,
) -> Result<Json<RdTaskApplyResponse>, AppError> {
    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let Some(repo_id) = task.repository_id.as_deref() else {
        return Err(AppError::ValidationError(
            "task has no repository".to_string(),
        ));
    };
    let root = repository_root(&state, &claims, repo_id).await?;
    let rows = sqlx::query("SELECT id, file_path, change_type, diff_patch, applied FROM rd_file_changes WHERE task_id = ? AND tenant_id = ? ORDER BY created_at ASC")
        .bind(&task_id)
        .bind(&claims.tenant_id)
        .fetch_all(&state.db)
        .await?;
    let selected = req
        .change_ids
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let apply_all = selected.is_empty();
    let mut applied = 0usize;
    let mut skipped = 0usize;
    for row in rows {
        let id: String = row.get("id");
        let file_path: String = row.get("file_path");
        let change_type: String = row.get("change_type");
        let already: bool = row.get("applied");
        if already || (!apply_all && !selected.contains(&id)) {
            skipped += 1;
            continue;
        }
        let patch: String = row.get("diff_patch");
        if !rd_file_change_is_applyable(&change_type, &file_path, &patch) {
            skipped += 1;
            record_event(
                &state.db,
                &claims.tenant_id,
                &task_id,
                "apply",
                "skipped",
                "已跳过不可应用或包含运行时路径的 Diff",
                json!({
                    "changeId": id,
                    "filePath": file_path,
                    "changeType": change_type,
                    "reason": "not_applyable_or_runtime_path",
                }),
            )
            .await?;
            continue;
        }
        record_git_worktree_snapshot_before_apply(
            &state,
            &claims,
            &task_id,
            repo_id,
            &root,
            &[file_path.clone()],
        )
        .await?;
        run_rd_hook(
            &state,
            &claims,
            HookEventType::PreToolUse,
            "rd.git_apply",
            json!({
                "taskId": task_id.clone(),
                "repositoryId": repo_id,
                "changeId": id.clone(),
                "checkOnly": false,
            }),
            None,
            false,
            true,
        )
        .await?;
        git_apply(&root, &patch, true).await?;
        git_apply(&root, &patch, false).await?;
        sqlx::query("UPDATE rd_file_changes SET applied = 1, applied_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ?")
            .bind(&id)
            .bind(&claims.tenant_id)
            .execute(&state.db)
            .await?;
        mark_rd_patch_ownership_applied(&state.db, &claims.tenant_id, &id, true).await?;
        run_rd_hook(
            &state,
            &claims,
            HookEventType::PostToolUse,
            "rd.git_apply",
            json!({
                "taskId": task_id.clone(),
                "repositoryId": repo_id,
                "changeId": id.clone(),
            }),
            Some(json!({"status": "applied"})),
            false,
            false,
        )
        .await?;
        applied += 1;
    }
    record_event(
        &state.db,
        &claims.tenant_id,
        &task_id,
        "apply",
        "completed",
        "代码修改已应用",
        json!({"applied": applied, "skipped": skipped}),
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        Some(repo_id),
        Some(&task_id),
        "diff_applied_count",
        applied as f64,
        json!({"skipped": skipped, "applyMode": if apply_all { "all" } else { "selected" }}),
    )
    .await?;
    complete_rd_task_if_no_pending_applyable_changes(&state.db, &claims.tenant_id, &task_id)
        .await?;
    Ok(Json(RdTaskApplyResponse { applied, skipped }))
}

pub(super) async fn rollback_task_changes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
    Json(req): Json<RdTaskApplyRequest>,
) -> Result<Json<RdTaskRollbackResponse>, AppError> {
    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let Some(repo_id) = task.repository_id.as_deref() else {
        return Err(AppError::ValidationError(
            "task has no repository".to_string(),
        ));
    };
    let root = repository_root(&state, &claims, repo_id).await?;
    let rows = sqlx::query("SELECT id, diff_patch, applied FROM rd_file_changes WHERE task_id = ? AND tenant_id = ? ORDER BY applied_at DESC, created_at DESC")
        .bind(&task_id)
        .bind(&claims.tenant_id)
        .fetch_all(&state.db)
        .await?;
    let selected = req
        .change_ids
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let rollback_all = selected.is_empty();
    let mut candidates = Vec::new();
    let mut skipped = 0usize;
    for row in rows {
        let id: String = row.get("id");
        let applied: bool = row.get("applied");
        if !applied || (!rollback_all && !selected.contains(&id)) {
            skipped += 1;
            continue;
        }
        let patch: String = row.get("diff_patch");
        candidates.push((id, patch));
    }

    for (id, patch) in &candidates {
        git_apply_reverse(&root, patch, true)
            .await
            .map_err(|error| {
                AppError::ValidationError(format!(
                    "rollback precheck failed for change {id}: {error}"
                ))
            })?;
    }

    let mut rolled_back = 0usize;
    for (id, patch) in candidates {
        run_rd_hook(
            &state,
            &claims,
            HookEventType::PreToolUse,
            "rd.git_apply_reverse",
            json!({
                "taskId": task_id.clone(),
                "repositoryId": repo_id,
                "changeId": id.clone(),
                "checkOnly": false,
            }),
            None,
            false,
            true,
        )
        .await?;
        git_apply_reverse(&root, &patch, false).await?;
        sqlx::query("UPDATE rd_file_changes SET applied = 0, applied_at = NULL WHERE id = ? AND tenant_id = ?")
            .bind(&id)
            .bind(&claims.tenant_id)
            .execute(&state.db)
            .await?;
        mark_rd_patch_ownership_applied(&state.db, &claims.tenant_id, &id, false).await?;
        run_rd_hook(
            &state,
            &claims,
            HookEventType::PostToolUse,
            "rd.git_apply_reverse",
            json!({
                "taskId": task_id.clone(),
                "repositoryId": repo_id,
                "changeId": id.clone(),
            }),
            Some(json!({"status": "rolled_back"})),
            false,
            false,
        )
        .await?;
        rolled_back += 1;
    }

    record_event(
        &state.db,
        &claims.tenant_id,
        &task_id,
        "rollback",
        "completed",
        "已回滚任务应用的代码修改",
        json!({"rolledBack": rolled_back, "skipped": skipped}),
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        Some(repo_id),
        Some(&task_id),
        "diff_rolled_back_count",
        rolled_back as f64,
        json!({"skipped": skipped, "rollbackMode": if rollback_all { "all" } else { "selected" }}),
    )
    .await?;
    if rolled_back > 0 {
        reopen_rd_task_if_pending_applyable_changes(&state.db, &claims.tenant_id, &task_id).await?;
    }
    Ok(Json(RdTaskRollbackResponse {
        rolled_back,
        skipped,
    }))
}

pub(super) async fn apply_task_hunks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
    Json(req): Json<RdTaskApplyHunksRequest>,
) -> Result<Json<RdTaskApplyHunksResponse>, AppError> {
    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let Some(repo_id) = task.repository_id.as_deref() else {
        return Err(AppError::ValidationError(
            "task has no repository".to_string(),
        ));
    };
    if req.hunk_indexes.is_empty() {
        return Err(AppError::ValidationError(
            "at least one hunk must be selected".to_string(),
        ));
    }
    let row = sqlx::query("SELECT id, repository_id, file_path, change_type, diff_patch, applied FROM rd_file_changes WHERE id = ? AND task_id = ? AND tenant_id = ?")
        .bind(&req.change_id)
        .bind(&task_id)
        .bind(&claims.tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("rd file change not found".to_string()))?;
    let already: bool = row.get("applied");
    if already {
        return Err(AppError::ValidationError(
            "change has already been applied".to_string(),
        ));
    }
    let file_path: String = row.get("file_path");
    let change_type: String = row.get("change_type");
    let patch: String = row.get("diff_patch");
    if !rd_file_change_is_applyable(&change_type, &file_path, &patch) {
        return Err(AppError::ValidationError(
            "change is not applyable or contains runtime/internal paths".to_string(),
        ));
    }
    let split = split_unified_diff_hunks(&patch)?;
    let selected_indexes = req.hunk_indexes.into_iter().collect::<BTreeSet<_>>();
    let selected_patch = build_patch_from_hunks(&split, &selected_indexes)?;
    let remaining_indexes = (0..split.hunks.len())
        .filter(|idx| !selected_indexes.contains(idx))
        .collect::<BTreeSet<_>>();
    let remaining_patch = if remaining_indexes.is_empty() {
        None
    } else {
        Some(build_patch_from_hunks(&split, &remaining_indexes)?)
    };

    let root = repository_root(&state, &claims, repo_id).await?;
    record_git_worktree_snapshot_before_apply(
        &state,
        &claims,
        &task_id,
        repo_id,
        &root,
        &[file_path.clone()],
    )
    .await?;
    run_rd_hook(
        &state,
        &claims,
        HookEventType::PreToolUse,
        "rd.git_apply_hunks",
        json!({
            "taskId": task_id.clone(),
            "repositoryId": repo_id,
            "changeId": req.change_id.clone(),
            "hunkIndexes": selected_indexes.iter().copied().collect::<Vec<_>>(),
            "checkOnly": false,
        }),
        None,
        false,
        true,
    )
    .await?;
    git_apply(&root, &selected_patch, true).await?;
    git_apply(&root, &selected_patch, false).await?;

    let remaining_change_id = if let Some(remaining_patch) = remaining_patch {
        let remaining_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("UPDATE rd_file_changes SET diff_patch = ?, change_type = 'partial_applied', applied = 1, applied_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ?")
            .bind(&selected_patch)
            .bind(&req.change_id)
            .bind(&claims.tenant_id)
            .execute(&state.db)
            .await?;
        mark_rd_patch_ownership_applied(&state.db, &claims.tenant_id, &req.change_id, true).await?;
        sqlx::query("INSERT INTO rd_file_changes (id, task_id, tenant_id, repository_id, file_path, change_type, diff_patch) VALUES (?, ?, ?, ?, ?, 'partial_remaining', ?)")
            .bind(&remaining_id)
            .bind(&task_id)
            .bind(&claims.tenant_id)
            .bind(row.get::<Option<String>, _>("repository_id"))
            .bind(&file_path)
            .bind(&remaining_patch)
            .execute(&state.db)
            .await?;
        record_rd_patch_ownerships(
            &state.db,
            &claims.tenant_id,
            Some(repo_id),
            &task_id,
            &remaining_id,
            std::slice::from_ref(&file_path),
            &remaining_patch,
        )
        .await?;
        Some(remaining_id)
    } else {
        sqlx::query("UPDATE rd_file_changes SET applied = 1, applied_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ?")
            .bind(&req.change_id)
            .bind(&claims.tenant_id)
            .execute(&state.db)
            .await?;
        mark_rd_patch_ownership_applied(&state.db, &claims.tenant_id, &req.change_id, true).await?;
        None
    };

    run_rd_hook(
        &state,
        &claims,
        HookEventType::PostToolUse,
        "rd.git_apply_hunks",
        json!({
            "taskId": task_id.clone(),
            "repositoryId": repo_id,
            "changeId": req.change_id,
            "hunkIndexes": selected_indexes.iter().copied().collect::<Vec<_>>(),
        }),
        Some(json!({"status": "applied", "remainingChangeId": remaining_change_id.clone()})),
        false,
        false,
    )
    .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        &task_id,
        "apply",
        "completed",
        "代码修改块已应用",
        json!({
            "changeId": row.get::<String, _>("id"),
            "appliedHunks": selected_indexes.len(),
            "totalHunks": split.hunks.len(),
            "remainingChangeId": remaining_change_id.clone(),
        }),
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        Some(repo_id),
        Some(&task_id),
        "diff_applied_hunk_count",
        selected_indexes.len() as f64,
        json!({
            "changeId": row.get::<String, _>("id"),
            "totalHunks": split.hunks.len(),
            "remainingChangeId": remaining_change_id.clone(),
        }),
    )
    .await?;
    complete_rd_task_if_no_pending_applyable_changes(&state.db, &claims.tenant_id, &task_id)
        .await?;
    Ok(Json(RdTaskApplyHunksResponse {
        applied_hunks: selected_indexes.len(),
        total_hunks: split.hunks.len(),
        remaining_change_id,
    }))
}

pub(super) async fn run_task_test(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
    Json(req): Json<RdTaskTestRequest>,
) -> Result<Json<RdTestRunDto>, AppError> {
    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let Some(repo_id) = task.repository_id.as_deref() else {
        return Err(AppError::ValidationError(
            "task has no repository".to_string(),
        ));
    };
    let root = repository_root(&state, &claims, repo_id).await?;
    let saved_setting =
        load_repo_setting(&state.db, &claims.tenant_id, &claims.sub, repo_id).await?;
    let command = req
        .command
        .or_else(|| saved_setting.and_then(|s| s.0))
        .ok_or_else(|| AppError::ValidationError("test command is required".to_string()))?;
    reject_dangerous_command(&command)?;
    run_rd_hook(
        &state,
        &claims,
        HookEventType::PreToolUse,
        "rd.test_command",
        json!({
            "taskId": task_id.clone(),
            "repositoryId": repo_id,
            "command": command.clone(),
        }),
        None,
        false,
        true,
    )
    .await?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let started = Instant::now();
    let timeout_secs = rd_test_command_timeout_secs();
    let agent_runtime_session_id =
        crate::routes::agent_ops::find_runtime_session_for_linked_resource(
            &state,
            &claims.tenant_id,
            "rd_task",
            &task_id,
        )
        .await?;
    let output = if let Some(runtime_session_id) = agent_runtime_session_id.as_deref() {
        let runtime_repo_dir = sqlx::query(
            "SELECT workspace_root FROM agent_runtime_sessions WHERE tenant_id = ? AND id = ?",
        )
        .bind(&claims.tenant_id)
        .bind(runtime_session_id)
        .fetch_optional(&state.db)
        .await?
        .map(|row| PathBuf::from(row.get::<String, _>("workspace_root")).join("workspace/repo"));
        match runtime_repo_dir {
            Some(dir) if dir.exists() => {
                run_command_in_dir_with_agent_runtime(
                    &state,
                    &claims.tenant_id,
                    runtime_session_id,
                    &dir,
                    &command,
                )
                .await
            }
            _ => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    &task_id,
                    "test",
                    "running",
                    "未找到 Agent Runtime 隔离仓库，手动测试回退到仓库目录执行",
                    json!({"agentRuntimeSessionId": runtime_session_id}),
                )
                .await?;
                run_command_in_dir(&root, &command).await
            }
        }
    } else {
        record_event(
            &state.db,
            &claims.tenant_id,
            &task_id,
            "test",
            "running",
            "当前任务未绑定 Agent Runtime，手动测试回退到仓库目录执行",
            json!({"repositoryId": repo_id}),
        )
        .await?;
        run_command_in_dir(&root, &command).await
    };
    let status = output.status;
    let exit_code = output.exit_code;
    let stdout_text = output.stdout_text;
    let stderr_text = output.stderr_text;
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    sqlx::query("INSERT INTO rd_test_runs (id, task_id, tenant_id, repository_id, command, status, exit_code, stdout_text, stderr_text, duration_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&run_id)
        .bind(&task_id)
        .bind(&claims.tenant_id)
        .bind(repo_id)
        .bind(&command)
        .bind(&status)
        .bind(exit_code)
        .bind(&stdout_text)
        .bind(&stderr_text)
        .bind(duration_ms)
        .execute(&state.db)
        .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        &task_id,
        "test",
        &status,
        "测试命令已执行",
        json!({"command": command, "exitCode": exit_code, "durationMs": duration_ms, "timeoutSecs": timeout_secs}),
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        Some(repo_id),
        Some(&task_id),
        "test_run",
        1.0,
        json!({
            "runId": run_id.clone(),
            "command": command.clone(),
            "status": status.clone(),
            "exitCode": exit_code,
            "durationMs": duration_ms,
        }),
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        Some(repo_id),
        Some(&task_id),
        "test_duration_ms",
        duration_ms as f64,
        json!({
            "runId": run_id.clone(),
            "command": command.clone(),
            "status": status.clone(),
        }),
    )
    .await?;
    let hook_event = if status == "passed" {
        HookEventType::PostToolUse
    } else {
        HookEventType::PostToolUseFailure
    };
    run_rd_hook(
        &state,
        &claims,
        hook_event,
        "rd.test_command",
        json!({
            "taskId": task_id.clone(),
            "repositoryId": repo_id,
            "command": command,
        }),
        Some(json!({
            "status": status.clone(),
            "exitCode": exit_code,
            "durationMs": duration_ms,
            "stdout": stdout_text.clone(),
            "stderr": stderr_text.clone(),
        })),
        status != "passed",
        false,
    )
    .await?;
    if status != "passed" {
        let repair_state = state.clone();
        let repair_claims = claims.clone();
        let repair_task_id = task_id.clone();
        let repair_run_id = run_id.clone();
        tokio::spawn(async move {
            if let Err(error) = attempt_rd_auto_repair(
                &repair_state,
                &repair_claims,
                &repair_task_id,
                &repair_run_id,
            )
            .await
            {
                let _ = record_event(
                    &repair_state.db,
                    &repair_claims.tenant_id,
                    &repair_task_id,
                    "auto_repair",
                    "failed",
                    &format!("自动修复生成失败: {error}"),
                    json!({"testRunId": repair_run_id}),
                )
                .await;
            }
        });
    }
    get_test_run(&state.db, &claims.tenant_id, &run_id)
        .await
        .map(Json)
}
