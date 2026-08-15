//! Runtime task-event mapping and compact tool text summaries.

use super::*;

pub(in crate::routes::rd) fn rd_runtime_event_to_task_event(
    event: agent_gateway::AgentEvent,
) -> Option<(String, String, String, Value)> {
    match event {
        agent_gateway::AgentEvent::TurnStarted { runtime_turn_id } => Some((
            "runtime_turn".to_string(),
            "running".to_string(),
            "runtime turn 已创建".to_string(),
            json!({
                "event": "turn_started",
                "runtimeTurnId": runtime_turn_id,
            }),
        )),
        agent_gateway::AgentEvent::SessionActivated {
            mcp_servers,
            skills,
            permission_mode,
            model,
        } => Some((
            "runtime_config".to_string(),
            "completed".to_string(),
            "runtime 配置已激活".to_string(),
            json!({
                "event": "session_activated",
                "mcpServers": mcp_servers,
                "skills": skills,
                "permissionMode": permission_mode,
                "model": model,
            }),
        )),
        agent_gateway::AgentEvent::ConfigHotReload {
            mcp_servers,
            skills,
            permission_mode,
            model,
        } => Some((
            "runtime_config".to_string(),
            "completed".to_string(),
            "runtime 配置已热更新".to_string(),
            json!({
                "event": "config_hot_reload",
                "mcpServers": mcp_servers,
                "skills": skills,
                "permissionMode": permission_mode,
                "model": model,
            }),
        )),
        agent_gateway::AgentEvent::ToolUseStart { index, id, name } => Some((
            "runtime_tool".to_string(),
            "running".to_string(),
            format!("runtime 准备调用工具 {name}"),
            json!({
                "event": "tool_use_start",
                "index": index,
                "toolUseId": id,
                "toolName": name,
            }),
        )),
        agent_gateway::AgentEvent::ToolResult {
            index,
            tool_name,
            input,
            output,
            is_error,
        } => {
            let (source, source_name) = classify_rd_runtime_tool(&tool_name);
            let input_summary = summarize_rd_tool_text(&input, 900);
            let output_summary = summarize_rd_tool_text(&output, 1_200);
            let status = if is_error { "failed" } else { "completed" };
            let message = if is_error {
                format!("runtime 工具 {tool_name} 执行失败")
            } else {
                format!("runtime 工具 {tool_name} 执行完成")
            };
            Some((
                "runtime_tool".to_string(),
                status.to_string(),
                message,
                json!({
                    "event": "tool_result",
                    "toolCalls": [{
                        "index": index,
                        "toolName": tool_name,
                        "source": source,
                        "sourceName": source_name,
                        "isError": is_error,
                        "durationMs": 0,
                        "input": input_summary.text,
                        "output": output_summary.text,
                        "inputChars": input_summary.original_chars,
                        "outputChars": output_summary.original_chars,
                        "inputHash": input_summary.hash,
                        "outputHash": output_summary.hash,
                        "inputTruncated": input_summary.truncated,
                        "outputTruncated": output_summary.truncated,
                        "dedupedOutputLines": output_summary.deduped_lines,
                    }],
                }),
            ))
        }
        agent_gateway::AgentEvent::Usage {
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
        } => Some((
            "runtime_usage".to_string(),
            "completed".to_string(),
            "runtime token 用量已记录".to_string(),
            json!({
                "event": "usage",
                "inputTokens": input_tokens,
                "outputTokens": output_tokens,
                "cacheCreationTokens": cache_creation_tokens,
                "cacheReadTokens": cache_read_tokens,
                "totalTokens": input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens,
            }),
        )),
        agent_gateway::AgentEvent::Iteration { count } => Some((
            "runtime_iteration".to_string(),
            "running".to_string(),
            format!("runtime 第 {count} 轮工具循环"),
            json!({"event": "iteration", "count": count}),
        )),
        agent_gateway::AgentEvent::HookProgress { phase, detail } => Some((
            "runtime_hook".to_string(),
            "running".to_string(),
            detail
                .clone()
                .unwrap_or_else(|| format!("runtime hook: {phase}")),
            json!({"event": "hook_progress", "phase": phase, "detail": detail}),
        )),
        agent_gateway::AgentEvent::ApprovalRequired {
            turn_id,
            invocation_id,
            tool_name,
            current_mode,
            required_mode,
            reason,
        } => Some((
            "runtime_approval".to_string(),
            "waiting_approval".to_string(),
            format!("runtime 工具 {tool_name} 等待审批"),
            json!({
                "event": "approval_required",
                "turnId": turn_id,
                "invocationId": invocation_id,
                "toolName": tool_name,
                "currentMode": current_mode,
                "requiredMode": required_mode,
                "reason": reason,
            }),
        )),
        agent_gateway::AgentEvent::SessionCompacted {
            summary,
            removed_messages,
        } => Some((
            "runtime_compaction".to_string(),
            "completed".to_string(),
            "runtime 上下文已自动压缩".to_string(),
            json!({
                "event": "session_compacted",
                "summary": truncate_text(&summary, 1_500),
                "removedMessages": removed_messages,
            }),
        )),
        agent_gateway::AgentEvent::Error { message } => Some((
            "runtime".to_string(),
            "failed".to_string(),
            "runtime 流式事件报错".to_string(),
            json!({"event": "error", "message": message}),
        )),
        agent_gateway::AgentEvent::StreamStart { session_id } => Some((
            "runtime_stream".to_string(),
            "running".to_string(),
            "runtime 流式执行已开始".to_string(),
            json!({"event": "stream_start", "sessionId": session_id}),
        )),
        agent_gateway::AgentEvent::StreamEnd {
            session_id,
            stop_reason,
        } => Some((
            "runtime_stream".to_string(),
            "completed".to_string(),
            "runtime 流式执行已结束".to_string(),
            json!({"event": "stream_end", "sessionId": session_id, "stopReason": stop_reason}),
        )),
        agent_gateway::AgentEvent::TextDelta { .. }
        | agent_gateway::AgentEvent::ThinkingStart { .. }
        | agent_gateway::AgentEvent::ThinkingDelta { .. }
        | agent_gateway::AgentEvent::ThinkingEnd { .. }
        | agent_gateway::AgentEvent::TextBlockStart { .. }
        | agent_gateway::AgentEvent::TextBlockEnd { .. }
        | agent_gateway::AgentEvent::ToolUseInput { .. }
        | agent_gateway::AgentEvent::ToolUseEnd { .. }
        | agent_gateway::AgentEvent::Ping { .. } => None,
    }
}

pub(super) fn classify_rd_runtime_tool(tool_name: &str) -> (String, String) {
    if let Some(rest) = tool_name.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or_default().to_string();
        ("mcp".to_string(), server)
    } else if let Some(rest) = tool_name.strip_prefix("skill__") {
        let skill = rest.strip_suffix("__invoke").unwrap_or(rest).to_string();
        ("skill".to_string(), skill)
    } else {
        ("builtin".to_string(), String::new())
    }
}

pub(in crate::routes::rd) struct RdToolTextSummary {
    pub(in crate::routes::rd) text: String,
    pub(in crate::routes::rd) original_chars: usize,
    pub(in crate::routes::rd) truncated: bool,
    pub(in crate::routes::rd) deduped_lines: usize,
    pub(in crate::routes::rd) hash: String,
}

pub(in crate::routes::rd) fn summarize_rd_tool_text(
    text: &str,
    max_chars: usize,
) -> RdToolTextSummary {
    let original_chars = text.chars().count();
    let hash = stable_hash_hex(text);
    let mut seen = BTreeSet::new();
    let mut deduped_lines = 0usize;
    let mut lines = Vec::new();
    for line in text.lines() {
        let key = line.trim();
        if key.len() > 24 && !seen.insert(key.to_string()) {
            deduped_lines += 1;
            continue;
        }
        lines.push(line);
    }
    let compacted = lines.join("\n");
    let compacted_chars = compacted.chars().count();
    let text = if compacted_chars <= max_chars {
        compacted
    } else {
        let head_chars = max_chars.saturating_mul(2) / 3;
        let tail_chars = max_chars.saturating_sub(head_chars).saturating_sub(96);
        let head = compacted.chars().take(head_chars).collect::<String>();
        let tail = compacted
            .chars()
            .rev()
            .take(tail_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        format!(
            "{head}\n\n[tool output truncated: original_chars={original_chars}, compacted_chars={compacted_chars}, omitted_middle=true]\n\n{tail}"
        )
    };
    RdToolTextSummary {
        text,
        original_chars,
        truncated: compacted_chars > max_chars,
        deduped_lines,
        hash,
    }
}

#[cfg(test)]
mod tests {
    use super::rd_runtime_event_to_task_event;

    #[test]
    fn turn_started_maps_runtime_turn_id_for_recovery_timeline() {
        let (stage, status, message, detail) =
            rd_runtime_event_to_task_event(agent_gateway::AgentEvent::TurnStarted {
                runtime_turn_id: "runtime-turn-42".to_string(),
            })
            .expect("turn_started should be persisted");

        assert_eq!(stage, "runtime_turn");
        assert_eq!(status, "running");
        assert_eq!(message, "runtime turn 已创建");
        assert_eq!(detail["event"], "turn_started");
        assert_eq!(detail["runtimeTurnId"], "runtime-turn-42");
    }
}
