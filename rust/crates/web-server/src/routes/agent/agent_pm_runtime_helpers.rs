use super::*;

async fn wait_pm_session_recovery(
    manager: &AgentSessionManager,
    session_id: &str,
    max_wait: Duration,
) -> bool {
    let started = Instant::now();
    loop {
        let state = manager.get_session_state(session_id).await;
        if !matches!(state, Some(SessionState::Running)) {
            return true;
        }
        if started.elapsed() >= max_wait {
            return false;
        }
        sleep(Duration::from_millis(80)).await;
    }
}

/// Owns a PM-only transient session. Dropping an in-flight orchestration future
/// must also stop the provider request that is running on its scratch session.
pub(super) struct PmTransientSessionGuard {
    manager: Arc<AgentSessionManager>,
    session_id: String,
    cleanup_on_drop: bool,
}

async fn cancel_and_destroy_pm_transient_session(
    manager: Arc<AgentSessionManager>,
    session_id: String,
) {
    match manager.cancel_running_turn(&session_id).await {
        Ok(true) => {
            let _ = wait_pm_session_recovery(
                manager.as_ref(),
                &session_id,
                Duration::from_millis(1500),
            )
            .await;
        }
        Ok(false) | Err(GatewayError::SessionNotFound(_)) => {}
        Err(error) => tracing::warn!(
            session_id = %session_id,
            error = %error,
            "failed to cancel PM transient session during cleanup"
        ),
    }
    let _ = manager.destroy_session(&session_id).await;
}

impl PmTransientSessionGuard {
    pub(super) fn new(manager: Arc<AgentSessionManager>, session_id: String) -> Self {
        Self {
            manager,
            session_id,
            cleanup_on_drop: true,
        }
    }

    pub(super) async fn finish(mut self) {
        self.cleanup_on_drop = false;
        cancel_and_destroy_pm_transient_session(self.manager.clone(), self.session_id.clone())
            .await;
    }
}

impl Drop for PmTransientSessionGuard {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        let manager = self.manager.clone();
        let session_id = self.session_id.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(cancel_and_destroy_pm_transient_session(manager, session_id));
    }
}

struct AbortTurnOnDrop<T> {
    handle: tokio::task::JoinHandle<T>,
}

impl<T> AbortTurnOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self { handle }
    }

    fn abort(&self) {
        self.handle.abort();
    }
}

impl<T> std::future::Future for AbortTurnOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.handle).poll(cx)
    }
}

impl<T> Drop for AbortTurnOnDrop<T> {
    fn drop(&mut self) {
        if !self.handle.is_finished() {
            self.handle.abort();
        }
    }
}

pub(super) async fn run_pm_turn_with_timeout_cleanup_and_options(
    manager: Arc<AgentSessionManager>,
    session_id: String,
    message: String,
    timeout_secs: u64,
    timeout_label: &str,
    mut options: agent_gateway::AgentTurnOptions,
) -> Result<TurnResult, GatewayError> {
    options.stream_timeout_secs.get_or_insert(timeout_secs);
    let manager_for_turn = manager.clone();
    let session_id_for_turn = session_id.clone();
    let turn_task = AbortTurnOnDrop::new(tokio::spawn(async move {
        manager_for_turn
            .run_turn_with_options(&session_id_for_turn, message, options)
            .await
    }));
    tokio::pin!(turn_task);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), &mut turn_task).await {
        Ok(join_result) => match join_result {
            Ok(result) => result,
            Err(join_err) => Err(GatewayError::RuntimeExecution(format!(
                "{timeout_label} join failed: {join_err}"
            ))),
        },
        Err(_) => {
            tracing::warn!(
                "{timeout_label} exceeded {}s; aborting turn and forcing runtime recovery before retry",
                timeout_secs
            );
            let cancel_requested = match manager.cancel_running_turn(&session_id).await {
                Ok(true) => {
                    tracing::warn!(
                        session_id = %session_id,
                        "timeout recovery requested cooperative turn cancellation"
                    );
                    true
                }
                Ok(false) => false,
                Err(error) => {
                    tracing::warn!(
                    session_id = %session_id,
                    "timeout recovery cancel_running_turn failed: {}",
                    error
                    );
                    false
                }
            };
            let mut completed_after_cancel = false;
            if cancel_requested {
                completed_after_cancel =
                    tokio::time::timeout(Duration::from_millis(1500), &mut turn_task)
                        .await
                        .is_ok();
            }
            if !completed_after_cancel {
                turn_task.abort();
                let _ = turn_task.await;
            }
            if let Err(error) = manager.hot_reload_session(&session_id).await {
                tracing::warn!(
                    session_id = %session_id,
                    "timeout recovery hot_reload_session failed: {}",
                    error
                );
            }
            let recovery_wait = Duration::from_secs(pm_timeout_recovery_wait_secs());
            let mut recovered =
                wait_pm_session_recovery(manager.as_ref(), &session_id, recovery_wait).await;
            if !recovered {
                let _ = manager.hot_reload_session(&session_id).await;
                recovered = wait_pm_session_recovery(
                    manager.as_ref(),
                    &session_id,
                    Duration::from_millis(1500),
                )
                .await;
            }
            if !recovered {
                tracing::warn!(
                    session_id = %session_id,
                    "timeout recovery wait window elapsed while session remained running"
                );
            }
            Err(GatewayError::RuntimeExecution(format!(
                "{timeout_label} timed out after {timeout_secs}s"
            )))
        }
    }
}

pub(super) async fn run_pm_internal_turn_with_timeout_cleanup_and_options(
    manager: Arc<AgentSessionManager>,
    user_id: &str,
    tenant_id: &str,
    model: Option<&str>,
    session_source: Option<&str>,
    message: String,
    timeout_secs: u64,
    timeout_label: &str,
    options: agent_gateway::AgentTurnOptions,
) -> Result<TurnResult, GatewayError> {
    let session = manager
        .create_session(
            user_id,
            tenant_id,
            None,
            model,
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            session_source,
            None,
            None,
        )
        .await?;
    let transient_session_id = session.session_id.clone();
    let transient_session_guard =
        PmTransientSessionGuard::new(manager.clone(), transient_session_id.clone());
    let result = run_pm_turn_with_timeout_cleanup_and_options(
        manager.clone(),
        transient_session_id.clone(),
        message,
        timeout_secs,
        timeout_label,
        options,
    )
    .await;
    transient_session_guard.finish().await;
    result
}

pub(super) async fn run_pm_turn_streaming_with_timeout_cleanup_and_options(
    manager: Arc<AgentSessionManager>,
    session_id: String,
    message: String,
    sender: tokio::sync::mpsc::Sender<agent_gateway::AgentEvent>,
    timeout_secs: u64,
    timeout_label: &str,
    mut options: agent_gateway::AgentTurnOptions,
) -> Result<TurnResult, GatewayError> {
    options.stream_timeout_secs.get_or_insert(timeout_secs);
    let manager_for_turn = manager.clone();
    let session_id_for_turn = session_id.clone();
    let turn_task = AbortTurnOnDrop::new(tokio::spawn(async move {
        manager_for_turn
            .run_turn_streaming_with_options(&session_id_for_turn, message, sender, options)
            .await
            .and_then(|outcome| match outcome {
                agent_gateway::AgentTurnRunOutcome::Completed(turn) => Ok(turn),
                agent_gateway::AgentTurnRunOutcome::Suspended(turn) => {
                    Err(GatewayError::RuntimeExecution(format!(
                        "unexpected deferred tool in PM runtime turn {}",
                        turn.runtime_turn_id
                    )))
                }
            })
    }));
    tokio::pin!(turn_task);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), &mut turn_task).await {
        Ok(join_result) => match join_result {
            Ok(result) => result,
            Err(join_err) => Err(GatewayError::RuntimeExecution(format!(
                "{timeout_label} join failed: {join_err}"
            ))),
        },
        Err(_) => {
            tracing::warn!(
                "{timeout_label} exceeded {}s in streaming mode; aborting turn and forcing runtime recovery before retry",
                timeout_secs
            );
            let cancel_requested = match manager.cancel_running_turn(&session_id).await {
                Ok(true) => {
                    tracing::warn!(
                        session_id = %session_id,
                        "streaming timeout recovery requested cooperative turn cancellation"
                    );
                    true
                }
                Ok(false) => false,
                Err(error) => {
                    tracing::warn!(
                    session_id = %session_id,
                    "streaming timeout recovery cancel_running_turn failed: {}",
                    error
                    );
                    false
                }
            };
            let mut completed_after_cancel = false;
            if cancel_requested {
                completed_after_cancel =
                    tokio::time::timeout(Duration::from_millis(1500), &mut turn_task)
                        .await
                        .is_ok();
            }
            if !completed_after_cancel {
                turn_task.abort();
                let _ = turn_task.await;
            }
            if let Err(error) = manager.hot_reload_session(&session_id).await {
                tracing::warn!(
                    session_id = %session_id,
                    "streaming timeout recovery hot_reload_session failed: {}",
                    error
                );
            }
            let recovery_wait = Duration::from_secs(pm_timeout_recovery_wait_secs());
            let mut recovered =
                wait_pm_session_recovery(manager.as_ref(), &session_id, recovery_wait).await;
            if !recovered {
                let _ = manager.hot_reload_session(&session_id).await;
                recovered = wait_pm_session_recovery(
                    manager.as_ref(),
                    &session_id,
                    Duration::from_millis(1500),
                )
                .await;
            }
            if !recovered {
                tracing::warn!(
                    session_id = %session_id,
                    "streaming timeout recovery wait window elapsed while session remained running"
                );
            }
            Err(GatewayError::RuntimeExecution(format!(
                "{timeout_label} timed out after {timeout_secs}s"
            )))
        }
    }
}

pub(super) async fn run_pm_user_visible_answer_streaming_turn(
    manager: Arc<AgentSessionManager>,
    session_id: String,
    message: String,
    timeout_secs: u64,
    timeout_label: &str,
    options: agent_gateway::AgentTurnOptions,
    on_text_delta: impl FnMut(String) + Send + 'static,
) -> Result<TurnResult, GatewayError> {
    run_pm_user_visible_answer_streaming_turn_preserving_partial(
        manager,
        session_id,
        message,
        timeout_secs,
        timeout_label,
        options,
        on_text_delta,
    )
    .await
    .0
}

/// Runs a visible streaming turn while retaining every text delta received
/// before a timeout, cancellation, provider error, or join failure. Callers
/// that recover a user-facing answer must use the returned partial text rather
/// than restarting synthesis from an empty response.
pub(super) async fn run_pm_user_visible_answer_streaming_turn_preserving_partial(
    manager: Arc<AgentSessionManager>,
    session_id: String,
    message: String,
    timeout_secs: u64,
    timeout_label: &str,
    options: agent_gateway::AgentTurnOptions,
    mut on_text_delta: impl FnMut(String) + Send + 'static,
) -> (Result<TurnResult, GatewayError>, String) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<agent_gateway::AgentEvent>(1024);
    let partial_text = Arc::new(Mutex::new(String::new()));
    let partial_text_for_drain = partial_text.clone();
    let drain_task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let agent_gateway::AgentEvent::TextDelta { text, .. } = event {
                if !text.is_empty() {
                    partial_text_for_drain.lock().await.push_str(&text);
                    on_text_delta(text);
                }
            }
        }
    });
    let result = run_pm_turn_streaming_with_timeout_cleanup_and_options(
        manager,
        session_id,
        message,
        tx,
        timeout_secs,
        timeout_label,
        options,
    )
    .await;
    let _ = drain_task.await;
    let captured = partial_text.lock().await.clone();
    (result, captured)
}

fn format_pm_contract_repair_reason_cn(contract_name: &str, attempt: usize, issue: &str) -> String {
    let target = if contract_name.eq_ignore_ascii_case("TASK_GRAPH_V2") {
        "任务图"
    } else {
        "执行约束"
    };
    let issue_trimmed = issue.trim();
    let lower = issue_trimmed.to_ascii_lowercase();

    let detail = if lower.contains("repair_output_missing_json_object")
        || lower.contains("missing json object")
    {
        "模型输出不是合法 JSON 对象".to_string()
    } else if lower.contains("no overlap") && lower.contains("routeallowlist") {
        "routeAllowlist 与当前可用路由集合无交集".to_string()
    } else if lower.contains("timed out") {
        "修复调用超时".to_string()
    } else if lower.contains("missing")
        || lower.contains("must be > 0")
        || lower.contains("invalid")
        || lower.contains("too long")
    {
        format!("合同校验未通过（{}）", issue_trimmed)
    } else if lower.contains("repair_failed") {
        format!("修复调用失败（{}）", issue_trimmed)
    } else {
        format!("{}", issue_trimmed)
    };

    format!("第{}次{}修复失败：{}。", attempt, target, detail)
}

pub(super) async fn repair_exec_constraints_with_retries(
    manager: Arc<AgentSessionManager>,
    session_id: &str,
    session_source: &str,
    user_id: &str,
    tenant_id: &str,
    model: &str,
    user_question: &str,
    preface_text: &mut String,
    runtime_budget: &PmTimeoutBudget,
    planned_route_ids: &[String],
    strict_route_mode: bool,
    initial_issue: Option<String>,
) -> (Option<String>, bool, usize, Vec<String>) {
    let mut contract_issue = initial_issue;
    if contract_issue.is_none() {
        return (None, false, 0, Vec::new());
    }

    let max_retries = pm_contract_repair_max_retries();
    let mut attempts = 0usize;
    let mut repaired = false;
    let mut failure_reasons = Vec::<String>::new();
    let planned_route_set: HashSet<String> = planned_route_ids
        .iter()
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect();

    while let Some(current_issue) = contract_issue.clone() {
        if attempts >= max_retries {
            break;
        }
        attempts += 1;
        let repair_prompt = wrap_pm_research_prompt(
            session_source,
            build_pm_contract_repair_prompt(
                "EXEC_CONSTRAINTS",
                user_question,
                preface_text,
                runtime_budget,
                planned_route_ids,
                &current_issue,
                attempts,
                max_retries,
            ),
        );
        let mut options = agent_gateway::AgentTurnOptions {
            blocked_tools: pm_blocked_non_search_research_tools(),
            prefer_native_web_search: false,
            suppress_native_web_search: true,
            model_budget_stage: runtime::RuntimeModelBudgetStage::DomainVerifier,
            ..agent_gateway::AgentTurnOptions::default()
        };
        options.system_instructions.push(
            "Internal PM contract repair turn. Do not call tools, search, browse, fetch URLs, or inspect resources. Return only the requested JSON object."
                .to_string(),
        );
        match run_pm_internal_turn_with_timeout_cleanup_and_options(
            manager.clone(),
            user_id,
            tenant_id,
            Some(model),
            Some(session_source),
            repair_prompt,
            pm_contract_repair_turn_timeout_secs(),
            "contract repair turn",
            options,
        )
        .await
        {
            Ok(repair_turn) => {
                let json_candidate = extract_first_json_object(&repair_turn.text)
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
                match json_candidate {
                    Some(json_value) => {
                        if let Err(err) =
                            validate_exec_constraints_contract(&json_value, runtime_budget)
                        {
                            tracing::warn!(
                                session_id = %session_id,
                                attempt = attempts,
                                "pm contract repair validation failed: {}",
                                err
                            );
                            let err_text = err.to_string();
                            failure_reasons.push(format_pm_contract_repair_reason_cn(
                                "EXEC_CONSTRAINTS",
                                attempts,
                                &err_text,
                            ));
                            contract_issue = Some(err_text);
                        } else {
                            if strict_route_mode && !planned_route_set.is_empty() {
                                let allowlist: Vec<String> = json_value
                                    .get("routeAllowlist")
                                    .and_then(|value| value.as_array())
                                    .map(|items| {
                                        items
                                            .iter()
                                            .filter_map(|item| item.as_str())
                                            .map(|item| item.trim().to_ascii_lowercase())
                                            .filter(|item| !item.is_empty())
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                if !allowlist.is_empty() {
                                    let has_overlap = allowlist
                                        .iter()
                                        .any(|route_id| planned_route_set.contains(route_id));
                                    if !has_overlap {
                                        let planned = planned_route_ids.join(", ");
                                        let err_text = format!(
                                            "EXEC_CONSTRAINTS.routeAllowlist has no overlap with planned sourceRoutes ({})",
                                            planned
                                        );
                                        failure_reasons.push(format_pm_contract_repair_reason_cn(
                                            "EXEC_CONSTRAINTS",
                                            attempts,
                                            &err_text,
                                        ));
                                        contract_issue = Some(err_text);
                                        continue;
                                    }
                                }
                            }
                            preface_text.push_str("\nEXEC_CONSTRAINTS ");
                            preface_text.push_str(&json_value.to_string());
                            repaired = true;
                            contract_issue = None;
                            break;
                        }
                    }
                    None => {
                        tracing::warn!(
                            session_id = %session_id,
                            attempt = attempts,
                            "pm contract repair output missing JSON object"
                        );
                        let err_text = "repair_output_missing_json_object".to_string();
                        failure_reasons.push(format_pm_contract_repair_reason_cn(
                            "EXEC_CONSTRAINTS",
                            attempts,
                            &err_text,
                        ));
                        contract_issue = Some(err_text);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
                    attempt = attempts,
                    "pm contract repair turn failed: {}",
                    error
                );
                let err_text = format!("exec_contract_repair_failed: {}", error);
                failure_reasons.push(format_pm_contract_repair_reason_cn(
                    "EXEC_CONSTRAINTS",
                    attempts,
                    &err_text,
                ));
                contract_issue = Some(err_text);
            }
        }
    }

    (contract_issue, repaired, attempts, failure_reasons)
}

pub(super) async fn repair_task_graph_with_retries(
    manager: Arc<AgentSessionManager>,
    session_id: &str,
    session_source: &str,
    user_id: &str,
    tenant_id: &str,
    model: &str,
    user_question: &str,
    preface_text: &mut String,
    initial_issue: Option<String>,
) -> (Option<String>, bool, usize, Vec<String>) {
    let mut contract_issue = initial_issue;
    if contract_issue.is_none() {
        return (None, false, 0, Vec::new());
    }

    let max_retries = pm_contract_repair_max_retries();
    let mut attempts = 0usize;
    let mut repaired = false;
    let mut failure_reasons = Vec::<String>::new();

    while let Some(current_issue) = contract_issue.clone() {
        if attempts >= max_retries {
            break;
        }
        attempts += 1;
        let repair_prompt = wrap_pm_research_prompt(
            session_source,
            build_pm_task_graph_repair_prompt(
                user_question,
                preface_text,
                &current_issue,
                attempts,
                max_retries,
            ),
        );
        let mut options = agent_gateway::AgentTurnOptions {
            blocked_tools: pm_blocked_non_search_research_tools(),
            prefer_native_web_search: false,
            suppress_native_web_search: true,
            model_budget_stage: runtime::RuntimeModelBudgetStage::DomainVerifier,
            ..agent_gateway::AgentTurnOptions::default()
        };
        options.system_instructions.push(
            "Internal PM task-graph repair turn. Do not call tools, search, browse, fetch URLs, or inspect resources. Return only the requested JSON object."
                .to_string(),
        );
        match run_pm_internal_turn_with_timeout_cleanup_and_options(
            manager.clone(),
            user_id,
            tenant_id,
            Some(model),
            Some(session_source),
            repair_prompt,
            pm_contract_repair_turn_timeout_secs(),
            "task graph repair turn",
            options,
        )
        .await
        {
            Ok(repair_turn) => {
                let json_candidate = extract_first_json_object(&repair_turn.text)
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
                match json_candidate {
                    Some(json_value) => {
                        let candidate_preface = format!("TASK_GRAPH_V2 {}", json_value);
                        if let Some(err) = detect_pm_task_graph_issue(&candidate_preface) {
                            tracing::warn!(
                                session_id = %session_id,
                                attempt = attempts,
                                "pm task graph repair validation failed: {}",
                                err
                            );
                            failure_reasons.push(format_pm_contract_repair_reason_cn(
                                "TASK_GRAPH_V2",
                                attempts,
                                &err,
                            ));
                            contract_issue = Some(err);
                        } else {
                            preface_text.push_str("\nTASK_GRAPH_V2 ");
                            preface_text.push_str(&json_value.to_string());
                            repaired = true;
                            contract_issue = None;
                            break;
                        }
                    }
                    None => {
                        tracing::warn!(
                            session_id = %session_id,
                            attempt = attempts,
                            "pm task graph repair output missing JSON object"
                        );
                        let err_text = "repair_output_missing_json_object".to_string();
                        failure_reasons.push(format_pm_contract_repair_reason_cn(
                            "TASK_GRAPH_V2",
                            attempts,
                            &err_text,
                        ));
                        contract_issue = Some(err_text);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
                    attempt = attempts,
                    "pm task graph repair turn failed: {}",
                    error
                );
                let err_text = format!("task_graph_repair_failed: {}", error);
                failure_reasons.push(format_pm_contract_repair_reason_cn(
                    "TASK_GRAPH_V2",
                    attempts,
                    &err_text,
                ));
                contract_issue = Some(err_text);
            }
        }
    }

    (contract_issue, repaired, attempts, failure_reasons)
}
