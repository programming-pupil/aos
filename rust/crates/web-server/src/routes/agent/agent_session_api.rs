use super::*;

async fn enforce_user_visible_session_limit(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    source: &str,
) -> Result<(), AppError> {
    if !is_user_visible_session_source(source) {
        return Ok(());
    }
    let quota_source = user_visible_session_quota_source(source);

    let visible_count = get_agent_manager(state)
        .list_user_sessions(tenant_id, user_id, None)
        .await
        .into_iter()
        .filter(|session| is_user_visible_session_source(&session.source))
        .filter(|session| user_visible_session_quota_source(&session.source) == quota_source)
        .count();
    if visible_count >= MAX_USER_VISIBLE_SESSIONS_PER_USER {
        return Err(AppError::ValidationError(format!(
            "session limit reached: maximum {MAX_USER_VISIBLE_SESSIONS_PER_USER} {quota_source} sessions per user. Please delete old {quota_source} sessions and retry."
        )));
    }
    Ok(())
}

fn user_visible_session_quota_source(source: &str) -> &str {
    match source {
        "chat" | "pm" | "rd" | "agent" | "super_assistant" => source,
        _ => "agent",
    }
}

fn is_user_visible_session_source(source: &str) -> bool {
    !source.starts_with("pm_internal")
        && source != PM_INTERNAL_TRANSIENT_SESSION_SOURCE
        && !source.starts_with("rd_internal")
        && !source.starts_with("rd_thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_visible_session_quota_is_split_by_user_facing_source() {
        for source in ["chat", "pm", "rd", "agent", "super_assistant"] {
            assert_eq!(user_visible_session_quota_source(source), source);
        }
    }

    #[test]
    fn unknown_user_visible_sources_share_agent_quota_bucket() {
        for source in ["", "custom", "Chat", "materials"] {
            assert_eq!(user_visible_session_quota_source(source), "agent");
        }
    }

    #[test]
    fn internal_runtime_sources_are_not_user_visible() {
        for source in [
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            "pm_internal_mission",
            "rd_internal",
            "rd_internal_cand",
            "rd_thread",
        ] {
            assert!(!is_user_visible_session_source(source));
        }
    }

    #[test]
    fn user_facing_sources_remain_visible() {
        for source in ["chat", "pm", "rd", "agent", "super_assistant", "custom"] {
            assert!(is_user_visible_session_source(source));
        }
    }

    #[test]
    fn legacy_conversation_sources_restore_through_super_assistant_entry() {
        for source in ["chat", "pm", "dataAttribution", "nl2sql"] {
            assert_eq!(
                canonical_entry_for_session_source(Some(source)),
                "super_assistant"
            );
        }
        assert_eq!(canonical_entry_for_session_source(Some("rd")), "agent");
        assert_eq!(canonical_entry_for_session_source(Some("custom")), "agent");
        assert_eq!(canonical_entry_for_session_source(None), "agent");
    }

    #[test]
    fn validation_gateway_errors_are_reported_as_client_errors() {
        let error = GatewayError::Validation("configure an API key".to_string());
        assert_eq!(
            gateway_error_status(&error),
            axum::http::StatusCode::BAD_REQUEST
        );
    }
}

/// POST /api/v1/agent/sessions — create a new agent session
pub(super) async fn create_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let user_id = &claims.sub;
    let tenant_id = &claims.tenant_id;
    let source = req.source.as_deref().unwrap_or("agent");

    if let Err(error) = enforce_user_visible_session_limit(&state, tenant_id, user_id, source).await
    {
        return error.into_response();
    }

    match get_agent_manager(&state)
        .create_session(
            user_id,
            tenant_id,
            req.project_id.as_deref(),
            req.model.as_deref(),
            source,
            req.scenario.as_deref(),
            req.locale.as_deref(),
            None,
        )
        .await
    {
        Ok(handle) => {
            let dto = SessionDto::from(handle);
            Json(CreateSessionResponse { session: dto }).into_response()
        }
        Err(e) => gateway_error_to_response(&e),
    }
}

/// GET /api/v1/agent/sessions — list active sessions
pub(super) async fn list_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListSessionsQuery>,
) -> impl IntoResponse {
    let mut sessions: Vec<SessionInfo> = get_agent_manager(&state)
        .list_user_sessions(&claims.tenant_id, &claims.sub, query.source.as_deref())
        .await;
    // Hide internal runtime sessions from user-facing session list.
    sessions.retain(|session| is_user_visible_session_source(&session.source));
    let dtos: Vec<SessionDto> = sessions.into_iter().map(SessionDto::from).collect();
    Json(serde_json::json!({ "sessions": dtos, "total": dtos.len() })).into_response()
}

/// GET `/api/v1/agent/sessions/{session_id}` — get session info
pub(super) async fn get_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match get_agent_manager(&state).get_session(&session_id).await {
        Some(handle) => {
            if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
                return AppError::Forbidden.into_response();
            }
            Json(SessionDto::from(handle)).into_response()
        }
        None => AppError::NotFound(format!("session {session_id} not found")).into_response(),
    }
}

/// DELETE `/api/v1/agent/sessions/{session_id}` — destroy a session
pub(super) async fn delete_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    if let Some(handle) = get_agent_manager(&state).get_session(&session_id).await {
        if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
            return AppError::Forbidden.into_response();
        }
    }

    match get_agent_manager(&state).destroy_session(&session_id).await {
        Ok(()) => {
            if let Err(error) = crate::semantic_kernel_store::delete_session_artifacts(
                &state.db,
                &claims.tenant_id,
                &session_id,
            )
            .await
            {
                tracing::error!(
                    tenant_id = %claims.tenant_id,
                    session_id = %session_id,
                    error = %error,
                    "session deleted but artifact tombstone cleanup failed"
                );
                return AppError::Internal(
                    "session deleted; artifact cleanup will be retried by retention recovery"
                        .to_string(),
                )
                .into_response();
            }
            Json(serde_json::json!({ "deleted": true })).into_response()
        }
        Err(e) => gateway_error_to_response(&e),
    }
}

/// POST `/api/v1/agent/sessions/{session_id}/cancel-turn` — cancel the active streaming turn.
pub(super) async fn cancel_session_turn(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let Some(handle) = get_agent_manager(&state).get_session(&session_id).await else {
        return AppError::NotFound(format!("session {session_id} not found")).into_response();
    };
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return AppError::Forbidden.into_response();
    }

    match get_agent_manager(&state)
        .cancel_running_turn(&session_id)
        .await
    {
        Ok(cancelled) => Json(CancelSessionTurnResponse {
            session_id,
            cancelled,
            status: if cancelled { "cancelling" } else { "idle" }.to_string(),
        })
        .into_response(),
        Err(e) => gateway_error_to_response(&e),
    }
}

/// GET `/api/v1/agent/sessions/{session_id}/approvals` - list only the
/// authenticated owner's unresolved durable approvals. Tool inputs and scope
/// hashes remain server-side.
pub(super) async fn list_session_approvals(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let Some(handle) = get_agent_manager(&state).get_session(&session_id).await else {
        return AppError::NotFound(format!("session {session_id} not found")).into_response();
    };
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return AppError::Forbidden.into_response();
    }
    match crate::semantic_kernel_store::list_runtime_approvals(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &session_id,
    )
    .await
    {
        Ok(approvals) => Json(serde_json::json!({ "approvals": approvals })).into_response(),
        Err(error) => AppError::Internal(error.to_string()).into_response(),
    }
}

/// GET `/api/v1/agent/sessions/{session_id}/questions` - list durable pending
/// questions for the authenticated session owner.
pub(super) async fn list_session_questions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let Some(handle) = get_agent_manager(&state).get_session(&session_id).await else {
        return AppError::NotFound(format!("session {session_id} not found")).into_response();
    };
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return AppError::Forbidden.into_response();
    }
    match crate::semantic_kernel_store::list_runtime_questions(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &session_id,
    )
    .await
    {
        Ok(questions) => Json(serde_json::json!({ "questions": questions })).into_response(),
        Err(error) => AppError::Internal(error.to_string()).into_response(),
    }
}

/// POST `/api/v1/agent/sessions/{session_id}/turn` — run a turn (non-streaming)
pub(super) async fn run_turn(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    Json(req): Json<RunTurnRequest>,
) -> impl IntoResponse {
    let (session_source, session_model) =
        if let Some(handle) = get_agent_manager(&state).get_session(&session_id).await {
            if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
                return AppError::Forbidden.into_response();
            }
            (handle.source, handle.model)
        } else {
            (String::new(), String::new())
        };

    if req.message.trim().is_empty() {
        return AppError::ValidationError("message cannot be empty".into()).into_response();
    }

    // Intercept skill slash commands: `/<skill-name> args` -> `$<skill-name> args`.
    let user_message = maybe_dispatch_skill_command(&req.message, &claims.tenant_id, &state.db)
        .await
        .unwrap_or(req.message);
    let user_message = sanitize_pm_user_message(&session_source, user_message);
    let message = wrap_pm_research_prompt(&session_source, user_message.clone());

    let response = if session_source.eq_ignore_ascii_case("pm") {
        let manager = get_agent_manager(&state).clone();
        let run_id = format!("pm-run-{}", uuid::Uuid::new_v4());
        let (effective_user_message, memory_artifact, memory_instruction) =
            build_pm_effective_user_message_for_task(
                &state.db,
                &session_id,
                &claims.tenant_id,
                &claims.sub,
                &user_message,
                &user_message,
            )
            .await;
        if let Some(artifact) = memory_artifact.as_ref() {
            crate::routes::memory_continuity::record_memory_citations(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                &session_id,
                Some(&run_id),
                artifact,
            )
            .await;
            state
                .pm_telemetry()
                .enqueue(PmTelemetryEvent::stage_attempt(
                    &run_id,
                    "memory",
                    1,
                    "completed",
                    Some(serde_json::json!({
                        "event": "memory.loaded",
                        "app": "pm",
                        "sessionId": session_id,
                        "memory": artifact,
                    })),
                    None,
                    None,
                    None,
                    None,
                    None,
                ))
                .await;
            record_pm_audit_event(
                state.telemetry_db(),
                &claims.tenant_id,
                &claims.sub,
                &run_id,
                "pm_session_memory_loaded",
                "info",
                "pm session memory injected into foreground turn context",
                Some(artifact),
            )
            .await;
        }
        let message = wrap_pm_research_prompt(&session_source, effective_user_message.clone());
        let stage_telemetry = state.pm_telemetry.clone();
        let run_id_for_stage = run_id.clone();
        let mut on_stage =
            move |stage: &str, status: &str, attempt: usize, detail: Option<serde_json::Value>| {
                stage_telemetry.try_enqueue(PmTelemetryEvent::stage_attempt(
                    &run_id_for_stage,
                    stage,
                    attempt,
                    status,
                    detail,
                    None,
                    None,
                    None,
                    None,
                    None,
                ));
            };
        let (result, quality) = match run_pm_orchestrated_turn(
            &state,
            manager,
            state.control_db(),
            &session_id,
            &session_source,
            &effective_user_message,
            message,
            &claims.sub,
            &claims.tenant_id,
            &session_model,
            Some(&run_id),
            None,
            None,
            memory_instruction,
            &mut on_stage,
            None,
        )
        .await
        {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    tenant_id = %claims.tenant_id,
                    user_id = %claims.sub,
                    error = %e,
                    "foreground PM orchestration failed; delivering local first-party synthesis"
                );
                let turn = build_pm_local_strategy_synthesis_turn(
                    &session_id,
                    &session_model,
                    &effective_user_message,
                    &format!("foreground PM orchestration failed: {e}"),
                    1,
                    &[],
                );
                let mut quality = degrade_pm_quality_with_reason(
                    evaluate_pm_answer_quality(&turn),
                    "foreground_orchestration_local_first_party_synthesis",
                    "Foreground PM orchestration failed; delivered a first-party grounded synthesis instead of an error response.",
                );
                quality.deliverable = true;
                (turn, quality)
            }
        };
        let mut result = result;
        let mut quality = quality;
        let mut has_answer_text = !result.text.trim().is_empty();
        let mut deliverable_for_status = pm_is_soft_deliverable_quality(&quality);
        let mut completion_recovered = false;
        if !has_answer_text || !deliverable_for_status {
            result = build_pm_local_strategy_synthesis_turn(
                &session_id,
                &session_model,
                &effective_user_message,
                "foreground PM quality gate did not produce a soft-deliverable answer",
                result.iterations.max(1),
                &result.tool_calls,
            );
            quality = degrade_pm_quality_with_reason(
                evaluate_pm_answer_quality(&result),
                "foreground_quality_gate_local_first_party_synthesis",
                "Foreground PM delivery used a first-party grounded synthesis instead of returning an empty or blocked answer.",
            );
            quality.deliverable = true;
            has_answer_text = !result.text.trim().is_empty();
            deliverable_for_status = true;
            completion_recovered = true;
        }
        let quality_score = pm_quality_delivery_score(&quality);
        upsert_pm_route_learning_feature(
            &state.db,
            &claims.tenant_id,
            "foreground.pm",
            Some("search"),
            pm_is_deliverable_quality(&quality),
            quality_score,
            0.0,
            0.0,
        )
        .await;
        upsert_pm_route_bandit_state(
            &state.db,
            &claims.tenant_id,
            "foreground.pm",
            Some("search"),
            quality_score,
            0.05,
        )
        .await;
        let completion_ready = has_answer_text || deliverable_for_status;
        debug_assert!(
            completion_ready,
            "PM foreground fallback must be deliverable"
        );
        let degraded_delivery = completion_ready && !quality.passed;
        let response_mode = if quality.citation_count > 0 || quality.domain_count > 0 {
            "deep_summary"
        } else {
            "llm_reply"
        };
        persist_pm_run_finish(
            &state.db,
            &run_id,
            &PmRunFinishPayload {
                status: "completed".to_string(),
                current_stage: Some("synthesize".to_string()),
                attempt: Some(result.iterations.max(1)),
                total_elapsed_ms: None,
                error_code: completion_recovered.then(|| "quality_gate_soft_delivered".to_string()),
                error_message: None,
                final_quality_score: Some(quality_score),
                metadata: Some(serde_json::json!({
                    "citationCount": quality.citation_count,
                    "domainCount": quality.domain_count,
                    "toolCallCount": quality.tool_call_count,
                    "triadCoverage": quality.triad_coverage,
                    "degradedDelivery": degraded_delivery,
                    "answerTextPresent": has_answer_text,
                    "completionRecovered": completion_recovered,
                    "responseMode": response_mode
                })),
            },
        )
        .await;
        persist_pm_session_memory_candidate(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            &user_message,
        )
        .await;
        crate::routes::memory_continuity::persist_unified_memory_candidate(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            Some(&session_id),
            "pm",
            &user_message,
        )
        .await;
        crate::routes::memory_continuity::persist_agent_turn_exact_archive(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            "pm",
            &run_id,
            &user_message,
            Some(&effective_user_message),
            &result.text,
            &result.tool_calls,
            Some(serde_json::json!({
                "source": "agent_session_run_turn_pm",
                "runId": run_id,
                "responseMode": response_mode,
                "qualityScore": quality_score,
                "qualityLevel": quality.quality_level,
                "deliverable": quality.deliverable,
                "degradedDelivery": degraded_delivery,
                "model": result.usage.model.clone(),
                "iterations": result.iterations
            })),
        )
        .await;
        persist_pm_session_summary_after_turn(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            &run_id,
            &user_message,
            &result.text,
            &quality,
            result.compacted.as_ref(),
        )
        .await;
        turn_to_run_turn_response(result, Some(quality), Some(&user_message))
    } else {
        let result = match get_agent_manager(&state)
            .run_turn(&session_id, message)
            .await
        {
            Ok(r) => r,
            Err(e) => return gateway_error_to_response(&e),
        };
        crate::routes::memory_continuity::persist_agent_turn_exact_archive(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            if session_source.trim().is_empty() {
                "chat"
            } else {
                &session_source
            },
            &format!("agent-turn-{}", uuid::Uuid::new_v4()),
            &user_message,
            None,
            &result.text,
            &result.tool_calls,
            Some(serde_json::json!({
                "source": "agent_session_run_turn",
                "sessionSource": session_source,
                "model": result.usage.model.clone(),
                "iterations": result.iterations,
                "hotReloaded": result.hot_reloaded
            })),
        )
        .await;
        turn_to_run_turn_response(result, None, None)
    };
    Json(response).into_response()
}

pub(super) async fn get_session_state(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let manager = get_agent_manager(&state);
    let handle = manager.get_session(&session_id).await;
    if let Some(handle) = handle.as_ref() {
        if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
            return AppError::Forbidden.into_response();
        }
    }

    match manager.get_session_state(&session_id).await {
        Some(session_state) => Json(serde_json::json!({
            "state": session_state.to_string(),
            "canonical": {
                "sessionId": session_id,
                "entry": canonical_entry_for_session_source(handle.as_ref().map(|h| h.source.as_str())),
                "source": handle.as_ref().map(|h| h.source.as_str()).unwrap_or("unknown"),
                "running": matches!(session_state, SessionState::Running),
                "resumable": true,
                "historyEndpoint": format!("/api/v1/agent/sessions/{}/history", session_id),
                "streamEndpoint": format!("/api/v1/agent/sessions/{}/stream", session_id),
                "cancelEndpoint": format!("/api/v1/agent/sessions/{}/cancel-turn", session_id),
            }
        }))
        .into_response(),
        None => AppError::NotFound(format!("session {session_id} not found")).into_response(),
    }
}

fn canonical_entry_for_session_source(source: Option<&str>) -> &'static str {
    match source
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "chat" | "pm" | "dataattribution" | "nl2sql" | "super_assistant" | "super-assistant" => {
            "super_assistant"
        }
        "rd" => "agent",
        _ => "agent",
    }
}

// ---------------------------------------------------------------------------
// Error conversion
// ---------------------------------------------------------------------------

fn gateway_error_status(error: &GatewayError) -> axum::http::StatusCode {
    match error {
        GatewayError::Validation(_) => axum::http::StatusCode::BAD_REQUEST,
        _ => error.http_status(),
    }
}

fn gateway_error_to_response(e: &GatewayError) -> axum::response::Response {
    let status = gateway_error_status(e);
    let body = ErrorResponse {
        error: e.to_string(),
        code: status.as_u16(),
    };
    (status, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// GET `/api/v1/agent/commands` — returns available slash commands for the frontend.
///
/// Returns both built-in commands (hardcoded) and skill-registered commands
/// (dynamic, fetched from the skills registry for this tenant).
pub(super) async fn get_commands(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> impl IntoResponse {
    // Built-in commands — always available
    let builtin: Vec<serde_json::Value> = vec![
        serde_json::json!({"name": "help",      "description": "Show available slash commands"}),
        serde_json::json!({"name": "commands",  "description": "Open the slash command palette"}),
        serde_json::json!({"name": "status",    "description": "Show current session status"}),
        serde_json::json!({"name": "compact",   "description": "Compact local session history"}),
        serde_json::json!({"name": "model",     "description": "Show or switch the active model", "hint": "/model [model]"}),
        serde_json::json!({"name": "permissions", "description": "Show the active permission mode"}),
        serde_json::json!({"name": "clear",     "description": "Start a fresh session"}),
        serde_json::json!({"name": "cost",      "description": "Show cumulative token usage for this session"}),
        serde_json::json!({"name": "mcp",       "description": "Inspect configured MCP servers", "hint": "/mcp [list|show <server>|help]"}),
        serde_json::json!({"name": "memory",     "description": "Inspect loaded instruction memory files"}),
        serde_json::json!({"name": "skills",    "description": "Inspect enabled skills", "hint": "/skills [list|help]"}),
        serde_json::json!({"name": "session",   "description": "Start a fresh session", "hint": "/session new"}),
        serde_json::json!({"name": "export",    "description": "Export the current conversation to a file", "hint": "/export [json|markdown]"}),
    ];

    // Dynamic skill commands — fetched from the database
    let skill_commands: Vec<serde_json::Value> = sqlx::query_as::<_, (String,)>(
        "SELECT name FROM skills_registry WHERE tenant_id = ? AND enabled = true",
    )
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(name,)| {
        serde_json::json!({
            "name": name.to_lowercase(),
            "description": format!("Execute skill: {}", name),
            "source": "skill"
        })
    })
    .collect();

    Json(serde_json::json!({
        "builtin": builtin,
        "skills": skill_commands,
    }))
}

// ---------------------------------------------------------------------------
// Session rename
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RenameSessionBody {
    pub name: String,
}

/// PATCH `/api/v1/agent/sessions/{session_id}` — rename a session
pub(super) async fn rename_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    Json(body): Json<RenameSessionBody>,
) -> impl IntoResponse {
    // Ownership check
    if let Some(handle) = get_agent_manager(&state).get_session(&session_id).await {
        if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
            return AppError::Forbidden.into_response();
        }
    }

    match get_agent_manager(&state)
        .rename_session(&session_id, &body.name)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "renamed": true, "name": body.name })).into_response(),
        Err(e) => gateway_error_to_response(&e),
    }
}

// ---------------------------------------------------------------------------
// Toggle pin session
// ---------------------------------------------------------------------------

/// POST `/api/v1/agent/sessions/{session_id}/pin` — toggle pin state
pub(super) async fn toggle_pin_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    // Ownership check
    if let Some(handle) = get_agent_manager(&state).get_session(&session_id).await {
        if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
            return AppError::Forbidden.into_response();
        }
    }

    match get_agent_manager(&state)
        .toggle_pin_session(&session_id)
        .await
    {
        Ok(is_pinned) => {
            Json(serde_json::json!({ "pinned": true, "is_pinned": is_pinned })).into_response()
        }
        Err(e) => gateway_error_to_response(&e),
    }
}

// ---------------------------------------------------------------------------
// Toggle bookmark session
// ---------------------------------------------------------------------------

/// POST `/api/v1/agent/sessions/{session_id}/bookmark` — toggle bookmark state
pub(super) async fn toggle_bookmark_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    // Ownership check
    if let Some(handle) = get_agent_manager(&state).get_session(&session_id).await {
        if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
            return AppError::Forbidden.into_response();
        }
    }

    match get_agent_manager(&state)
        .toggle_bookmark_session(&session_id)
        .await
    {
        Ok(is_bookmarked) => {
            Json(serde_json::json!({ "bookmarked": true, "is_bookmarked": is_bookmarked }))
                .into_response()
        }
        Err(e) => gateway_error_to_response(&e),
    }
}

// ---------------------------------------------------------------------------
// Branch session — fork from a specific message
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub(super) struct BranchSessionBody {
    #[allow(dead_code)]
    message_index: Option<usize>,
    #[allow(dead_code)]
    message_id: Option<i64>,
}

pub(super) async fn branch_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    Json(body): Json<BranchSessionBody>,
) -> impl IntoResponse {
    if let Some(handle) = get_agent_manager(&state).get_session(&session_id).await {
        if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
            return AppError::Forbidden.into_response();
        }
    }

    // Create a new session with the same config as the source
    let Some(source) = get_agent_manager(&state).get_session(&session_id).await else {
        return AppError::NotFound("Source session not found".into()).into_response();
    };

    if let Err(error) =
        enforce_user_visible_session_limit(&state, &claims.tenant_id, &claims.sub, &source.source)
            .await
    {
        return error.into_response();
    }

    let create_result = get_agent_manager(&state)
        .create_session(
            &claims.sub,
            &claims.tenant_id,
            None,
            Some(&source.model),
            &source.source,
            None,
            None,
            None,
        )
        .await;

    match create_result {
        Ok(handle) => {
            // Branch sessions intentionally start with the same workspace/model
            // metadata and keep conversation copying as a caller-visible option.
            // This avoids silently duplicating large histories into a new Agent
            // session until message-level branching is persisted end-to-end.
            let share_url = format!("/agent/sessions/{}", handle.session_id);
            Json(serde_json::json!({
                "session": {
                    "session_id": handle.session_id,
                    "name": format!("{} (branch)", source.name),
                    "model": source.model,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                    "workspace": source.workspace,
                    "state": "idle",
                    "mcp_servers": source.session_metadata.mcp_servers,
                    "skills": source.session_metadata.skills,
                    "permission_mode": source.session_metadata.permission_mode,
                    "branch_from_session_id": source.session_id,
                    "branch_from_message_index": body.message_index,
                    "history_copied": false,
                },
                "share_url": share_url,
            }))
            .into_response()
        }
        Err(e) => gateway_error_to_response(&e),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------
