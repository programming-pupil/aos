use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionMemoryModePatchRequest {
    use_memories: Option<bool>,
    generate_memories: Option<bool>,
    pollution_state: Option<String>,
    pollution_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextStatusResponse {
    session_id: String,
    model: String,
    provider: String,
    message_count: usize,
    estimated_tokens: usize,
    token_estimator: String,
    context_window: usize,
    effective_context_limit: usize,
    auto_compact_token_limit: usize,
    context_usage_percent: f64,
    tokens_until_compaction: isize,
    should_compact: bool,
    unknown_context_window: bool,
    compaction_count: u32,
    last_compaction_summary: Option<String>,
    last_compaction_removed_messages: usize,
    state: String,
    memory_state: crate::routes::memory_continuity::ThreadMemoryState,
}

pub(super) async fn get_session_context_status(
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
    let status = match get_agent_manager(&state).context_status(&session_id).await {
        Ok(value) => value,
        Err(error) => return context_gateway_error_to_response(&error),
    };
    let memory_state = crate::routes::memory_continuity::get_thread_memory_state(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &session_id,
    )
    .await;
    Json(ContextStatusResponse {
        should_compact: status.tokens_until_compaction <= 0,
        session_id: status.session_id,
        model: status.model,
        provider: status.provider,
        message_count: status.message_count,
        estimated_tokens: status.estimated_tokens,
        token_estimator: status.token_estimator,
        context_window: status.context_window,
        effective_context_limit: status.effective_context_limit,
        auto_compact_token_limit: status.auto_compact_token_limit,
        context_usage_percent: status.context_usage_percent,
        tokens_until_compaction: status.tokens_until_compaction,
        unknown_context_window: status.unknown_context_window,
        compaction_count: status.compaction_count,
        last_compaction_summary: status.last_compaction_summary,
        last_compaction_removed_messages: status.last_compaction_removed_messages,
        state: status.state.to_string(),
        memory_state,
    })
    .into_response()
}

pub(super) async fn compact_session_context(
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
    let result = match get_agent_manager(&state)
        .compact_session_manual(&session_id)
        .await
    {
        Ok(value) => value,
        Err(error) => return context_gateway_error_to_response(&error),
    };
    let metadata = serde_json::json!({
        "messageCountAfter": result.message_count_after,
        "memoryRefsPolicy": "refs_only",
        "providerNativeCompactionSupported": result.provider_native_supported,
        "providerNativeCompactionUsed": result.provider_native_used,
        "event": "context.compaction.completed",
    });
    let window_id = get_agent_manager(&state)
        .config_registry()
        .record_session_compaction(agent_gateway::SessionCompactionParams {
            tenant_id: claims.tenant_id.clone(),
            user_id: claims.sub.clone(),
            session_id: session_id.clone(),
            trigger: result.trigger.clone(),
            strategy: result.strategy.clone(),
            summary_tokens: result.summary_tokens as i64,
            removed_message_count: result.removed_message_count as i64,
            retained_tail_tokens: result.retained_tail_tokens as i64,
            used_memory_refs_json: Some(serde_json::json!([])),
            metadata_json: Some(metadata),
        })
        .await
        .unwrap_or_else(|_| format!("ctx-{}", uuid::Uuid::new_v4()));

    // --- Zero_Loss_Target measurement (task 18.1 production wiring) ----------
    // The probe-collection harness (`super_assistant::collect_zero_loss_measurement`)
    // is otherwise unreachable: the runtime auto-compaction hook only holds a DB
    // pool and cannot run `build_injection_bundle` (which needs `AppState`). This
    // manual-compaction endpoint is the reachable path that owns an `AppState`,
    // so after compaction commits we replay a follow-up probe for every
    // session-scoped key fact and record the measured recall rate — turning
    // "0 丢失达标" into a real, observable metric instead of an unmeasured claim
    // (Req 4.5 / 4.6). Best-effort throughout: a memory/retrieval failure yields
    // a lower/zero recall rather than failing the compaction response (Req 4.10).
    // Opt-in via `AOS_ZERO_LOSS_PROBE`: probe collection replays a retrieval per
    // established fact, so it is gated off by default to avoid imposing that
    // per-compaction cost unconditionally (Req 4.5 / 4.10).
    let zero_loss = if crate::routes::super_assistant::zero_loss_probe_enabled() {
        use crate::routes::super_assistant::{
            collect_zero_loss_measurement, record_zero_loss_measurement, ExtractedKeyInfo,
            KeyInfoKind, SessionCompactedEvent, SessionCompactedEventTag,
        };
        // Read/probe the SAME Unified_Memory bucket the compaction hook wrote to,
        // resolved from the session source so there is no bucket drift.
        let app = agent_gateway::memory_app_for_session_source(&handle.source, None);
        let established: Vec<ExtractedKeyInfo> =
            crate::routes::memory_continuity::list_unified_memory_items(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                Some("session"),
                Some(&app),
                Some(&session_id),
                false,
                64,
            )
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|item| ExtractedKeyInfo {
                kind: KeyInfoKind::Fact,
                content: item.content,
                source_turn_id: None,
                confidence: item.confidence,
                pinned: item.pinned,
            })
            .collect();
        let event = SessionCompactedEvent {
            event_type: SessionCompactedEventTag::SessionCompacted,
            removed_messages: result.removed_message_count,
            summary: result.summary.clone(),
            summary_tokens: Some(result.summary_tokens),
            retained_tail_tokens: Some(result.retained_tail_tokens),
        };
        let measurement = collect_zero_loss_measurement(
            &state,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            &app,
            &established,
            &event,
            None,
        )
        .await;
        // Persist into the session trace (grouped with this compaction window) so
        // the recall rate and pass/fail verdict are durably auditable.
        record_zero_loss_measurement(
            &state,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            &window_id,
            &measurement,
        )
        .await;
        Some(measurement)
    } else {
        None
    };

    Json(serde_json::json!({
        "sessionId": result.session_id,
        "windowId": window_id,
        "trigger": result.trigger,
        "strategy": result.strategy,
        "summary": result.summary,
        "summaryTokens": result.summary_tokens,
        "removedMessageCount": result.removed_message_count,
        "retainedTailTokens": result.retained_tail_tokens,
        "messageCountAfter": result.message_count_after,
        "providerNativeCompactionSupported": result.provider_native_supported,
        "providerNativeCompactionUsed": result.provider_native_used,
        "usedMemoryRefs": [],
        "zeroLoss": zero_loss,
    }))
    .into_response()
}

pub(super) async fn list_session_compactions(
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
    let rows = match sqlx::query(
        r#"
        SELECT window_id, previous_window_id, trigger, strategy, summary_tokens,
               removed_message_count, retained_tail_tokens,
               CAST(used_memory_refs_json AS TEXT) AS used_memory_refs_json,
               CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_session_compactions
        WHERE tenant_id = ? AND user_id = ? AND session_id = ?
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            return AppError::Internal(format!("failed to load compactions: {error}"))
                .into_response()
        }
    };
    let items = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "windowId": row.get::<String, _>("window_id"),
                "previousWindowId": row.get::<Option<String>, _>("previous_window_id"),
                "trigger": row.get::<String, _>("trigger"),
                "strategy": row.get::<String, _>("strategy"),
                "summaryTokens": row.get::<i32, _>("summary_tokens"),
                "removedMessageCount": row.get::<i32, _>("removed_message_count"),
                "retainedTailTokens": row.get::<i32, _>("retained_tail_tokens"),
                "usedMemoryRefs": row.get::<Option<String>, _>("used_memory_refs_json")
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                    .unwrap_or_else(|| serde_json::json!([])),
                "metadata": row.get::<Option<String>, _>("metadata_json")
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "sessionId": session_id, "items": items })).into_response()
}

pub(super) async fn patch_session_memory_mode(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    Json(req): Json<SessionMemoryModePatchRequest>,
) -> impl IntoResponse {
    let Some(handle) = get_agent_manager(&state).get_session(&session_id).await else {
        return AppError::NotFound(format!("session {session_id} not found")).into_response();
    };
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return AppError::Forbidden.into_response();
    }
    match crate::routes::memory_continuity::set_thread_memory_mode(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &session_id,
        req.use_memories,
        req.generate_memories,
        req.pollution_state.as_deref(),
        req.pollution_reason.as_deref(),
    )
    .await
    {
        Ok(state) => Json(state).into_response(),
        Err(error) => error.into_response(),
    }
}

pub(super) async fn list_session_memory_citations(
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
    let rows = match sqlx::query(
        r#"
        SELECT id, turn_id, memory_id, path, line_start, line_end, note,
               CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_memory_citations
        WHERE tenant_id = ? AND user_id = ? AND session_id = ?
        ORDER BY created_at DESC
        LIMIT 100
        "#,
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            return AppError::Internal(format!("failed to load memory citations: {error}"))
                .into_response()
        }
    };
    let items = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.get::<String, _>("id"),
                "turnId": row.get::<Option<String>, _>("turn_id"),
                "memoryId": row.get::<String, _>("memory_id"),
                "path": row.get::<String, _>("path"),
                "lineStart": row.get::<Option<i32>, _>("line_start"),
                "lineEnd": row.get::<Option<i32>, _>("line_end"),
                "note": row.get::<Option<String>, _>("note"),
                "metadata": row.get::<Option<String>, _>("metadata_json")
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "sessionId": session_id, "items": items })).into_response()
}

fn context_gateway_error_to_response(error: &GatewayError) -> axum::response::Response {
    let status = error.http_status();
    (
        status,
        Json(serde_json::json!({
            "error": error.to_string(),
            "code": status.as_u16(),
        })),
    )
        .into_response()
}
