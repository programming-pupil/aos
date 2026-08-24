use super::*;

pub(super) const CHAT_ADVERSARIAL_RESOURCE_TYPE: &str = "chat_adversarial_run";

fn trim_summary(value: &str, max_chars: usize) -> String {
    let mut iter = value.chars();
    let trimmed = iter.by_ref().take(max_chars).collect::<String>();
    if iter.next().is_some() {
        format!("{trimmed}...")
    } else {
        trimmed
    }
}

pub(super) async fn create_chat_adversarial_agent_task(
    state: &AppState,
    claims: &Claims,
    run_id: &str,
    question: &str,
    models: &[String],
    max_rounds: u32,
) -> Result<String, AppError> {
    let task_id = crate::routes::agent_ops::create_task(
        state,
        crate::routes::agent_ops::CreateAgentTaskInput {
            tenant_id: claims.tenant_id.clone(),
            source: "webui".to_string(),
            source_ref: Some(run_id.to_string()),
            source_label: Some("Super Debate".to_string()),
            capability_key: "super_adversarial".to_string(),
            agent_id: None,
            agent_name: Some("Super Debate".to_string()),
            title: trim_summary(question, 120),
            summary: Some(trim_summary(question, 512)),
            owner_user_id: Some(claims.sub.clone()),
            correlation_id: Some(run_id.to_string()),
            parent_task_id: None,
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
            external_message_id: None,
            idempotency_key: Some(format!("chat-adversarial:{run_id}")),
            input_json: Some(serde_json::json!({
                "question": question,
                "models": models,
                "maxRounds": max_rounds,
            })),
        },
    )
    .await?;
    crate::routes::agent_ops::link_task_resource(
        state,
        &claims.tenant_id,
        &task_id,
        CHAT_ADVERSARIAL_RESOURCE_TYPE,
        run_id,
    )
    .await?;
    Ok(task_id)
}

pub(super) async fn latest_chat_adversarial_agent_task_id(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
) -> Result<Option<String>, AppError> {
    let row = sqlx::query(
        r"
        SELECT id
        FROM agent_tasks
        WHERE tenant_id = ?
          AND linked_resource_type = ?
          AND linked_resource_id = ?
        ORDER BY updated_at DESC, created_at DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(CHAT_ADVERSARIAL_RESOURCE_TYPE)
    .bind(run_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|row| row.get("id")))
}

pub(super) async fn mark_chat_adversarial_agent_running(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
    current_round: i32,
    max_rounds: i32,
) -> Result<(), AppError> {
    if let Some(task_id) = latest_chat_adversarial_agent_task_id(state, tenant_id, run_id).await? {
        let progress = if max_rounds > 0 {
            20 + ((current_round.clamp(0, max_rounds) * 65) / max_rounds)
        } else {
            50
        };
        crate::routes::agent_ops::mark_task_running(
            state,
            tenant_id,
            &task_id,
            crate::routes::agent_ops::PHASE_DEBATING,
            &format!("Super Debate running: round {current_round}/{max_rounds}"),
            progress,
        )
        .await?;
        let _ = crate::routes::agent_ops::heartbeat_agent_task_queue(
            state,
            tenant_id,
            &task_id,
            &format!("chat-adversarial:{run_id}"),
            300,
        )
        .await;
    }
    Ok(())
}

pub(super) async fn mark_chat_adversarial_agent_completed(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
    output_json: serde_json::Value,
) -> Result<(), AppError> {
    if let Some(task_id) = latest_chat_adversarial_agent_task_id(state, tenant_id, run_id).await? {
        crate::routes::agent_ops::complete_task(
            state,
            tenant_id,
            &task_id,
            "Super Debate completed",
            Some(output_json),
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn mark_chat_adversarial_agent_failed(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
    error_message: &str,
) -> Result<(), AppError> {
    if let Some(task_id) = latest_chat_adversarial_agent_task_id(state, tenant_id, run_id).await? {
        crate::routes::agent_ops::fail_task(
            state,
            tenant_id,
            &task_id,
            "chat_adversarial_failed",
            error_message,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn mark_chat_adversarial_agent_cancel_requested(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
) -> Result<(), AppError> {
    if let Some(task_id) = latest_chat_adversarial_agent_task_id(state, tenant_id, run_id).await? {
        crate::routes::agent_ops::mark_task_cancelling(
            state,
            tenant_id,
            &task_id,
            "Super Debate cancellation requested",
            Some(serde_json::json!({ "runId": run_id })),
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn mark_chat_adversarial_agent_cancelled(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
) -> Result<(), AppError> {
    if let Some(task_id) = latest_chat_adversarial_agent_task_id(state, tenant_id, run_id).await? {
        crate::routes::agent_ops::mark_task_cancelled(
            state,
            tenant_id,
            &task_id,
            "Super Debate cancelled",
            Some(serde_json::json!({ "runId": run_id })),
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn request_chat_adversarial_cancel(
    state: &AppState,
    tenant_id: &str,
    user_id: Option<&str>,
    run_id: &str,
) -> Result<String, AppError> {
    let mut query = String::from(
        r"
        UPDATE chat_adversarial_runs
        SET status = CASE
                WHEN status IN ('queued','running') THEN 'cancelling'
                ELSE status
            END,
            error_message = COALESCE(error_message, 'cancel requested'),
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
          AND status IN ('queued','running','cancelling')
        ",
    );
    if user_id.is_some() {
        query.push_str(" AND user_id = ?");
    }
    let mut sql = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(tenant_id)
        .bind(run_id);
    if let Some(user_id) = user_id {
        sql = sql.bind(user_id);
    }
    sql.execute(&state.db).await?;
    mark_chat_adversarial_agent_cancel_requested(state, tenant_id, run_id).await?;
    let status = load_chat_adversarial_status(state, tenant_id, run_id).await?;
    Ok(status.unwrap_or_else(|| "not_found".to_string()))
}

pub(super) async fn chat_adversarial_cancel_requested(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
) -> Result<bool, AppError> {
    let status = sqlx::query_scalar::<_, Option<String>>(
        "SELECT status FROM chat_adversarial_runs WHERE tenant_id = ? AND user_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(run_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();
    Ok(matches!(
        status.as_deref(),
        Some("cancelling") | Some("cancelled")
    ))
}

pub(super) async fn finish_chat_adversarial_cancelled(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    trace: Option<&serde_json::Value>,
) -> Result<(), AppError> {
    let trace_json = trace
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| AppError::Internal(format!("failed to encode trace: {e}")))?;
    sqlx::query(
        r"
        UPDATE chat_adversarial_runs
        SET status = 'cancelled',
            trace_json = COALESCE(?, trace_json),
            error_message = COALESCE(error_message, 'cancelled by user'),
            updated_at = CURRENT_TIMESTAMP,
            completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP)
        WHERE id = ? AND tenant_id = ? AND user_id = ?
          AND status IN ('queued','running','cancelling')
        ",
    )
    .bind(trace_json)
    .bind(run_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(&state.db)
    .await?;
    mark_chat_adversarial_agent_cancelled(state, tenant_id, run_id).await?;
    Ok(())
}

async fn load_chat_adversarial_status(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
) -> Result<Option<String>, AppError> {
    let status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM chat_adversarial_runs WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(status)
}
