//! Runtime session lifecycle, reuse, and timeout cleanup for RD tasks.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RdRuntimeSessionPolicy {
    ThreadPersistent,
    Transient,
}

impl RdRuntimeSessionPolicy {
    pub(super) fn source(self) -> &'static str {
        match self {
            Self::ThreadPersistent => RD_THREAD_SOURCE,
            Self::Transient => RD_INTERNAL_SOURCE,
        }
    }

    pub(super) fn is_persistent(self) -> bool {
        matches!(self, Self::ThreadPersistent)
    }
}

#[derive(Debug)]
pub(super) struct RdRuntimeTaskContext {
    id: String,
    thread_id: Option<String>,
    repository_id: Option<String>,
    model: Option<String>,
    agent_profile_id: Option<String>,
    workflow_id: Option<String>,
    runtime_session_id: Option<String>,
}

#[derive(Debug)]
struct RdRuntimeSessionCandidate {
    task_id: String,
    session_id: String,
}

pub(super) struct RdRuntimeSessionStart {
    pub(super) handle: agent_gateway::SessionHandle,
    pub(super) reused: bool,
    pub(super) source_task_id: Option<String>,
}

pub(super) async fn start_rd_runtime_session(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: Option<&str>,
    model: Option<&str>,
    allowed_tools: Option<Vec<String>>,
    session_policy: RdRuntimeSessionPolicy,
) -> Result<RdRuntimeSessionStart, AppError> {
    let manager = state.agent_manager().clone();
    let task_context = if session_policy.is_persistent() {
        load_rd_runtime_task_context(&state.db, &claims.tenant_id, &claims.sub, task_id).await?
    } else {
        None
    };
    let effective_repository_id = repository_id.map(ToOwned::to_owned).or_else(|| {
        task_context
            .as_ref()
            .and_then(|ctx| ctx.repository_id.clone())
    });
    let effective_model = model
        .map(ToOwned::to_owned)
        .or_else(|| task_context.as_ref().and_then(|ctx| ctx.model.clone()));

    if let Some(candidate) = reusable_rd_runtime_session_candidate(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        task_context.as_ref(),
    )
    .await?
    {
        if matches!(
            manager.get_session_state(&candidate.session_id).await,
            Some(agent_gateway::SessionState::Running)
        ) {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "runtime",
                "running",
                "历史研发 runtime 正在执行中，已创建新会话避免并发冲突",
                json!({
                    "candidateSessionId": candidate.session_id,
                    "candidateTaskId": candidate.task_id,
                    "reason": "session_running",
                }),
            )
            .await?;
        } else {
            match manager.get_session(&candidate.session_id).await {
                Some(mut handle)
                    if handle.tenant_id == claims.tenant_id && handle.user_id == claims.sub =>
                {
                    manager
                        .hot_reload_session_with_options(
                            &candidate.session_id,
                            effective_model.as_deref(),
                            allowed_tools.clone(),
                        )
                        .await
                        .map_err(|error| {
                            AppError::Internal(format!(
                                "RD runtime hot_reload_session failed: {error}"
                            ))
                        })?;
                    if let Some(reloaded) = manager.get_session(&candidate.session_id).await {
                        handle = reloaded;
                    }
                    return Ok(RdRuntimeSessionStart {
                        handle,
                        reused: true,
                        source_task_id: Some(candidate.task_id),
                    });
                }
                Some(handle) => {
                    tracing::warn!(
                        task_id = %task_id,
                        session_id = %candidate.session_id,
                        actual_tenant_id = %handle.tenant_id,
                        actual_user_id = %handle.user_id,
                        "refusing to reuse RD runtime session with mismatched ownership"
                    );
                    record_event(
                        &state.db,
                        &claims.tenant_id,
                        task_id,
                        "runtime",
                        "running",
                        "历史研发 runtime 归属不匹配，已创建新会话",
                        json!({
                            "candidateSessionId": candidate.session_id,
                            "candidateTaskId": candidate.task_id,
                            "reason": "ownership_mismatch",
                        }),
                    )
                    .await?;
                }
                None => {
                    record_event(
                        &state.db,
                        &claims.tenant_id,
                        task_id,
                        "runtime",
                        "running",
                        "历史研发 runtime 无法恢复，已创建新会话",
                        json!({
                            "candidateSessionId": candidate.session_id,
                            "candidateTaskId": candidate.task_id,
                            "reason": "restore_failed",
                        }),
                    )
                    .await?;
                }
            }
        }
    }

    let handle = manager
        .create_session(
            &claims.sub,
            &claims.tenant_id,
            effective_repository_id.as_deref(),
            effective_model.as_deref(),
            session_policy.source(),
            Some(RD_SCENARIO),
            None,
            allowed_tools,
        )
        .await
        .map_err(|e| AppError::Internal(format!("RD runtime create_session failed: {e}")))?;
    Ok(RdRuntimeSessionStart {
        handle,
        reused: false,
        source_task_id: None,
    })
}

async fn load_rd_runtime_task_context(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
) -> Result<Option<RdRuntimeTaskContext>, AppError> {
    let row = sqlx::query(
        "SELECT id, thread_id, repository_id, model, agent_profile_id, workflow_id, runtime_session_id \
         FROM rd_tasks WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(task_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| RdRuntimeTaskContext {
        id: row.get("id"),
        thread_id: row.get("thread_id"),
        repository_id: row.get("repository_id"),
        model: row.get("model"),
        agent_profile_id: row.get("agent_profile_id"),
        workflow_id: row.get("workflow_id"),
        runtime_session_id: row.get("runtime_session_id"),
    }))
}

async fn reusable_rd_runtime_session_candidate(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    context: Option<&RdRuntimeTaskContext>,
) -> Result<Option<RdRuntimeSessionCandidate>, AppError> {
    let Some(context) = context else {
        return Ok(None);
    };
    if let Some(session_id) = context
        .runtime_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(RdRuntimeSessionCandidate {
            task_id: context.id.clone(),
            session_id: session_id.to_string(),
        }));
    }

    let Some(thread_id) = context
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let row = if let Some(model) = context
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sqlx::query(
            "SELECT id, runtime_session_id \
             FROM rd_tasks \
             WHERE tenant_id = ? AND user_id = ? AND thread_id = ? AND id <> ? \
               AND runtime_session_id IS NOT NULL \
               AND repository_id IS ? \
               AND agent_profile_id IS ? \
               AND workflow_id IS ? \
               AND model IS ? \
             ORDER BY iteration_no DESC, updated_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(thread_id)
        .bind(&context.id)
        .bind(context.repository_id.as_deref())
        .bind(context.agent_profile_id.as_deref())
        .bind(context.workflow_id.as_deref())
        .bind(model)
        .fetch_optional(db)
        .await?
    } else {
        sqlx::query(
            "SELECT id, runtime_session_id \
             FROM rd_tasks \
             WHERE tenant_id = ? AND user_id = ? AND thread_id = ? AND id <> ? \
               AND runtime_session_id IS NOT NULL \
               AND repository_id IS ? \
               AND agent_profile_id IS ? \
               AND workflow_id IS ? \
             ORDER BY iteration_no DESC, updated_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(thread_id)
        .bind(&context.id)
        .bind(context.repository_id.as_deref())
        .bind(context.agent_profile_id.as_deref())
        .bind(context.workflow_id.as_deref())
        .fetch_optional(db)
        .await?
    };

    Ok(row.and_then(|row| {
        let session_id: Option<String> = row.get("runtime_session_id");
        session_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|session_id| RdRuntimeSessionCandidate {
                task_id: row.get("id"),
                session_id,
            })
    }))
}

pub(super) async fn run_rd_runtime_turn_with_timeout_cleanup(
    state: &AppState,
    manager: std::sync::Arc<agent_gateway::AgentSessionManager>,
    session_id: String,
    tenant_id: &str,
    task_id: &str,
    message: String,
    destroy_on_timeout: bool,
    governance_plan: Option<RdRuntimeToolGovernancePlan>,
    timeout_detail: Value,
) -> Result<agent_gateway::TurnResult, AppError> {
    let timeout_secs = rd_runtime_timeout_secs();
    let (sender, receiver) = mpsc::channel(128);
    let event_db = state.db.clone();
    let event_tenant_id = tenant_id.to_string();
    let event_task_id = task_id.to_string();
    let event_task = tokio::spawn(async move {
        persist_rd_runtime_events(
            event_db,
            event_tenant_id,
            event_task_id,
            receiver,
            governance_plan,
        )
        .await;
    });

    let manager_for_turn = manager.clone();
    let session_id_for_turn = session_id.clone();
    let mut turn_task = tokio::spawn(async move {
        manager_for_turn
            .run_turn_streaming(&session_id_for_turn, message, sender)
            .await
    });
    match timeout(Duration::from_secs(timeout_secs), &mut turn_task).await {
        Ok(join_result) => {
            let result = match join_result {
                Ok(Ok(turn)) => Ok(turn),
                Ok(Err(error)) => Err(rd_runtime_gateway_error_to_app_error(error)),
                Err(error) => Err(AppError::Internal(format!(
                    "RD runtime turn join failed: {error}"
                ))),
            };
            wait_rd_runtime_event_task(event_task).await;
            result
        }
        Err(_) => {
            turn_task.abort();
            let _ = turn_task.await;
            wait_rd_runtime_event_task(event_task).await;
            if destroy_on_timeout {
                let _ = manager.destroy_session(&session_id).await;
                clear_rd_runtime_session_binding(&state.db, tenant_id, &session_id).await;
            }
            if let Err(error) = record_event(
                &state.db,
                tenant_id,
                task_id,
                "runtime_timeout",
                "failed",
                &format!("研发 runtime 执行超过 {timeout_secs}s，已中止本轮"),
                json!({
                    "sessionId": session_id,
                    "timeoutSecs": timeout_secs,
                    "destroyOnTimeout": destroy_on_timeout,
                    "destroyed": destroy_on_timeout,
                    "detail": timeout_detail,
                }),
            )
            .await
            {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    task_id = %task_id,
                    "failed to record RD runtime timeout event: {}",
                    error
                );
            }
            Err(AppError::Internal(format!(
                "RD runtime turn timed out after {timeout_secs}s"
            )))
        }
    }
}

fn rd_runtime_gateway_error_to_app_error(error: agent_gateway::GatewayError) -> AppError {
    let error_text = error.to_string();
    if rd_runtime_error_is_auth_failure(&error_text) {
        return AppError::ValidationError(format!(
            "研发模型 API Key 鉴权失败：请到 API 密钥管理检查适用场景=研发、模型类型=聊天模型的 Key、BaseURL 和模型名称是否有效。原始错误：{}",
            truncate_text(&error_text, 500)
        ));
    }

    AppError::Internal(format!("RD runtime turn failed: {error_text}"))
}

pub(super) fn rd_runtime_error_is_auth_failure(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("authentication_error")
        || lower.contains("incorrect_api_key")
        || error_text.contains("用户信息验证失败")
}

async fn clear_rd_runtime_session_binding(db: &SqlitePool, tenant_id: &str, session_id: &str) {
    if let Err(error) = sqlx::query(
        "UPDATE rd_tasks SET runtime_session_id = NULL \
         WHERE tenant_id = ? AND runtime_session_id = ?",
    )
    .bind(tenant_id)
    .bind(session_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            tenant_id = %tenant_id,
            session_id = %session_id,
            "failed to clear RD runtime session binding after timeout: {}",
            error
        );
    }
}

async fn wait_rd_runtime_event_task(task: tokio::task::JoinHandle<()>) {
    if timeout(Duration::from_secs(5), task).await.is_err() {
        tracing::warn!("RD runtime event persistence task did not finish within 5s");
    }
}
