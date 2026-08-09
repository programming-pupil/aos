use super::*;

fn build_pm_live_tool_summary(
    index: u32,
    tool_name: &str,
    source: &str,
    input_preview: &str,
    output_preview: &str,
    is_error: bool,
    duration_ms: Option<u64>,
) -> serde_json::Value {
    let by_name_error = if is_error {
        serde_json::json!({ tool_name: 1 })
    } else {
        serde_json::json!({})
    };
    serde_json::json!({
        "count": 1,
        "errorCount": if is_error { 1 } else { 0 },
        "byName": { tool_name: 1 },
        "byNameError": by_name_error,
        "samples": [{
            "idx": index,
            "tool": tool_name,
            "source": source,
            "isError": is_error,
            "durationMs": duration_ms,
            "input": truncate_for_log(input_preview, 220),
            "output": truncate_for_log(output_preview, 220),
        }],
    })
}

pub(super) type PmRetrieveTurnLiveResult = Result<
    (TurnResult, Vec<agent_gateway::ToolCallRecord>),
    (GatewayError, Vec<agent_gateway::ToolCallRecord>),
>;

#[derive(Debug, Clone)]
struct PmLiveToolCallState {
    name: String,
    input_preview: String,
    input_announced: bool,
    started_at: Instant,
}

fn infer_pm_live_tool_source(tool_name: &str) -> &'static str {
    let lower = tool_name.to_ascii_lowercase();
    if lower.contains("mcp") {
        "mcp"
    } else if lower.contains("skill") {
        "skill"
    } else {
        "builtin"
    }
}

fn truncate_pm_visible_text(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn summarize_pm_live_tool_output(output: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        if let Some(error) = value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(|value| truncate_pm_visible_text(value, 120))
            .filter(|value| !value.is_empty())
        {
            return format!("失败原因：{error}");
        }
        for key in ["results", "items", "rows"] {
            if let Some(items) = value.get(key).and_then(serde_json::Value::as_array) {
                if items.is_empty() {
                    return "未找到匹配结果".to_string();
                }
                let first_label = items.first().and_then(|item| {
                    ["title", "name", "url", "snippet"]
                        .iter()
                        .find_map(|key| item.get(key).and_then(serde_json::Value::as_str))
                });
                return first_label.map_or_else(
                    || format!("返回 {} 条结果", items.len()),
                    |label| {
                        format!(
                            "返回 {} 条结果 · {}",
                            items.len(),
                            truncate_pm_visible_text(label, 100)
                        )
                    },
                );
            }
        }
        let status_code = value.get("status_code").and_then(serde_json::Value::as_u64);
        if let Some(body) = value.get("body").and_then(serde_json::Value::as_str) {
            let body_chars = body.chars().count();
            if status_code.is_some_and(|status| status >= 400) {
                return format!("HTTP {}，未获取可用内容", status_code.unwrap_or(0));
            }
            return status_code.map_or_else(
                || format!("已读取页面内容（{body_chars} 字符）"),
                |status| format!("HTTP {status}，已读取页面内容（{body_chars} 字符）"),
            );
        }
        for key in ["answer", "text", "content", "message"] {
            if let Some(text) = value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(|value| truncate_pm_visible_text(value, 140))
                .filter(|value| !value.is_empty())
            {
                return text;
            }
        }
        if let Some(status) = status_code {
            return format!("HTTP {status}，查询已返回");
        }
    }
    let first = first_non_empty_line(output);
    if first.is_empty() {
        truncate_pm_visible_text(output, 140)
    } else {
        truncate_pm_visible_text(&first, 140)
    }
}

fn extract_pm_live_tool_target(preview_input: &str, raw_input: &str) -> String {
    let try_extract = |text: &str| -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            let keys = [
                "query", "url", "target", "keyword", "q", "website", "site", "domain",
            ];
            for key in keys {
                if let Some(v) = value.get(key).and_then(|x| x.as_str()) {
                    let out = v.trim();
                    if !out.is_empty() {
                        return Some(truncate_pm_visible_text(out, 140));
                    }
                }
            }
        }
        for url in extract_http_urls(text) {
            if !url.trim().is_empty() {
                return Some(truncate_pm_visible_text(&url, 140));
            }
        }
        None
    };

    try_extract(preview_input)
        .or_else(|| try_extract(raw_input))
        .unwrap_or_default()
}

fn announce_pm_live_tool_input(
    state: &mut PmLiveToolCallState,
    index: u32,
    input: &str,
    selected_variant: Option<&str>,
    selected_route: Option<&str>,
    selected_route_channel: Option<&str>,
) -> Option<serde_json::Value> {
    state.input_preview = truncate_for_log(input, 220);
    let target = extract_pm_live_tool_target(&state.input_preview, input);
    if state.input_announced || target.is_empty() {
        return None;
    }
    state.input_announced = true;
    let source = infer_pm_live_tool_source(&state.name);
    Some(serde_json::json!({
        "message": format!("开始查询：{} · {}", state.name, target),
        "selectedVariant": selected_variant,
        "selectedRoute": selected_route,
        "selectedRouteChannel": selected_route_channel,
        "liveToolEvent": {
            "phase": "start",
            "index": index,
            "tool": state.name,
            "source": source,
            "target": target,
        }
    }))
}

fn parse_pm_native_web_search_status(
    phase: &str,
    detail: Option<&str>,
) -> Option<serde_json::Value> {
    if phase != "native_web_search" {
        return None;
    }
    let parsed = detail
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let status = parsed
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let message = match status {
        "attempting" => "正在尝试模型原生联网搜索",
        "accepted" => "模型原生联网搜索已产出可用结果",
        "rejected" => "模型原生联网不受支持，已自动切换后备检索链路",
        "unusable" => "模型原生联网未产出可用证据，正在切换后备检索链路",
        "failed" => "模型原生联网失败，正在切换后备检索链路",
        "timeout" => "模型原生联网超时，正在切换后备检索链路",
        _ => "模型原生联网状态已更新",
    };
    Some(serde_json::json!({
        "message": message,
        "nativeWebSearch": parsed,
    }))
}

pub(super) type PmLiveStageCallback<'a> =
    dyn FnMut(&str, &str, usize, Option<serde_json::Value>) + Send + 'a;

pub(super) async fn run_pm_retrieve_turn_with_live_events(
    manager: Arc<AgentSessionManager>,
    session_id: &str,
    message: String,
    timeout_secs: u64,
    timeout_label: &str,
    attempt: usize,
    selected_variant: Option<&str>,
    selected_route: Option<&str>,
    selected_route_channel: Option<&str>,
    web_tool_result_budget: usize,
    on_stage: &mut PmLiveStageCallback<'_>,
) -> PmRetrieveTurnLiveResult {
    let parent_session_id = session_id.to_string();
    let mut scratch_session_id: Option<String> = None;
    let mut scratch_session_guard: Option<PmTransientSessionGuard> = None;
    let runtime_session_id = match manager.get_session(session_id).await {
        Some(handle) => {
            let model_hint = (!handle.model.trim().is_empty()).then_some(handle.model.as_str());
            match manager
                .create_session(
                    &handle.user_id,
                    &handle.tenant_id,
                    None,
                    model_hint,
                    PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
                    Some("pm"),
                    None,
                    None,
                )
                .await
            {
                Ok(scratch_session) => {
                    let id = scratch_session.session_id.clone();
                    scratch_session_id = Some(id.clone());
                    scratch_session_guard =
                        Some(PmTransientSessionGuard::new(manager.clone(), id.clone()));
                    id
                }
                Err(error) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %error,
                        "pm retrieve scratch session creation failed; falling back to caller session"
                    );
                    session_id.to_string()
                }
            }
        }
        None => session_id.to_string(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<agent_gateway::AgentEvent>(2048);
    let (result_tx, mut result_rx) =
        tokio::sync::mpsc::channel::<Result<TurnResult, GatewayError>>(1);
    on_stage(
        "retrieve",
        "running",
        attempt,
        Some(serde_json::json!({
            "message": "正在按统一联网链路检索",
            "selectedVariant": selected_variant,
            "selectedRoute": selected_route,
            "selectedRouteChannel": selected_route_channel,
            "scratchSession": scratch_session_id.is_some(),
            "nativeWebSearch": {
                "preferred": false,
                "status": "requested",
                "note": "Search Orchestrator 会先使用健康的 Search 扩展；没有配置或参考质量不够时再尝试模型原生 Responses web_search、MCP search/browser/fetch 和本地/RAG。"
            }
        })),
    );
    let manager_for_turn = manager.clone();
    let session_id_for_turn = runtime_session_id.clone();
    let timeout_label_owned = timeout_label.to_string();
    tokio::spawn(async move {
        let result = run_pm_turn_streaming_with_timeout_cleanup_and_options(
            manager_for_turn,
            session_id_for_turn,
            message,
            tx,
            timeout_secs,
            &timeout_label_owned,
            agent_gateway::AgentTurnOptions {
                prefer_native_web_search: true,
                suppress_native_web_search: false,
                reasoning_budget: agent_gateway::InternalReasoningBudget::Deep,
                blocked_tools: pm_blocked_non_search_research_tools(),
                web_tool_result_budget: Some(web_tool_result_budget.max(1)),
                ..Default::default()
            },
        )
        .await;
        let _ = result_tx.send(result).await;
    });

    let mut live_tools = HashMap::<u32, PmLiveToolCallState>::new();
    let mut observed_tool_calls = Vec::<agent_gateway::ToolCallRecord>::new();
    while let Some(event) = rx.recv().await {
        match event {
            agent_gateway::AgentEvent::ToolUseStart { index, name, .. } => {
                live_tools.insert(
                    index,
                    PmLiveToolCallState {
                        name: name.clone(),
                        input_preview: String::new(),
                        input_announced: false,
                        started_at: Instant::now(),
                    },
                );
                let source = infer_pm_live_tool_source(&name);
                let start_target = extract_pm_live_tool_target("", "");
                let detail = serde_json::json!({
                    "message": if start_target.is_empty() {
                        format!("正在调用工具：{}", name)
                    } else {
                        format!("正在调用工具：{} · {}", name, start_target)
                    },
                    "selectedVariant": selected_variant,
                    "selectedRoute": selected_route,
                    "selectedRouteChannel": selected_route_channel,
                    "liveToolEvent": {
                        "phase": "start",
                        "index": index,
                        "tool": name,
                        "source": source,
                        "target": start_target,
                    }
                });
                on_stage("retrieve", "running", attempt, Some(detail));
            }
            agent_gateway::AgentEvent::ToolUseInput { index, input } => {
                if let Some(row) = live_tools.get_mut(&index) {
                    if let Some(detail) = announce_pm_live_tool_input(
                        row,
                        index,
                        &input,
                        selected_variant,
                        selected_route,
                        selected_route_channel,
                    ) {
                        on_stage("retrieve", "running", attempt, Some(detail));
                    }
                }
            }
            agent_gateway::AgentEvent::ToolResult {
                index,
                tool_name,
                input,
                output,
                is_error,
            } => {
                // Treat only structured tool errors as failures; avoid false positives from output text.
                let call_is_error = is_error;
                let state = live_tools.remove(&index);
                let started_at = state
                    .as_ref()
                    .map(|row| row.started_at)
                    .unwrap_or_else(Instant::now);
                let input_preview = state
                    .as_ref()
                    .map(|row| row.input_preview.clone())
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| truncate_for_log(&input, 220));
                let tool_display_name = state
                    .as_ref()
                    .map(|row| row.name.clone())
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| tool_name.clone());
                let source = infer_pm_live_tool_source(&tool_display_name);
                let tool_target = extract_pm_live_tool_target(&input_preview, &input);
                let duration_ms = Some(
                    started_at
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                );
                let output_preview = summarize_pm_live_tool_output(&output);
                let message = if call_is_error {
                    format!("工具失败：{} · {}", tool_display_name, output_preview)
                } else {
                    format!("工具完成：{} · {}", tool_display_name, output_preview)
                };
                let detail = serde_json::json!({
                    "message": message,
                    "selectedVariant": selected_variant,
                    "selectedRoute": selected_route,
                    "selectedRouteChannel": selected_route_channel,
                    "liveToolEvent": {
                        "phase": if call_is_error { "error" } else { "result" },
                        "index": index,
                        "tool": tool_display_name,
                        "source": source,
                        "target": tool_target,
                        "durationMs": duration_ms,
                        "isError": call_is_error,
                    },
                    "toolSummary": build_pm_live_tool_summary(
                        index,
                        &tool_display_name,
                        source,
                        &input_preview,
                        &output_preview,
                        call_is_error,
                        duration_ms,
                    ),
                });
                on_stage("retrieve", "running", attempt, Some(detail));
                observed_tool_calls.push(agent_gateway::ToolCallRecord {
                    index,
                    tool_name: tool_display_name,
                    source: source.to_string(),
                    source_name: String::new(),
                    input,
                    output,
                    is_error: call_is_error,
                    duration_ms: duration_ms.unwrap_or(0),
                });
            }
            agent_gateway::AgentEvent::HookProgress { phase, detail } => {
                if let Some(mut native_detail) =
                    parse_pm_native_web_search_status(&phase, detail.as_deref())
                {
                    let native_status = native_detail
                        .get("nativeWebSearch")
                        .and_then(|value| value.get("status"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    if let Some(obj) = native_detail.as_object_mut() {
                        obj.insert(
                            "selectedVariant".to_string(),
                            serde_json::json!(selected_variant),
                        );
                        obj.insert(
                            "selectedRoute".to_string(),
                            serde_json::json!(selected_route),
                        );
                        obj.insert(
                            "selectedRouteChannel".to_string(),
                            serde_json::json!(selected_route_channel),
                        );
                    }
                    let native_stage_status = match native_status.as_str() {
                        "accepted" => "completed",
                        "rejected" | "unusable" | "failed" | "timeout" => "failed",
                        _ => "running",
                    };
                    on_stage(
                        "native_web_search",
                        native_stage_status,
                        attempt,
                        Some(native_detail.clone()),
                    );
                    // The retrieve stage represents the whole evidence path;
                    // it remains active while a configured provider, MCP, or
                    // local fallback takes over after native search fails.
                    on_stage("retrieve", "running", attempt, Some(native_detail));
                }
            }
            _ => {}
        }
    }

    let result = match result_rx.recv().await {
        Some(Ok(mut turn)) => {
            turn.session_id = parent_session_id;
            Ok((turn, observed_tool_calls))
        }
        Some(Err(error)) => Err((error, observed_tool_calls)),
        None => Err((
            GatewayError::RuntimeExecution("turn result channel closed without result".to_string()),
            observed_tool_calls,
        )),
    };
    if let Some(guard) = scratch_session_guard {
        guard.finish().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{announce_pm_live_tool_input, summarize_pm_live_tool_output, PmLiveToolCallState};
    use std::time::Instant;

    #[test]
    fn query_input_is_announced_once_with_a_bounded_target() {
        let mut state = PmLiveToolCallState {
            name: "WebSearch".to_string(),
            input_preview: String::new(),
            input_announced: false,
            started_at: Instant::now(),
        };
        let query = "最新 AI Agent 行业实践".repeat(20);
        let detail = announce_pm_live_tool_input(
            &mut state,
            7,
            &serde_json::json!({"query": query}).to_string(),
            Some("evidence"),
            Some("web"),
            Some("builtin"),
        )
        .expect("first complete query input should be announced");
        assert_eq!(detail["liveToolEvent"]["phase"], "start");
        assert_eq!(detail["liveToolEvent"]["index"], 7);
        assert!(detail["liveToolEvent"]["target"]
            .as_str()
            .is_some_and(|target| target.chars().count() <= 140));

        assert!(announce_pm_live_tool_input(
            &mut state,
            7,
            r#"{"query":"duplicate chunk"}"#,
            None,
            None,
            None,
        )
        .is_none());
    }

    #[test]
    fn tool_results_are_summarized_without_raw_json_or_html() {
        assert_eq!(
            summarize_pm_live_tool_output(
                r#"{"results":[{"title":"AOS Release Notes","url":"https://example.com"}]}"#,
            ),
            "返回 1 条结果 · AOS Release Notes"
        );
        assert_eq!(
            summarize_pm_live_tool_output(
                r#"{"status_code":200,"body":"<html><body>content</body></html>"}"#,
            ),
            "HTTP 200，已读取页面内容（33 字符）"
        );
        assert_eq!(
            summarize_pm_live_tool_output(r#"{"error":"connection timed out"}"#),
            "失败原因：connection timed out"
        );
    }
}
