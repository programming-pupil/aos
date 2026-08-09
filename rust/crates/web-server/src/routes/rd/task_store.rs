//! RD task storage helpers, event persistence, row mapping, and approval state transitions.

use super::*;

pub(super) async fn complete_rd_task_if_no_pending_applyable_changes(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<bool, AppError> {
    if rd_task_has_pending_applyable_changes(db, tenant_id, task_id).await? {
        return Ok(false);
    }

    let result = sqlx::query(
        "UPDATE rd_tasks \
         SET status = 'completed', completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP) \
         WHERE id = ? AND tenant_id = ? AND status = 'waiting_approval'",
    )
    .bind(task_id)
    .bind(tenant_id)
    .execute(db)
    .await?;

    if result.rows_affected() > 0 {
        record_event(
            db,
            tenant_id,
            task_id,
            "approval",
            "completed",
            "所有可应用 Diff 已处理，任务审批完成",
            json!({}),
        )
        .await?;
        return Ok(true);
    }

    Ok(false)
}

pub(super) async fn reopen_rd_task_if_pending_applyable_changes(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<bool, AppError> {
    if !rd_task_has_pending_applyable_changes(db, tenant_id, task_id).await? {
        return Ok(false);
    }

    let result = sqlx::query(
        "UPDATE rd_tasks \
         SET status = 'waiting_approval', completed_at = NULL \
         WHERE id = ? AND tenant_id = ? AND status = 'completed'",
    )
    .bind(task_id)
    .bind(tenant_id)
    .execute(db)
    .await?;

    if result.rows_affected() > 0 {
        record_event(
            db,
            tenant_id,
            task_id,
            "approval",
            "waiting_approval",
            "回滚后存在可重新应用的 Diff，任务已回到待审批",
            json!({}),
        )
        .await?;
        return Ok(true);
    }

    Ok(false)
}

async fn rd_task_has_pending_applyable_changes(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<bool, AppError> {
    let rows = sqlx::query(
        "SELECT file_path, change_type, diff_patch \
         FROM rd_file_changes \
         WHERE task_id = ? AND tenant_id = ? AND applied = 0",
    )
    .bind(task_id)
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    Ok(rows.iter().any(|row| {
        let file_path: String = row.get("file_path");
        let change_type: String = row.get("change_type");
        let diff_patch: String = row.get("diff_patch");
        rd_file_change_is_applyable(&change_type, &file_path, &diff_patch)
    }))
}

pub(super) async fn record_event(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    stage: &str,
    status: &str,
    message: &str,
    detail: Value,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO rd_task_events (task_id, tenant_id, stage, status, message, detail_json) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(task_id)
        .bind(tenant_id)
        .bind(stage)
        .bind(status)
        .bind(message)
        .bind(detail)
        .execute(db)
        .await?;
    Ok(())
}

pub(super) async fn update_rd_task_context_strategy(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    strategy: &RdTaskContextStrategy,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE rd_tasks \
         SET context_profile = ?, context_depth = ?, should_deep_scan = ? \
         WHERE id = ? AND tenant_id = ?",
    )
    .bind(strategy.profile.as_str())
    .bind(&strategy.depth)
    .bind(strategy.should_deep_scan)
    .bind(task_id)
    .bind(tenant_id)
    .execute(db)
    .await?;
    Ok(())
}

pub(super) async fn ensure_task_access(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
) -> Result<(), AppError> {
    let count: i64 = sqlx::query(
        "SELECT COUNT(*) c FROM rd_tasks WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(task_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_one(db)
    .await?
    .get("c");
    if count == 0 {
        return Err(AppError::NotFound("rd task not found".to_string()));
    }
    Ok(())
}

pub(super) async fn get_task_row(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
) -> Result<RdTaskDto, AppError> {
    let row = sqlx::query("SELECT id, thread_id, parent_task_id, iteration_no, thread_title, repository_id, spec_id, agent_profile_id, workflow_id, runtime_session_id, mode, context_profile, context_depth, should_deep_scan, status, title, prompt, model, plan_md, answer_md, review_md, pr_title, pr_description, error_message, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at, CAST(completed_at AS TEXT) completed_at FROM rd_tasks WHERE id = ? AND tenant_id = ? AND user_id = ?")
        .bind(task_id)
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("rd task not found".to_string()))?;
    Ok(row_to_task(&row))
}

pub(super) async fn get_agent_workflow_row(
    db: &SqlitePool,
    tenant_id: &str,
    workflow_id: &str,
) -> Result<RdAgentWorkflowDto, AppError> {
    let row = sqlx::query("SELECT id, name, description, definition_json, source, source_item_id, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at FROM rd_agent_workflows WHERE id = ? AND tenant_id = ?")
        .bind(workflow_id)
        .bind(tenant_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("rd agent workflow not found".to_string()))?;
    Ok(row_to_agent_workflow(&row))
}

pub(super) async fn load_enabled_agent_workflow(
    db: &SqlitePool,
    tenant_id: &str,
    workflow_id: &str,
) -> Result<RdAgentWorkflowDto, AppError> {
    let workflow = get_agent_workflow_row(db, tenant_id, workflow_id).await?;
    if !workflow.enabled {
        return Err(AppError::ValidationError(
            "selected RD agent workflow is disabled".to_string(),
        ));
    }
    Ok(workflow)
}

pub(super) async fn get_test_run(
    db: &SqlitePool,
    tenant_id: &str,
    run_id: &str,
) -> Result<RdTestRunDto, AppError> {
    let row = sqlx::query("SELECT id, task_id, repository_id, command, status, exit_code, stdout_text, stderr_text, duration_ms, CAST(created_at AS TEXT) created_at FROM rd_test_runs WHERE id = ? AND tenant_id = ?")
        .bind(run_id)
        .bind(tenant_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("test run not found".to_string()))?;
    Ok(row_to_test_run(&row))
}

pub(super) fn row_to_task(row: &sqlx::sqlite::SqliteRow) -> RdTaskDto {
    let context_profile: Option<String> = row.get("context_profile");
    let context_profile_name = context_profile
        .as_deref()
        .and_then(RdContextProfile::from_str)
        .map(|profile| profile.display_name().to_string());
    RdTaskDto {
        id: row.get("id"),
        thread_id: row.get("thread_id"),
        parent_task_id: row.get("parent_task_id"),
        iteration_no: row.get("iteration_no"),
        thread_title: row.get("thread_title"),
        repository_id: row.get("repository_id"),
        spec_id: row.get("spec_id"),
        agent_profile_id: row.get("agent_profile_id"),
        workflow_id: row.get("workflow_id"),
        runtime_session_id: row.get("runtime_session_id"),
        mode: row.get("mode"),
        context_profile,
        context_profile_name,
        context_depth: row.get("context_depth"),
        should_deep_scan: row.get("should_deep_scan"),
        status: row.get("status"),
        title: row.get("title"),
        prompt: row.get("prompt"),
        model: row.get("model"),
        plan_md: row.get("plan_md"),
        answer_md: row.get("answer_md"),
        review_md: row.get("review_md"),
        pr_title: row.get("pr_title"),
        pr_description: row.get("pr_description"),
        error_message: row.get("error_message"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        completed_at: row.get("completed_at"),
    }
}

pub(super) fn row_to_change(row: &sqlx::sqlite::SqliteRow) -> RdFileChangeDto {
    RdFileChangeDto {
        id: row.get("id"),
        task_id: row.get("task_id"),
        repository_id: row.get("repository_id"),
        file_path: row.get("file_path"),
        change_type: row.get("change_type"),
        diff_patch: row.get("diff_patch"),
        applied: row.get("applied"),
        applied_at: row.get("applied_at"),
        created_at: row.get("created_at"),
    }
}

pub(super) fn row_to_test_run(row: &sqlx::sqlite::SqliteRow) -> RdTestRunDto {
    RdTestRunDto {
        id: row.get("id"),
        task_id: row.get("task_id"),
        repository_id: row.get("repository_id"),
        command: row.get("command"),
        status: row.get("status"),
        exit_code: row.get("exit_code"),
        stdout_text: row.get("stdout_text"),
        stderr_text: row.get("stderr_text"),
        duration_ms: row.get("duration_ms"),
        created_at: row.get("created_at"),
    }
}

pub(super) fn row_to_agent_workflow(row: &sqlx::sqlite::SqliteRow) -> RdAgentWorkflowDto {
    RdAgentWorkflowDto {
        id: row.get("id"),
        name: row.get("name"),
        description: row.get("description"),
        definition_json: row.get("definition_json"),
        source: row.get("source"),
        source_item_id: row.get("source_item_id"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
