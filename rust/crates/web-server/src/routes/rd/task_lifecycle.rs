//! Task lifecycle handlers: intent routing, creation, cancellation, and retry.

use super::*;

async fn next_rd_task_iteration(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
) -> Result<i32, AppError> {
    let row = sqlx::query(
        "SELECT MAX(iteration_no) AS max_iteration FROM rd_tasks WHERE tenant_id = ? AND user_id = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .fetch_one(db)
    .await?;
    let max_iteration = row
        .try_get::<Option<i32>, _>("max_iteration")
        .unwrap_or(None)
        .unwrap_or(0);
    Ok((max_iteration + 1).max(2))
}

pub(super) async fn route_task_intent(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdIntentRouteRequest>,
) -> Result<Json<RdIntentRouteResponse>, AppError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        let profile = RdContextProfile::FocusedAsk;
        return Ok(Json(RdIntentRouteResponse {
            mode: "ask".to_string(),
            confidence: 0.0,
            reason: Some("empty prompt".to_string()),
            source: "fallback".to_string(),
            model: req.model,
            profile: profile.as_str().to_string(),
            profile_name: profile.display_name().to_string(),
            depth: "standard".to_string(),
            should_deep_scan: false,
        }));
    }

    Ok(Json(
        route_rd_task_intent(&state, &claims, prompt, req.model.as_deref()).await,
    ))
}

pub(super) async fn create_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdTaskCreateRequest>,
) -> Result<Json<RdTaskDto>, AppError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(AppError::ValidationError("prompt is required".to_string()));
    }
    let parent_task = match req.parent_task_id.as_deref() {
        Some(parent_id) => {
            Some(get_task_row(&state.db, &claims.tenant_id, &claims.sub, parent_id).await?)
        }
        None => None,
    };
    let repository_id = req.repository_id.clone().or_else(|| {
        parent_task
            .as_ref()
            .and_then(|task| task.repository_id.clone())
    });
    if let Some(repo_id) = &repository_id {
        let _ = repository_root(&state, &claims, repo_id).await?;
    }
    let agent_profile = match req.agent_profile_id.as_deref() {
        Some(id) => Some(load_enabled_agent_profile(&state.db, &claims.tenant_id, id).await?),
        None => None,
    };
    let workflow_id = req.workflow_id.clone().or_else(|| {
        parent_task
            .as_ref()
            .and_then(|task| task.workflow_id.clone())
    });
    let workflow = match workflow_id.as_deref() {
        Some(id) => Some(load_enabled_agent_workflow(&state.db, &claims.tenant_id, id).await?),
        None => None,
    };
    let selected_model = req.model.clone().or_else(|| {
        agent_profile
            .as_ref()
            .and_then(|profile| profile.default_model.clone())
    });
    let id = uuid::Uuid::new_v4().to_string();
    let mode = normalize_mode(req.mode.as_deref());
    let baseline_policy = RdGitBaselinePolicy::from_option(req.baseline_policy.as_deref());
    let context_strategy = resolve_rd_task_context_strategy(
        &mode,
        prompt,
        req.context_profile.as_deref(),
        req.context_depth.as_deref(),
        req.should_deep_scan,
        parent_task.as_ref(),
    );
    let title = req.title.unwrap_or_else(|| derive_title(prompt));
    let parent_task_id = parent_task.as_ref().map(|task| task.id.clone());
    let thread_id = parent_task
        .as_ref()
        .and_then(|task| task.thread_id.clone())
        .or_else(|| parent_task.as_ref().map(|task| task.id.clone()))
        .unwrap_or_else(|| id.clone());
    let thread_title = parent_task
        .as_ref()
        .and_then(|task| task.thread_title.clone())
        .unwrap_or_else(|| title.clone());
    let iteration_no = if parent_task.is_some() {
        next_rd_task_iteration(&state.db, &claims.tenant_id, &claims.sub, &thread_id).await?
    } else {
        1
    };
    run_rd_hook(
        &state,
        &claims,
        HookEventType::MessageReceived,
        "rd.task.create",
        json!({
            "mode": mode,
            "contextProfile": context_strategy.profile.as_str(),
            "contextProfileName": context_strategy.profile.display_name(),
            "contextDepth": &context_strategy.depth,
            "shouldDeepScan": context_strategy.should_deep_scan,
            "title": title,
            "prompt": prompt,
            "repositoryId": repository_id.clone(),
            "baselinePolicy": baseline_policy.as_str(),
            "specId": req.spec_id.clone(),
            "agentProfileId": req.agent_profile_id.clone(),
            "workflowId": workflow_id.clone(),
            "parentTaskId": parent_task_id.clone(),
            "threadId": thread_id.clone(),
            "iterationNo": iteration_no,
        }),
        None,
        false,
        true,
    )
    .await?;
    sqlx::query(
        "INSERT INTO rd_tasks (id, tenant_id, user_id, thread_id, parent_task_id, iteration_no, repository_id, spec_id, agent_profile_id, workflow_id, mode, context_profile, context_depth, should_deep_scan, status, title, thread_title, prompt, model, started_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'running', ?, ?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&thread_id)
    .bind(&parent_task_id)
    .bind(iteration_no)
    .bind(&repository_id)
    .bind(&req.spec_id)
    .bind(&req.agent_profile_id)
    .bind(&workflow_id)
    .bind(&mode)
    .bind(context_strategy.profile.as_str())
    .bind(&context_strategy.depth)
    .bind(context_strategy.should_deep_scan)
    .bind(&title)
    .bind(&thread_title)
    .bind(prompt)
    .bind(&selected_model)
    .execute(&state.db)
    .await?;
    let agent_task_id = match req.agent_task_id.clone() {
        Some(agent_task_id) => {
            crate::routes::agent_ops::link_task_resource(
                &state,
                &claims.tenant_id,
                &agent_task_id,
                "rd_task",
                &id,
            )
            .await?;
            agent_task_id
        }
        None => {
            let agent_task_id = crate::routes::agent_ops::create_task(
                &state,
                crate::routes::agent_ops::CreateAgentTaskInput {
                    tenant_id: claims.tenant_id.clone(),
                    source: "webui".to_string(),
                    source_ref: Some(id.clone()),
                    source_label: Some("Code Studio".to_string()),
                    capability_key: "rd_agent".to_string(),
                    agent_id: req.agent_profile_id.clone(),
                    agent_name: agent_profile.as_ref().map(|profile| profile.name.clone()),
                    title: title.clone(),
                    summary: Some(format!("代码开发任务：{mode}")),
                    owner_user_id: Some(claims.sub.clone()),
                    correlation_id: Some(id.clone()),
                    parent_task_id: None,
                    external_platform: None,
                    external_channel_id: None,
                    external_conversation_id: None,
                    external_message_id: None,
                    idempotency_key: Some(format!("rd_task:{id}")),
                    input_json: Some(json!({
                        "rdTaskId": id,
                        "repositoryId": repository_id,
                        "mode": mode,
                        "contextProfile": context_strategy.profile.as_str(),
                        "contextDepth": context_strategy.depth,
                    })),
                },
            )
            .await?;
            crate::routes::agent_ops::link_task_resource(
                &state,
                &claims.tenant_id,
                &agent_task_id,
                "rd_task",
                &id,
            )
            .await?;
            agent_task_id
        }
    };
    let runtime_session = crate::routes::agent_runtime::create_runtime_session(
        &state,
        crate::routes::agent_runtime::RuntimeSessionCreateInput {
            tenant_id: claims.tenant_id.clone(),
            user_id: claims.sub.clone(),
            agent_task_id: Some(agent_task_id.clone()),
            capability_key: "rd_agent".to_string(),
            isolation_mode: None,
            workspace_hint: Some(id.clone()),
        },
    )
    .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        &id,
        "understand",
        "completed",
        "已理解研发任务",
        json!({
            "mode": mode,
            "contextProfile": context_strategy.profile.as_str(),
            "contextProfileName": context_strategy.profile.display_name(),
            "contextDepth": &context_strategy.depth,
            "shouldDeepScan": context_strategy.should_deep_scan,
            "parentTaskId": parent_task_id,
            "threadId": thread_id,
            "iterationNo": iteration_no,
            "agentProfileId": req.agent_profile_id,
            "agentProfileName": agent_profile.as_ref().map(|profile| profile.name.clone()),
            "workflowId": workflow.as_ref().map(|workflow| workflow.id.clone()),
            "workflowName": workflow.as_ref().map(|workflow| workflow.name.clone()),
            "baselinePolicy": baseline_policy.as_str(),
            "agentTaskId": agent_task_id,
            "agentRuntimeSessionId": runtime_session.id,
            "agentRuntimeWorkspace": runtime_session.workspace_root,
        }),
    )
    .await?;
    let run_state = state.clone();
    let run_claims = claims.clone();
    let run_task_id = id.clone();
    let run_mode = mode.clone();
    let run_prompt = prompt.to_string();
    let run_context_strategy = context_strategy.clone();
    let run_baseline_policy = baseline_policy;
    let run_repository_id = repository_id.clone();
    let run_model = selected_model.clone();
    let run_agent_profile_id = agent_profile.as_ref().map(|profile| profile.id.clone());
    let run_workflow_id = workflow.as_ref().map(|workflow| workflow.id.clone());
    let run_agent_task_id = agent_task_id.clone();
    let run_agent_runtime_session_id = runtime_session.id.clone();
    tokio::spawn(async move {
        let worker_id = format!("rd-worker:{}", run_task_id);
        let claimed_queue = match crate::routes::agent_ops::claim_agent_task(
            &run_state,
            &run_claims.tenant_id,
            &run_agent_task_id,
            &worker_id,
            600,
        )
        .await
        {
            Ok(true) => true,
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(
                    tenant_id = %run_claims.tenant_id,
                    task_id = %run_task_id,
                    agent_task_id = %run_agent_task_id,
                    "failed to claim RD AgentOps task queue lease: {}",
                    error
                );
                false
            }
        };
        let acquired_execution_lease =
            match crate::routes::agent_ops::acquire_agent_task_execution_lease(
                &run_state,
                &run_claims.tenant_id,
                &run_agent_task_id,
                &worker_id,
                600,
            )
            .await
            {
                Ok(acquired) => acquired,
                Err(error) => {
                    tracing::warn!(
                        tenant_id = %run_claims.tenant_id,
                        task_id = %run_task_id,
                        agent_task_id = %run_agent_task_id,
                        "failed to acquire RD AgentOps execution lease: {}",
                        error
                    );
                    false
                }
            };
        if !acquired_execution_lease {
            let _ = crate::routes::agent_ops::add_event(
                &run_state,
                &run_claims.tenant_id,
                &run_agent_task_id,
                "queue.execution_lease_skipped",
                Some(crate::routes::agent_ops::PHASE_EXECUTING),
                Some(crate::routes::agent_ops::STATUS_RUNNING),
                "warn",
                "RD worker 未获取执行 lease，已跳过本次后台执行以避免并发重复运行",
                Some(json!({ "workerId": worker_id, "claimedQueue": claimed_queue })),
            )
            .await;
            return;
        }
        let _ = crate::routes::agent_ops::mark_task_running(
            &run_state,
            &run_claims.tenant_id,
            &run_agent_task_id,
            crate::routes::agent_ops::PHASE_EXECUTING,
            if claimed_queue {
                "RD worker 已 claim 任务并开始执行"
            } else {
                "RD worker 已获取执行 lease 并开始执行"
            },
            5,
        )
        .await;
        let _ = crate::routes::agent_ops::heartbeat_agent_task_queue(
            &run_state,
            &run_claims.tenant_id,
            &run_agent_task_id,
            &worker_id,
            600,
        )
        .await;
        let (heartbeat_stop_tx, mut heartbeat_stop_rx) = tokio::sync::watch::channel(false);
        let heartbeat_state = run_state.clone();
        let heartbeat_tenant_id = run_claims.tenant_id.clone();
        let heartbeat_agent_task_id = run_agent_task_id.clone();
        let heartbeat_worker_id = worker_id.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if *heartbeat_stop_rx.borrow() {
                            break;
                        }
                        if let Err(error) = crate::routes::agent_ops::heartbeat_agent_task_queue(
                            &heartbeat_state,
                            &heartbeat_tenant_id,
                            &heartbeat_agent_task_id,
                            &heartbeat_worker_id,
                            600,
                        )
                        .await
                        {
                            tracing::warn!(
                                tenant_id = %heartbeat_tenant_id,
                                agent_task_id = %heartbeat_agent_task_id,
                                worker_id = %heartbeat_worker_id,
                                "failed to heartbeat RD AgentOps task queue lease: {}",
                                error
                            );
                        }
                    }
                    changed = heartbeat_stop_rx.changed() => {
                        if changed.is_err() || *heartbeat_stop_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });
        let result = execute_rd_task(
            &run_state,
            &run_claims,
            &run_task_id,
            &run_mode,
            &run_prompt,
            run_context_strategy,
            run_baseline_policy,
            run_repository_id.as_deref(),
            run_model.as_deref(),
            run_agent_profile_id.as_deref(),
            run_workflow_id.as_deref(),
        )
        .await;
        let _ = heartbeat_stop_tx.send(true);
        let _ = heartbeat_handle.await;
        if let Err(error) = result {
            let message = error.to_string();
            let error_detail = rd_task_error_detail_json(&error);
            if rd_error_is_cancelled(&error) {
                tracing::info!(
                    tenant_id = %run_claims.tenant_id,
                    user_id = %run_claims.sub,
                    task_id = %run_task_id,
                    mode = %run_mode,
                    repository_id = ?run_repository_id,
                    workflow_id = ?run_workflow_id,
                    "RD background task cancelled"
                );
                if let Err(update_error) = sqlx::query(
                    "UPDATE rd_tasks SET status = 'cancelled', error_message = NULL, completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP) WHERE id = ? AND tenant_id = ?",
                )
                .bind(&run_task_id)
                .bind(&run_claims.tenant_id)
                .execute(&run_state.db)
                .await
                {
                    tracing::error!(
                        tenant_id = %run_claims.tenant_id,
                        task_id = %run_task_id,
                        error = %update_error,
                        "failed to persist RD task cancellation status"
                    );
                }
                let _ = record_event(
                    &run_state.db,
                    &run_claims.tenant_id,
                    &run_task_id,
                    "cancelled",
                    "cancelled",
                    "RD 任务已取消",
                    error_detail.clone(),
                )
                .await;
                let _ = record_quality_metric(
                    &run_state.db,
                    &run_claims.tenant_id,
                    run_repository_id.as_deref(),
                    Some(&run_task_id),
                    "task_cancelled",
                    1.0,
                    json!({"mode": run_mode.clone(), "detail": error_detail}),
                )
                .await;
                let _ = run_rd_hook(
                    &run_state,
                    &run_claims,
                    HookEventType::TaskCompleted,
                    "rd.task.cancelled",
                    json!({"taskId": run_task_id, "mode": run_mode, "workflowId": run_workflow_id.clone()}),
                    Some(json!({"status": "cancelled"})),
                    false,
                    false,
                )
                .await;
                if let Err(runtime_error) = crate::routes::agent_runtime::complete_runtime_session(
                    &run_state,
                    &run_claims.tenant_id,
                    &run_agent_runtime_session_id,
                    "cancelled",
                )
                .await
                {
                    tracing::warn!(
                        tenant_id = %run_claims.tenant_id,
                        task_id = %run_task_id,
                        runtime_session_id = %run_agent_runtime_session_id,
                        "failed to mark agent runtime session cancelled: {}",
                        runtime_error
                    );
                }
                if let Err(agent_ops_error) = crate::routes::agent_ops::mark_task_cancelled(
                    &run_state,
                    &run_claims.tenant_id,
                    &run_agent_task_id,
                    "RD 任务已取消",
                    Some(json!({ "rdTaskId": run_task_id, "status": "cancelled" })),
                )
                .await
                {
                    tracing::warn!(
                        tenant_id = %run_claims.tenant_id,
                        task_id = %run_task_id,
                        agent_task_id = %run_agent_task_id,
                        "failed to mark AgentOps RD task cancelled: {}",
                        agent_ops_error
                    );
                }
                return;
            }
            tracing::error!(
                tenant_id = %run_claims.tenant_id,
                user_id = %run_claims.sub,
                task_id = %run_task_id,
                mode = %run_mode,
                repository_id = ?run_repository_id,
                workflow_id = ?run_workflow_id,
                error = %message,
                error_detail = %error_detail,
                error_debug = ?error,
                "RD background task failed"
            );
            if let Err(update_error) = sqlx::query(
                "UPDATE rd_tasks SET status = 'failed', error_message = ?, completed_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ?",
            )
            .bind(&message)
            .bind(&run_task_id)
            .bind(&run_claims.tenant_id)
            .execute(&run_state.db)
            .await
            {
                tracing::error!(
                    tenant_id = %run_claims.tenant_id,
                    task_id = %run_task_id,
                    error = %update_error,
                    "failed to persist RD task failure status"
                );
            }
            if let Err(event_error) = record_event(
                &run_state.db,
                &run_claims.tenant_id,
                &run_task_id,
                "failed",
                "failed",
                &message,
                error_detail.clone(),
            )
            .await
            {
                tracing::error!(
                    tenant_id = %run_claims.tenant_id,
                    task_id = %run_task_id,
                    error = %event_error,
                    "failed to persist RD task failure event"
                );
            }
            let _ = record_quality_metric(
                &run_state.db,
                &run_claims.tenant_id,
                run_repository_id.as_deref(),
                Some(&run_task_id),
                "task_failed",
                1.0,
                json!({"mode": run_mode.clone(), "error": message.clone(), "detail": error_detail}),
            )
            .await;
            let _ = run_rd_hook(
                &run_state,
                &run_claims,
                HookEventType::TaskCompleted,
                "rd.task.failed",
                json!({"taskId": run_task_id, "mode": run_mode, "workflowId": run_workflow_id.clone()}),
                Some(json!({"status": "failed", "error": message})),
                true,
                false,
            )
            .await;
            if let Err(runtime_error) = crate::routes::agent_runtime::complete_runtime_session(
                &run_state,
                &run_claims.tenant_id,
                &run_agent_runtime_session_id,
                "failed",
            )
            .await
            {
                tracing::warn!(
                    tenant_id = %run_claims.tenant_id,
                    task_id = %run_task_id,
                    runtime_session_id = %run_agent_runtime_session_id,
                    "failed to mark agent runtime session failed: {}",
                    runtime_error
                );
            }
            if let Err(agent_ops_error) = crate::routes::agent_ops::fail_task(
                &run_state,
                &run_claims.tenant_id,
                &run_agent_task_id,
                "rd_task_failed",
                &message,
            )
            .await
            {
                tracing::warn!(
                    tenant_id = %run_claims.tenant_id,
                    task_id = %run_task_id,
                    agent_task_id = %run_agent_task_id,
                    "failed to mark AgentOps RD task failed: {}",
                    agent_ops_error
                );
            }
        } else {
            let rd_status =
                sqlx::query("SELECT status FROM rd_tasks WHERE id = ? AND tenant_id = ?")
                    .bind(&run_task_id)
                    .bind(&run_claims.tenant_id)
                    .fetch_optional(&run_state.db)
                    .await
                    .ok()
                    .flatten()
                    .map(|row| row.get::<String, _>("status"))
                    .unwrap_or_else(|| "completed".to_string());
            let runtime_status = if rd_status == "cancelled" {
                "cancelled"
            } else {
                "completed"
            };
            if let Err(runtime_error) = crate::routes::agent_runtime::complete_runtime_session(
                &run_state,
                &run_claims.tenant_id,
                &run_agent_runtime_session_id,
                runtime_status,
            )
            .await
            {
                tracing::warn!(
                    tenant_id = %run_claims.tenant_id,
                    task_id = %run_task_id,
                    runtime_session_id = %run_agent_runtime_session_id,
                    "failed to complete agent runtime session: {}",
                    runtime_error
                );
            }
            let agent_ops_result = if rd_status == "cancelled" {
                crate::routes::agent_ops::mark_task_cancelled(
                    &run_state,
                    &run_claims.tenant_id,
                    &run_agent_task_id,
                    "RD 任务已取消",
                    Some(json!({ "rdTaskId": run_task_id, "status": rd_status })),
                )
                .await
            } else if rd_status == "waiting_approval" {
                crate::routes::agent_ops::mark_task_waiting_approval(
                    &run_state,
                    &run_claims.tenant_id,
                    &run_agent_task_id,
                    crate::routes::agent_ops::PHASE_VALIDATING,
                    "RD 已生成 Diff，等待人工审批",
                    90,
                    Some(json!({ "rdTaskId": run_task_id, "status": rd_status })),
                )
                .await
            } else {
                crate::routes::agent_ops::complete_task(
                    &run_state,
                    &run_claims.tenant_id,
                    &run_agent_task_id,
                    "RD 任务已完成",
                    Some(json!({ "rdTaskId": run_task_id, "status": rd_status })),
                )
                .await
            };
            if let Err(agent_ops_error) = agent_ops_result {
                tracing::warn!(
                    tenant_id = %run_claims.tenant_id,
                    task_id = %run_task_id,
                    agent_task_id = %run_agent_task_id,
                    "failed to sync AgentOps RD task final state: {}",
                    agent_ops_error
                );
            }
        }
    });
    get_task_row(&state.db, &claims.tenant_id, &claims.sub, &id)
        .await
        .map(Json)
}

#[cfg(feature = "bot-agents")]
#[derive(Debug, Clone)]
pub(crate) struct RdBotTaskCreateInput {
    pub(crate) prompt: String,
    pub(crate) agent_task_id: Option<String>,
    pub(crate) repository_id: Option<String>,
    pub(crate) agent_profile_id: Option<String>,
    pub(crate) workflow_id: Option<String>,
    pub(crate) mode: Option<String>,
    pub(crate) context_depth: Option<String>,
    pub(crate) should_deep_scan: Option<bool>,
    pub(crate) model: Option<String>,
}

#[cfg(feature = "bot-agents")]
#[derive(Debug, Clone)]
pub(crate) struct RdBotTaskCreateResult {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) mode: String,
}

#[cfg(feature = "bot-agents")]
pub(crate) async fn create_task_from_bot(
    state: AppState,
    claims: Claims,
    input: RdBotTaskCreateInput,
) -> Result<RdBotTaskCreateResult, AppError> {
    let Json(task) = create_task(
        State(state),
        Extension(claims),
        Json(RdTaskCreateRequest {
            repository_id: input.repository_id,
            spec_id: None,
            agent_profile_id: input.agent_profile_id,
            workflow_id: input.workflow_id,
            parent_task_id: None,
            baseline_policy: None,
            mode: input.mode,
            context_profile: None,
            context_depth: input.context_depth,
            should_deep_scan: input.should_deep_scan,
            title: None,
            prompt: input.prompt,
            model: input.model,
            agent_task_id: input.agent_task_id,
        }),
    )
    .await?;
    Ok(RdBotTaskCreateResult {
        id: task.id,
        title: task.title,
        status: task.status,
        mode: task.mode,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct RdBotTaskRetryResult {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) mode: String,
}

pub(super) async fn cancel_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let agent_runtime_session_id =
        crate::routes::agent_ops::find_runtime_session_for_linked_resource(
            &state,
            &claims.tenant_id,
            "rd_task",
            &task_id,
        )
        .await?;
    if let Some(session_id) = agent_runtime_session_id.as_deref() {
        crate::routes::agent_runtime::request_cancel_runtime_session(
            &state,
            &claims.tenant_id,
            session_id,
        )
        .await?;
    }
    sqlx::query("UPDATE rd_tasks SET status = 'cancelled', completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP) WHERE id = ? AND tenant_id = ? AND status IN ('queued','running','waiting_approval')")
        .bind(&task_id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        &task_id,
        "cancel",
        "cancelled",
        "任务已取消",
        json!({"agentRuntimeSessionId": agent_runtime_session_id}),
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        task.repository_id.as_deref(),
        Some(&task_id),
        "task_cancelled",
        1.0,
        json!({"previousStatus": task.status, "mode": task.mode}),
    )
    .await?;
    Ok(Json(json!({"cancelled": true})))
}

pub(super) async fn retry_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
) -> Result<Json<RdTaskDto>, AppError> {
    retry_task_inner(state, claims, &task_id).await
}

async fn retry_task_inner(
    state: AppState,
    claims: Claims,
    task_id: &str,
) -> Result<Json<RdTaskDto>, AppError> {
    let original = get_task_row(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?;
    let retry_prompt = build_retry_prompt(&original);
    let Json(new_task) = create_task(
        State(state.clone()),
        Extension(claims.clone()),
        Json(RdTaskCreateRequest {
            repository_id: original.repository_id.clone(),
            spec_id: original.spec_id.clone(),
            agent_profile_id: original.agent_profile_id.clone(),
            workflow_id: original.workflow_id.clone(),
            parent_task_id: Some(original.id.clone()),
            baseline_policy: None,
            mode: Some(original.mode.clone()),
            context_profile: original.context_profile.clone(),
            context_depth: original.context_depth.clone(),
            should_deep_scan: Some(original.should_deep_scan),
            title: Some(format!("重试：{}", original.title)),
            prompt: retry_prompt,
            model: original.model.clone(),
            agent_task_id: None,
        }),
    )
    .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        &task_id,
        "retry",
        "completed",
        "已创建重试任务",
        json!({"newTaskId": new_task.id.clone()}),
    )
    .await?;
    Ok(Json(new_task))
}

pub(crate) async fn retry_task_from_agent_ops(
    state: AppState,
    claims: Claims,
    task_id: &str,
) -> Result<RdBotTaskRetryResult, AppError> {
    let Json(task) = retry_task_inner(state, claims, task_id).await?;
    Ok(RdBotTaskRetryResult {
        id: task.id,
        title: task.title,
        status: task.status,
        mode: task.mode,
    })
}

fn build_retry_prompt(task: &RdTaskDto) -> String {
    let mut prompt = format!(
        "请重新执行以下研发任务。重点参考上一次执行状态，避免重复相同失败，并继续遵守“先计划、再生成 Diff、人工确认后应用”的安全策略。\n\n# 原始需求\n{}\n\n# 上一次执行状态\n- task_id: {}\n- status: {}\n- mode: {}\n",
        task.prompt.trim(),
        task.id,
        task.status,
        task.mode
    );
    if let Some(error) = task
        .error_message
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        prompt.push_str("\n# 上一次错误信息\n");
        prompt.push_str(error);
        prompt.push('\n');
    }
    if let Some(answer) = task
        .answer_md
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        prompt.push_str("\n# 上一次结果摘要\n");
        prompt.push_str(&truncate_text(answer, 4_000));
        prompt.push('\n');
    }
    if let Some(review) = task
        .review_md
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        prompt.push_str("\n# 上一次审查内容\n");
        prompt.push_str(&truncate_text(review, 4_000));
        prompt.push('\n');
    }
    prompt
}
