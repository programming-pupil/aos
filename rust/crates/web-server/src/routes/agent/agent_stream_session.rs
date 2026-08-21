use super::agent_chat_turn_engine::{
    chat_memory_mode_str, plan_chat_turn, ChatTurnEngineInput, EffectiveChatSearchMode,
};
use super::*;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use pm_domain::stream_session as stream_policy;
use serde_json::json;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StreamSessionRequest {
    #[serde(default)]
    message: String,
    #[serde(default, alias = "attachments")]
    images: Vec<PmTaskImageInput>,
    #[serde(default)]
    documents: Vec<PmTaskDocumentInput>,
    #[serde(default, alias = "turn_options")]
    turn_options: ChatTurnOptions,
    #[serde(default)]
    approval: Option<StreamApprovalDecision>,
    #[serde(default)]
    interaction: Option<StreamInteractionResponse>,
    #[serde(default)]
    interactions: Vec<StreamInteractionResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamApprovalDecision {
    request_id: String,
    decision: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamInteractionResponse {
    interaction_id: String,
    answer: String,
    idempotency_key: String,
}

struct StreamTurnCancelOnDrop {
    manager: Arc<AgentSessionManager>,
    session_id: String,
    active: bool,
}

impl Drop for StreamTurnCancelOnDrop {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let manager = self.manager.clone();
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            match manager.cancel_running_turn(&session_id).await {
                Ok(true) => {
                    tracing::info!(session_id = %session_id, "stream dropped; cancellation requested for running turn");
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::debug!(session_id = %session_id, error = %error, "stream dropped; no cancellable turn found");
                }
            }
        });
    }
}

fn stream_session_sse_heartbeat_secs() -> u64 {
    env::var("AOSD_AGENT_STREAM_HEARTBEAT_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(15)
        .clamp(5, 60)
}

fn stream_session_ping_event() -> axum::response::sse::Event {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    axum::response::sse::Event::default()
        .event("ping")
        .data(json!({ "timestamp": timestamp }).to_string())
}

fn stream_session_image_max_count() -> usize {
    stream_policy::image_max_count()
}

fn stream_session_image_max_bytes() -> usize {
    stream_policy::image_max_bytes()
}

fn stream_session_image_max_total_bytes() -> usize {
    stream_policy::image_max_total_bytes()
}

fn stream_session_image_summary_max_tokens() -> u32 {
    stream_policy::image_summary_max_tokens()
}

fn stream_session_image_summary_timeout_secs() -> u64 {
    stream_policy::image_summary_timeout_secs()
}

fn stream_session_image_summary_model(session_model: &str) -> String {
    stream_policy::image_summary_model(session_model)
}

fn stream_summary_text_indicates_no_vision(summary: &str) -> bool {
    stream_policy::summary_text_indicates_no_vision(summary)
}

fn stream_image_context_warning_message(error: &str) -> &'static str {
    stream_policy::image_context_warning_message(error)
}

fn stream_session_scenario(source: &str) -> &'static str {
    stream_policy::scenario(source)
}

fn stream_notification_preview(text: &str, max_chars: usize) -> String {
    stream_policy::notification_preview(text, max_chars)
}

fn stream_notification_route(source: &str) -> (&'static str, &'static str, &'static str) {
    stream_policy::notification_route(source)
}

fn spawn_stream_answer_completed_notification(
    state: AppState,
    tenant_id: String,
    session_id: String,
    session_source: String,
    question: String,
    answer_text: String,
) {
    let (capability_key, event_key, title) = stream_notification_route(&session_source);
    let question_preview = stream_notification_preview(&question, 800);
    let answer_preview =
        stream_notification_preview(&extract_pm_visible_answer_text(&answer_text), 3500);
    let text = format!(
        "{title}\n\n会话ID: {session_id}\n\n问题:\n{question_preview}\n\n回答摘要:\n{answer_preview}"
    );
    tokio::spawn(async move {
        if !crate::routes::task_control_worker::legacy_capability_notifications_enabled(
            &state, &tenant_id,
        )
        .await
        {
            return;
        }
        match crate::routes::bot_agents_outbound::notify_capability_event(
            &state,
            &tenant_id,
            capability_key,
            event_key,
            crate::routes::bot_agents_outbound::BotOutboundMessage {
                title: Some(title.to_string()),
                text,
                external_conversation_id: None,
            },
        )
        .await
        {
            Ok(summary) if summary.attempted > 0 => {
                tracing::info!(
                    tenant_id = %tenant_id,
                    session_id = %session_id,
                    session_source = %session_source,
                    attempted = summary.attempted,
                    sent = summary.sent,
                    failed = summary.failed,
                    "stream answer completion bot notification dispatched"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    session_id = %session_id,
                    session_source = %session_source,
                    "stream answer completion bot notification skipped: {}",
                    error
                );
            }
        }
    });
}

async fn resolve_stream_image_summary_api_client(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    model: &str,
    scenario: &str,
) -> Result<(api::ProviderClient, Option<String>, String, String), AppError> {
    let requested_model = model.trim();
    if requested_model.is_empty() {
        return Err(AppError::ValidationError(
            "image summary model cannot be empty".to_string(),
        ));
    }

    let fallback_client = || {
        if !runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK") {
            return Err(AppError::Internal(
                "no approved tenant image model is configured".to_string(),
            ));
        }
        api::ProviderClient::from_model(requested_model)
            .map(|client| {
                (
                    client,
                    None,
                    "env-fallback".to_string(),
                    requested_model.to_string(),
                )
            })
            .map_err(|e| AppError::Internal(format!("no API keys available: {e}")))
    };

    let Some(config_registry) = state.config_registry.as_ref() else {
        return fallback_client();
    };

    let runtime_config = match config_registry
        .load_user_config(tenant_id, user_id, Some(scenario))
        .await
    {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                user_id = %user_id,
                scenario = %scenario,
                error = %e,
                "stream image summary config load failed; using env fallback"
            );
            return fallback_client();
        }
    };

    if runtime_config.api_keys.is_empty() {
        tracing::info!(
            tenant_id = %tenant_id,
            user_id = %user_id,
            scenario = %scenario,
            "no scoped API keys for stream image summary; using env fallback"
        );
        return fallback_client();
    }

    for entry in &runtime_config.api_keys {
        match api::build_provider(
            &entry.provider,
            requested_model,
            &entry.key,
            entry.base_url.as_deref(),
        ) {
            Ok(client) => {
                return Ok((
                    client,
                    Some(entry.id.clone()),
                    entry.provider.clone(),
                    requested_model.to_string(),
                ));
            }
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    scenario = %scenario,
                    key_id = %entry.id,
                    provider = %entry.provider,
                    model = %requested_model,
                    error = %error,
                    "stream image summary key init failed; trying next key"
                );
            }
        }
    }

    fallback_client()
}

pub(crate) async fn build_stream_image_summary(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_source: &str,
    session_model: &str,
    user_message: &str,
    input_context: &PmTaskInputContext,
) -> Result<(String, serde_json::Value), AppError> {
    let max_count = stream_session_image_max_count();
    if input_context.images.len() > max_count {
        return Err(AppError::ValidationError(format!(
            "too many images: maximum {max_count}"
        )));
    }

    let max_bytes = stream_session_image_max_bytes();
    let max_total = stream_session_image_max_total_bytes();
    let mut total_loaded = 0usize;
    let mut image_blocks = Vec::<api::InputContentBlock>::new();
    let mut accepted_labels = Vec::<String>::new();
    let mut skipped = Vec::<serde_json::Value>::new();

    for (idx, image) in input_context.images.iter().enumerate() {
        match super::agent_pm_task_api::pm_read_task_image_bytes(
            &state.data_dir,
            user_id,
            image,
            max_bytes,
        )
        .await
        {
            Ok((bytes, media_type, filename)) => {
                if total_loaded.saturating_add(bytes.len()) > max_total {
                    skipped.push(serde_json::json!({
                        "index": idx + 1,
                        "url": image.url,
                        "reason": format!("total image bytes exceeded max {max_total}"),
                    }));
                    continue;
                }
                total_loaded = total_loaded.saturating_add(bytes.len());
                image_blocks.push(api::InputContentBlock::Image {
                    media_type: media_type.clone(),
                    source_type: api::ImageSourceType::Base64,
                    data: BASE64.encode(bytes),
                });
                accepted_labels.push(format!("#{} {}", idx + 1, filename));
            }
            Err(reason) => skipped.push(serde_json::json!({
                "index": idx + 1,
                "url": image.url,
                "reason": reason,
            })),
        }
    }

    if image_blocks.is_empty() {
        return Err(AppError::ValidationError(
            "no usable images after validation".to_string(),
        ));
    }

    let scenario = stream_session_scenario(session_source);
    let summary_model = stream_session_image_summary_model(session_model);
    let (provider, _api_key_id, provider_name, effective_model) =
        resolve_stream_image_summary_api_client(
            state,
            tenant_id,
            user_id,
            &summary_model,
            scenario,
        )
        .await?;
    let provider = crate::governed_provider::GovernedProviderClient::new(
        provider,
        state.db.clone(),
        tenant_id,
        user_id,
        format!("agent:image-summary:{scenario}"),
    );

    let question = if user_message.trim().is_empty() {
        "请基于上传图片提炼关键事实并回答。".to_string()
    } else {
        user_message.trim().to_string()
    };
    let mut content = Vec::<api::InputContentBlock>::new();
    content.push(api::InputContentBlock::Text {
        text: format!(
            "你是图像分析助手。请只基于图片可见信息回答。\n用户问题：{question}\n输出要求：\n1) 逐图提炼可见事实；\n2) 输出综合结论；\n3) 标注不确定项；\n4) 不要臆测。"
        ),
    });
    content.extend(image_blocks);

    let api_req = api::MessageRequest {
        model: effective_model.clone(),
        max_tokens: stream_session_image_summary_max_tokens(),
        messages: vec![api::InputMessage {
            role: "user".to_string(),
            content,
        }],
        system: Some(
            "You are a careful vision analyst. Keep outputs concise, factual, and uncertainty-aware."
                .to_string(),
        ),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let timeout_secs = stream_session_image_summary_timeout_secs();
    let response = timeout(
        Duration::from_secs(timeout_secs),
        provider.send_message(&api_req),
    )
    .await
    .map_err(|_| {
        AppError::Internal(format!(
            "image summary model call timed out after {timeout_secs}s"
        ))
    })?
    .map_err(|error| AppError::Internal(format!("image summary model call failed: {error}")))?;

    let summary = response
        .content
        .iter()
        .filter_map(|block| {
            if let api::OutputContentBlock::Text { text } = block {
                Some(text.trim())
            } else {
                None
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if summary.trim().is_empty() {
        return Err(AppError::Internal(
            "image summary model returned empty text".to_string(),
        ));
    }
    if stream_summary_text_indicates_no_vision(&summary) {
        return Err(AppError::Internal(format!(
            "image summary indicates no vision capability (model={effective_model})"
        )));
    }

    let diagnostics = serde_json::json!({
        "imageCount": input_context.images.len(),
        "accepted": accepted_labels,
        "skipped": skipped,
        "provider": provider_name,
        "summaryModelRequested": summary_model,
        "summaryModelEffective": effective_model,
        "summaryModel": effective_model,
        "scenario": scenario,
        "totalLoadedBytes": total_loaded,
    });
    Ok((summary, diagnostics))
}

pub(crate) fn merge_stream_user_message_with_image_summary(
    user_message: &str,
    image_summary: &str,
    image_count: usize,
) -> String {
    stream_policy::merge_user_message_with_image_summary(user_message, image_summary, image_count)
}

/// POST `/api/v1/agent/sessions/{session_id}/stream` — SSE stream for real-time agent output.
/// Accepts POST with JSON body to avoid URL length limits for long messages:
///   { "message": "..." }
///
/// Uses truly streaming SSE: thinking/text/tool events are emitted as generated.
#[allow(clippy::too_many_lines)]
pub(super) async fn stream_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    request: axum::extract::Request<Body>,
) -> impl IntoResponse {
    // Auth check: return proper error for missing/unauthorized sessions.
    let Some(handle) = get_agent_manager(&state).get_session(&session_id).await else {
        return AppError::NotFound(format!("session {session_id} not found")).into_response();
    };
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return AppError::Forbidden.into_response();
    }
    let session_source = handle.source.clone();

    // Support both POST (JSON body) and GET (query param) for backward compatibility.
    let (
        raw_message,
        request_images,
        request_documents,
        turn_options,
        approval_request,
        interaction_requests,
    ) = if request.method() == axum::http::Method::POST {
        let (_, body) = request.into_parts();
        let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
            Ok(b) => b,
            Err(e) => {
                return AppError::Internal(format!("failed to read request body: {e}"))
                    .into_response()
            }
        };
        let parsed: StreamSessionRequest = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(_) => return AppError::ValidationError("invalid JSON body".into()).into_response(),
        };
        let StreamSessionRequest {
            message,
            images,
            documents,
            turn_options,
            approval,
            interaction,
            mut interactions,
        } = parsed;
        if let Some(interaction) = interaction {
            interactions.push(interaction);
        }
        (
            message,
            images,
            documents,
            turn_options,
            approval,
            interactions,
        )
    } else {
        (
            String::new(),
            Vec::new(),
            Vec::new(),
            ChatTurnOptions::default(),
            None,
            Vec::new(),
        )
    };

    if approval_request.is_some() && !interaction_requests.is_empty() {
        return AppError::ValidationError(
            "a stream request cannot resolve an approval and a user question together".into(),
        )
        .into_response();
    }

    let approval_resume = if let Some(approval) = approval_request {
        let stored = match crate::semantic_kernel_store::get_runtime_approval(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            approval.request_id.trim(),
        )
        .await
        {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                return AppError::ValidationError(
                    "approval request is not pending in this authenticated session".to_string(),
                )
                .into_response()
            }
            Err(error) => return AppError::Internal(error.to_string()).into_response(),
        };
        let decision = if stored.expired {
            runtime::RuntimeApprovalDecision::Expired
        } else {
            match approval.decision.trim().to_ascii_lowercase().as_str() {
                "approve" | "approved" => runtime::RuntimeApprovalDecision::Approved,
                "deny" | "denied" | "reject" | "rejected" => {
                    runtime::RuntimeApprovalDecision::Denied
                }
                "cancel" | "cancelled" | "canceled" => runtime::RuntimeApprovalDecision::Cancelled,
                _ => {
                    return AppError::ValidationError(
                        "approval decision must be approve, deny, or cancel".to_string(),
                    )
                    .into_response()
                }
            }
        };
        Some(runtime::DeferredApprovalDecision {
            tool_use_id: stored.invocation_id,
            decision,
            reason: approval.reason,
        })
    } else {
        None
    };

    let interaction_resume = if interaction_requests.is_empty() {
        None
    } else {
        let answers = interaction_requests
            .iter()
            .map(
                |interaction| crate::semantic_kernel_store::RuntimeUserQuestionAnswer {
                    interaction_id: interaction.interaction_id.trim(),
                    answer: &interaction.answer,
                    idempotency_key: interaction.idempotency_key.trim(),
                },
            )
            .collect::<Vec<_>>();
        match crate::semantic_kernel_store::respond_to_runtime_user_questions(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &claims.sub,
            &session_id,
            &answers,
        )
        .await
        {
            Ok(results) => Some(results),
            Err(error) => return AppError::ValidationError(error.to_string()).into_response(),
        }
    };

    // Intercept skill slash commands: `/<skill-name> args` -> `$<skill-name> args`.
    let original_user_message = if approval_resume.is_some() || interaction_resume.is_some() {
        String::new()
    } else {
        maybe_dispatch_skill_command(&raw_message, &claims.tenant_id, &state.db)
            .await
            .unwrap_or(raw_message)
    };
    let mut user_message = original_user_message.clone();
    let mut image_context_warning_payload: Option<serde_json::Value> = None;
    let mut image_context_trace_payload: Option<serde_json::Value> = None;
    if !request_images.is_empty() || !request_documents.is_empty() {
        match super::agent_pm_task_api::normalize_pm_task_input_context(
            &request_images,
            &request_documents,
            &claims.sub,
        ) {
            Ok(Some(input_context)) => {
                if !input_context.documents.is_empty() {
                    if let Some((document_context, diag)) =
                        super::agent_pm_task_api::build_pm_document_context_summary(
                            &state.data_dir,
                            &claims.sub,
                            &input_context,
                            &user_message,
                        )
                        .await
                    {
                        if !document_context.trim().is_empty() {
                            user_message =
                                super::agent_pm_task_api::merge_pm_user_message_with_document_context(
                                    &user_message,
                                    &document_context,
                                );
                        }
                        tracing::info!(
                            session_id = %session_id,
                            tenant_id = %claims.tenant_id,
                            user_id = %claims.sub,
                            diagnostics = %diag,
                            "stream session document context injected"
                        );
                    }
                }
                if input_context.images.is_empty() {
                    // Documents are injected above; no image summary is needed.
                } else {
                    match build_stream_image_summary(
                        &state,
                        &claims.tenant_id,
                        &claims.sub,
                        &session_source,
                        &handle.model,
                        &user_message,
                        &input_context,
                    )
                    .await
                    {
                        Ok((summary, diag)) => {
                            user_message = merge_stream_user_message_with_image_summary(
                                &user_message,
                                &summary,
                                input_context.images.len(),
                            );
                            image_context_trace_payload = Some(serde_json::json!({
                                "event": "chat.multimodal.image_summary_fallback",
                                "imageCount": input_context.images.len(),
                                "diagnostics": diag,
                            }));
                            tracing::info!(
                                session_id = %session_id,
                                tenant_id = %claims.tenant_id,
                                user_id = %claims.sub,
                                image_count = input_context.images.len(),
                                diagnostics = %diag,
                                "stream session image context summary injected"
                            );
                        }
                        Err(error) => {
                            let error_text = error.to_string();
                            image_context_warning_payload = Some(serde_json::json!({
                                "code": "image_context_summary_skipped",
                                "message": stream_image_context_warning_message(&error_text),
                                "detail": error_text,
                                "imageCount": input_context.images.len(),
                            }));
                            tracing::warn!(
                                session_id = %session_id,
                                tenant_id = %claims.tenant_id,
                                user_id = %claims.sub,
                                image_count = input_context.images.len(),
                                error = %error,
                                "stream session image context summary skipped"
                            );
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(error) => return error.into_response(),
        }
    }
    let mut user_message = sanitize_pm_user_message(&session_source, user_message);

    if approval_resume.is_none() && interaction_resume.is_none() && user_message.trim().is_empty() {
        return AppError::ValidationError("message cannot be empty".into()).into_response();
    }

    let mut chat_artifact_evidence = Vec::<serde_json::Value>::new();
    let mut chat_trace = Vec::<serde_json::Value>::new();

    if approval_resume.is_none()
        && interaction_resume.is_none()
        && session_source.eq_ignore_ascii_case("chat")
    {
        if let Some(trace) = image_context_trace_payload.take() {
            chat_trace.push(trace);
        }
        let chat_memory_context = crate::routes::memory_continuity::build_unified_memory_prompt(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            Some(&session_id),
            "chat",
            &user_message,
            chat_memory_mode_str(&turn_options.memory_mode),
        )
        .await;
        if let Some((memory_prompt, memory_artifact)) = chat_memory_context {
            chat_trace.push(json!({
                "event": "memory.loaded",
                "app": "chat",
                "sessionId": session_id,
                "memory": memory_artifact,
            }));
            chat_artifact_evidence.push(json!({
                "type": "memory",
                "title": "Long-term memory",
                "instruction": memory_prompt,
                "metadata": memory_artifact,
            }));
        }

        if let Some(file_context) = turn_options.file_context.as_ref() {
            if matches!(
                file_context.mode,
                ChatFileContextMode::Workspace
                    | ChatFileContextMode::Selected
                    | ChatFileContextMode::AllAttached
            ) {
                if let Some((file_prompt, citations)) =
                    crate::routes::chat_intelligence::build_chat_workspace_file_prompt(
                        &state,
                        &claims.tenant_id,
                        &claims.sub,
                        &user_message,
                        &file_context.file_ids,
                        file_context.strict_grounding,
                    )
                    .await
                {
                    crate::routes::memory_continuity::mark_thread_memory_polluted(
                        &state.db,
                        &claims.tenant_id,
                        &claims.sub,
                        &session_id,
                        "file_workspace_context",
                        Some(json!({ "fileIds": file_context.file_ids })),
                    )
                    .await;
                    user_message = format!("{file_prompt}\n\n{user_message}");
                    chat_artifact_evidence.push(json!({
                        "type": "file",
                        "title": "File workspace evidence",
                        "items": citations,
                    }));
                } else if file_context.strict_grounding {
                    user_message = format!(
                        "[File workspace evidence]\nStrict file grounding is enabled, but no matching indexed file evidence was found. If the answer is not supported by attached excerpts, say evidence is insufficient.\n[/File workspace evidence]\n\n{user_message}"
                    );
                    chat_trace.push(json!({
                        "event": "chat.file_workspace.no_match",
                        "strictGrounding": true,
                    }));
                }
            }
        }
    }

    let message = wrap_pm_research_prompt(&session_source, user_message.clone());

    if approval_resume.is_none()
        && interaction_resume.is_none()
        && session_source.eq_ignore_ascii_case("pm")
    {
        let task_id = format!("pm-research-task-{}", uuid::Uuid::new_v4());
        let task_manager = pm_research_task_manager().clone();
        if let Err(e) = task_manager
            .create_task(
                state.control_db(),
                state.pm_telemetry(),
                &task_id,
                &session_id,
                &user_message,
                None,
                &claims.tenant_id,
                &claims.sub,
            )
            .await
        {
            return e.into_response();
        }
        record_pm_audit_event(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &task_id,
            "pm_task_created_from_stream",
            "info",
            "pm stream switched to background research task",
            Some(&serde_json::json!({
                "sessionId": session_id,
                "model": handle.model.clone(),
                "source": handle.source.clone(),
            })),
        )
        .await;
        if let Err(error) = run_pm_background_runtime_cycle(&state).await {
            tracing::warn!(
                task_id = %task_id,
                session_id = %session_id,
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                error = %error,
                "trigger pm runtime cycle failed after stream-to-background switch"
            );
        }
        return stream_pm_research_task_events(State(state), Extension(claims), Path(task_id))
            .await
            .into_response();
    }

    let manager = get_agent_manager(&state).clone();
    let cancel_manager = manager.clone();
    let session_id_for_error = session_id.clone();
    let session_id_for_disconnect_cancel = session_id.clone();
    let stream_started_at = Instant::now();

    // Channel for real-time SSE events. Buffered so the model can generate ahead.
    // 2048 is enough for ~32K individual token events (e.g. thinking deltas),
    // which covers even extremely verbose extended-thinking outputs.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<agent_gateway::AgentEvent>(2048);

    // Channel for the turn result (sent when the turn finishes).
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel::<
        Result<agent_gateway::AgentTurnRunOutcome, agent_gateway::GatewayError>,
    >(1);

    // Spawn the turn so we can forward channel events while awaiting the result.
    let mut turn_policy = agent_gateway::AgentTurnOptions::default();
    let mut effective_search_mode = EffectiveChatSearchMode::Off;
    if approval_resume.is_none()
        && interaction_resume.is_none()
        && session_source.eq_ignore_ascii_case("chat")
    {
        let memory_instructions = chat_artifact_evidence
            .iter()
            .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("memory"))
            .filter_map(|item| item.get("instruction").and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let plan = plan_chat_turn(ChatTurnEngineInput {
            state: &state,
            tenant_id: &claims.tenant_id,
            user_id: &claims.sub,
            session_id: &session_id,
            model: &handle.model,
            message: &user_message,
            turn_options: turn_options.clone(),
            memory_instructions,
            has_documents: !request_documents.is_empty(),
            mark_memory_pollution: true,
            reasoning_budget_override: None,
        })
        .await;
        effective_search_mode = plan.search_mode;
        let budget = plan.reasoning_budget;
        turn_policy = plan.options;
        tracing::info!(
            session_id = %session_id,
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            search_mode = effective_search_mode.as_str(),
            document_count = request_documents.len(),
            reasoning_budget = ?budget,
            "chat turn adaptive reasoning budget selected"
        );
        chat_trace.extend(plan.trace);
    }

    tokio::spawn(async move {
        let r = if let Some(results) = interaction_resume {
            manager
                .resume_turn_streaming_with_options(&session_id, results, tx, turn_policy)
                .await
        } else if let Some(decision) = approval_resume {
            manager
                .resume_turn_streaming_with_approval_decisions(
                    &session_id,
                    vec![decision],
                    tx,
                    turn_policy,
                )
                .await
        } else {
            manager
                .run_turn_streaming_with_options(&session_id, message, tx, turn_policy)
                .await
        };
        let _ = result_tx.send(r).await;
    });

    let sse_stream = try_stream! {
        let _cancel_on_drop = StreamTurnCancelOnDrop {
            manager: cancel_manager.clone(),
            session_id: session_id_for_disconnect_cancel.clone(),
            active: true,
        };

        if let Some(payload) = image_context_warning_payload.clone() {
            yield axum::response::sse::Event::default()
                .event("image_context_warning")
                .data(payload.to_string());
        }

        let mut first_text_delta_ms: Option<u128> = None;
        let mut text_delta_count: usize = 0;
        let mut tool_event_seen = false;
        let heartbeat_secs = stream_session_sse_heartbeat_secs();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(heartbeat_secs));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut heartbeat_ready = false;

        // Forward channel events as they arrive (real-time streaming).
        loop {
            let event = tokio::select! {
                event = rx.recv() => event,
                _ = heartbeat.tick() => {
                    if heartbeat_ready {
                        yield stream_session_ping_event();
                    } else {
                        heartbeat_ready = true;
                    }
                    continue;
                }
            };
            let Some(event) = event else {
                break;
            };
            match &event {
                agent_gateway::AgentEvent::TextDelta { .. } => {
                    text_delta_count = text_delta_count.saturating_add(1);
                    if first_text_delta_ms.is_none() {
                        first_text_delta_ms = Some(stream_started_at.elapsed().as_millis());
                    }
                }
                agent_gateway::AgentEvent::ToolUseStart { .. }
                | agent_gateway::AgentEvent::ToolUseInput { .. }
                | agent_gateway::AgentEvent::ToolUseEnd { .. }
                | agent_gateway::AgentEvent::ToolResult { .. } => {
                    tool_event_seen = true;
                }
                _ => {}
            }
            // Skip events that are re-emitted from the TurnResult to avoid duplication.
            match &event {
                agent_gateway::AgentEvent::Usage { .. }
                | agent_gateway::AgentEvent::SessionCompacted { .. }
                | agent_gateway::AgentEvent::Iteration { .. }
                | agent_gateway::AgentEvent::StreamStart { .. }
                | agent_gateway::AgentEvent::StreamEnd { .. }
                | agent_gateway::AgentEvent::Error { .. }
                | agent_gateway::AgentEvent::HookProgress { .. }
                | agent_gateway::AgentEvent::Ping { .. } => continue,
                // ThinkingStart and ThinkingEnd are emitted during the turn and
                // forwarded here — they are NOT re-emitted in the TurnResult,
                // so they must NOT be skipped.
                _ => {}
            }

            yield axum::response::sse::Event::default()
                .event(event.sse_event())
                .data(event.sse_data());
        }

        // Channel closed — the turn has finished. Emit summary events from the result.
        let turn_result = loop {
            let result = tokio::select! {
                result = result_rx.recv() => result,
                _ = heartbeat.tick() => {
                    yield stream_session_ping_event();
                    continue;
                }
            };
            break result;
        };
        match turn_result {
            Some(Ok(agent_gateway::AgentTurnRunOutcome::Completed(turn))) => {
                let stream_mode = if text_delta_count > 0 && tool_event_seen {
                    "tool_interleaved"
                } else if text_delta_count > 0 {
                    "token_delta"
                } else {
                    "final_text_typewriter"
                };
                let elapsed_ms = stream_started_at.elapsed().as_millis();
                if session_source.eq_ignore_ascii_case("pm") {
                    let pm_quality = evaluate_pm_answer_quality(&turn);
                    yield axum::response::sse::Event::default()
                        .event("pm_quality")
                        .data(serde_json::to_string(&pm_quality).unwrap_or_else(|_| "{}".to_string()));
                }

                yield axum::response::sse::Event::default()
                    .event("usage")
                    .data(serde_json::to_string(&UsageDto::from(turn.usage.clone())).unwrap_or_else(|_| "{}".to_string()));

                for tc in &turn.tool_calls {
                    yield axum::response::sse::Event::default()
                        .event("tool_call")
                        .data(serde_json::to_string(&ToolCallDto::from(tc.clone())).unwrap_or_else(|_| "{}".to_string()));
                }
                crate::routes::memory_continuity::record_memory_tool_citations(
                    &state.db,
                    &claims.tenant_id,
                    &claims.sub,
                    &turn.session_id,
                    None,
                    &turn.tool_calls,
                )
                .await;
                if session_source.eq_ignore_ascii_case("chat") {
                    crate::routes::memory_continuity::persist_agent_turn_exact_archive(
                        &state.db,
                        &claims.tenant_id,
                        &claims.sub,
                        &turn.session_id,
                        "chat",
                        &format!("chat-stream-{}", uuid::Uuid::new_v4()),
                        &original_user_message,
                        Some(&user_message),
                        &turn.text,
                        &turn.tool_calls,
                        Some(json!({
                            "source": "agent_stream_session",
                            "streamMode": stream_mode,
                            "searchMode": effective_search_mode.as_str(),
                            "model": turn.usage.model.clone(),
                            "iterations": turn.iterations,
                            "hotReloaded": turn.hot_reloaded
                        })),
                    )
                    .await;
                }

                if let Some(ref thinking) = turn.thinking {
                    yield axum::response::sse::Event::default()
                        .event("thinking")
                        .data(serde_json::json!({ "thinking": thinking }).to_string());
                }

                if !turn.text.is_empty() {
                    let visible_text = if session_source.eq_ignore_ascii_case("pm") {
                        extract_pm_visible_answer_text(&turn.text)
                    } else {
                        turn.text.clone()
                    };
                    yield axum::response::sse::Event::default()
                        .event("text")
                        .data(serde_json::json!({ "text": visible_text }).to_string());
                }

                if !turn.text.trim().is_empty() {
                    spawn_stream_answer_completed_notification(
                        state.clone(),
                        claims.tenant_id.clone(),
                        turn.session_id.clone(),
                        session_source.clone(),
                        user_message.clone(),
                        turn.text.clone(),
                    );
                }

                if session_source.eq_ignore_ascii_case("chat") {
                    let state_for_memory = state.clone();
                    let tenant_for_memory = claims.tenant_id.clone();
                    let user_for_memory = claims.sub.clone();
                    let message_for_memory = original_user_message.clone();
                    let session_for_memory = turn.session_id.clone();
                    tokio::spawn(async move {
                        let memory_state =
                            crate::routes::memory_continuity::get_thread_memory_state(
                                &state_for_memory.db,
                                &tenant_for_memory,
                                &user_for_memory,
                                &session_for_memory,
                            )
                            .await;
                        if memory_state.generate_memories
                            && memory_state.pollution_state != "polluted"
                            && memory_state.pollution_state != "disabled"
                        {
                            crate::routes::chat_intelligence::persist_chat_memory_candidate(
                                &state_for_memory,
                                &tenant_for_memory,
                                &user_for_memory,
                                &message_for_memory,
                            )
                            .await;
                        }
                        crate::routes::memory_continuity::persist_unified_memory_candidate(
                            &state_for_memory.db,
                            &tenant_for_memory,
                            &user_for_memory,
                            Some(&session_for_memory),
                            "chat",
                            &message_for_memory,
                        )
                        .await;
                    });
                    chat_trace.push(json!({
                        "event": "chat.stream.finalized",
                        "streamMode": stream_mode,
                        "firstTokenLatencyMs": first_text_delta_ms,
                        "elapsedMs": elapsed_ms,
                        "textDeltaCount": text_delta_count,
                        "toolInterleaved": tool_event_seen,
                        "cancelled": false,
                    }));
                    if !chat_artifact_evidence.is_empty() {
                        for artifact in chat_artifact_evidence
                            .iter()
                            .filter_map(|item| item.get("metadata"))
                        {
                            crate::routes::memory_continuity::record_memory_citations(
                                &state.db,
                                &claims.tenant_id,
                                &claims.sub,
                                &turn.session_id,
                                None,
                                artifact,
                            )
                            .await;
                        }
                        crate::routes::chat_intelligence::persist_chat_artifact(
                            &state,
                            &claims.tenant_id,
                            &claims.sub,
                            &turn.session_id,
                            "evidence",
                            &json!({
                                "items": chat_artifact_evidence,
                                "searchMode": effective_search_mode.as_str(),
                            }),
                        )
                        .await;
                    }
                    crate::routes::chat_intelligence::persist_chat_artifact(
                        &state,
                        &claims.tenant_id,
                        &claims.sub,
                        &turn.session_id,
                        "trace",
                        &json!({
                            "items": chat_trace,
                            "streamMode": stream_mode,
                            "firstTokenLatencyMs": first_text_delta_ms,
                            "elapsedMs": elapsed_ms,
                        }),
                    )
                    .await;
                }

                yield axum::response::sse::Event::default()
                    .event("stream_end")
                    .data(serde_json::json!({
                        "session_id": turn.session_id,
                        "iterations": turn.iterations,
                        "streamMode": stream_mode,
                        "telemetry": {
                            "firstTokenLatencyMs": first_text_delta_ms,
                            "elapsedMs": elapsed_ms,
                            "fallbackTypewriter": stream_mode == "final_text_typewriter",
                            "textDeltaCount": text_delta_count,
                            "toolInterleaved": tool_event_seen,
                            "cancelled": false,
                            "searchMode": effective_search_mode.as_str(),
                        }
                    }).to_string());
            }
            Some(Ok(agent_gateway::AgentTurnRunOutcome::Suspended(turn))) => {
                let pending = crate::semantic_kernel_store::list_runtime_approvals(
                    &state.db,
                    &claims.tenant_id,
                    &claims.sub,
                    &turn.session_id,
                )
                .await
                .unwrap_or_default();
                let interactions = crate::semantic_kernel_store::list_runtime_interactions(
                    &state.db,
                    &claims.tenant_id,
                    &claims.sub,
                    &turn.session_id,
                )
                .await
                .unwrap_or_default();
                let questions = interactions
                    .iter()
                    .filter(|interaction| {
                        interaction.kind == agent_protocol::InteractionKind::UserQuestion
                    })
                    .map(|interaction| {
                        let question = interaction
                            .display_projection
                            .get("question")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let options = interaction
                            .display_projection
                            .get("options")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!([]));
                        serde_json::json!({
                            "requestId": interaction.interaction_id,
                            "turnId": interaction.scope.turn_id,
                            "invocationId": interaction.scope.invocation_id,
                            "question": question,
                            "options": options,
                            "status": interaction.state,
                            "expiresAt": interaction.expires_at,
                            "expired": interaction.expires_at.is_some_and(|expires_at| expires_at <= chrono::Utc::now()),
                            "idempotencyKey": interaction.idempotency_key,
                        })
                    })
                    .collect::<Vec<_>>();
                yield axum::response::sse::Event::default()
                    .event(if questions.is_empty() { "approval_paused" } else { "question_paused" })
                    .data(serde_json::json!({
                        "sessionId": turn.session_id,
                        "runtimeTurnId": turn.runtime_turn_id,
                        "approvals": pending,
                        "questions": questions,
                        "partialText": turn.partial_text,
                        "iterations": turn.iterations,
                    }).to_string());
            }
            Some(Err(e)) => {
                if matches!(e, agent_gateway::GatewayError::TurnCancelled) {
                    let elapsed_ms = stream_started_at.elapsed().as_millis();
                    if session_source.eq_ignore_ascii_case("chat") {
                        chat_trace.push(json!({
                            "event": "chat.stream.cancelled",
                            "elapsedMs": elapsed_ms,
                            "textDeltaCount": text_delta_count,
                            "toolInterleaved": tool_event_seen,
                        }));
                        crate::routes::chat_intelligence::persist_chat_artifact(
                            &state,
                            &claims.tenant_id,
                            &claims.sub,
                            &session_id_for_error,
                            "trace",
                            &json!({
                                "items": chat_trace,
                                "streamMode": "cancelled",
                                "elapsedMs": elapsed_ms,
                                "cancelled": true,
                            }),
                        )
                        .await;
                    }
                    yield axum::response::sse::Event::default()
                        .event("stream_end")
                        .data(serde_json::json!({
                            "session_id": session_id_for_error,
                            "iterations": 0,
                            "streamMode": "cancelled",
                            "telemetry": {
                                "firstTokenLatencyMs": first_text_delta_ms,
                                "elapsedMs": elapsed_ms,
                                "fallbackTypewriter": false,
                                "textDeltaCount": text_delta_count,
                                "toolInterleaved": tool_event_seen,
                                "cancelled": true,
                                "searchMode": effective_search_mode.as_str(),
                            }
                        }).to_string());
                } else {
                    tracing::error!(session_id = %session_id_for_error, "stream_session turn failed: {}", e);
                    yield axum::response::sse::Event::default()
                        .event("error")
                        .data(serde_json::json!({ "error": e.to_string() }).to_string());
                }
            }
            None => {
                tracing::warn!(session_id = %session_id_for_error, "turn result channel closed without result");
            }
        }
    };

    axum::response::Sse::new(sse_stream.map_err(|e: std::io::Error| {
        let boxed: axum::BoxError = e.into();
        boxed
    }))
    .keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(stream_session_sse_heartbeat_secs()))
            .text("keepalive"),
    )
    .into_response()
}
