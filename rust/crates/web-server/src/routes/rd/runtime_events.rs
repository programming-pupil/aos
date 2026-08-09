//! Runtime event persistence pipeline.

use super::*;

mod attribution;
mod live_governance;
mod mapping;

use self::attribution::load_rd_runtime_attribution_context;
use self::live_governance::RdRuntimeLiveGovernance;
#[cfg(test)]
pub(super) use mapping::rd_runtime_event_to_task_event;
pub(super) use mapping::summarize_rd_tool_text;

#[allow(clippy::too_many_arguments)]
pub(super) fn rd_runtime_soft_feedback_json(
    tool_name: &str,
    repeated_target: bool,
    repeated_input: bool,
    is_error: bool,
    failed_target_count: usize,
    failed_input_count: usize,
    output_signal: Option<&'static str>,
    unattributed_read: bool,
    over_read: bool,
    over_search: bool,
    deep_mode_recommended: bool,
    recommendations: &[String],
) -> Value {
    let mut signals = Vec::new();
    if repeated_target {
        signals.push(json!({
            "code": "repeated_target",
            "level": "watch",
            "message": "同一目标被重复访问，优先复用已有证据或说明复核原因。",
        }));
    }
    if repeated_input {
        signals.push(json!({
            "code": "repeated_input",
            "level": "watch",
            "message": "同一工具输入重复出现，建议调整参数而不是原样重试。",
        }));
    }
    if is_error {
        signals.push(json!({
            "code": "tool_failed",
            "level": "correction_needed",
            "message": "工具调用失败，下一步应根据失败原因调整路径、参数或搜索词。",
        }));
    }
    if failed_target_count > 1 || failed_input_count > 1 {
        signals.push(json!({
            "code": "failed_retry",
            "level": "correction_needed",
            "message": "同一失败目标/输入被重复尝试，建议切换定位策略。",
        }));
    }
    if output_signal == Some("empty_result") {
        signals.push(json!({
            "code": "empty_result",
            "level": "watch",
            "message": "工具结果为空，建议扩大到相邻目录或改用符号/错误关键词。",
        }));
    }
    if unattributed_read {
        signals.push(json!({
            "code": "unattributed_read",
            "level": "watch",
            "message": "读取目标未命中结构化召回证据；如继续扩展，应说明证据缺口。",
        }));
    }
    if over_read || over_search {
        signals.push(json!({
            "code": "expanded_scope",
            "level": if deep_mode_recommended { "deep_mode_suggested" } else { "expanded" },
            "message": "工具读取/搜索已超过当前计划软阈值，建议先收敛候选范围。",
        }));
    }
    if deep_mode_recommended {
        signals.push(json!({
            "code": "deep_mode_recommended",
            "level": "deep_mode_suggested",
            "message": "如果仍需继续扩大范围，建议让用户确认深度模式。",
        }));
    }

    let level = if deep_mode_recommended {
        "deep_mode_suggested"
    } else if is_error || failed_target_count > 1 || failed_input_count > 1 {
        "correction_needed"
    } else if over_read || over_search {
        "expanded"
    } else if repeated_target
        || repeated_input
        || output_signal == Some("empty_result")
        || unattributed_read
    {
        "watch"
    } else {
        "ok"
    };
    let next_action = match level {
        "deep_mode_suggested" => "先总结当前已确认/未确认的证据缺口，再建议用户用深度模式继续。",
        "correction_needed" => "不要原样重试失败输入；先调整路径、搜索词或回到召回候选重新定位。",
        "expanded" => "先收敛候选模块，复用已读证据；如证据仍不足，再说明原因后继续扩大。",
        "watch" => "注意复用已有工具结果，避免无依据扩张；必要时说明为什么继续读取。",
        _ => "当前工具节奏正常，继续按召回证据渐进读取和核对真实文件。",
    };

    json!({
        "level": level,
        "toolName": tool_name,
        "signals": signals,
        "nextAction": next_action,
        "agentGuidance": next_action,
        "blocking": false,
        "effectFirst": true,
        "recommendations": recommendations.iter().take(5).cloned().collect::<Vec<_>>(),
    })
}

pub(super) fn rd_runtime_tool_output_signal(
    tool_name: &str,
    output: &str,
    is_error: bool,
) -> Option<&'static str> {
    if is_error {
        return Some("error");
    }
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("empty_result");
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(tool_name, "grep_search" | "glob_search")
        && [
            "no matches",
            "no results",
            "no files found",
            "0 matches",
            "0 results",
            "not found",
            "未找到",
            "无匹配",
            "没有匹配",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return Some("empty_result");
    }
    None
}
pub(super) async fn persist_rd_runtime_events(
    db: SqlitePool,
    tenant_id: String,
    task_id: String,
    mut receiver: mpsc::Receiver<agent_gateway::AgentEvent>,
    governance_plan: Option<RdRuntimeToolGovernancePlan>,
) {
    let mut pending = Vec::with_capacity(16);
    let mut seen_tool_results = HashSet::new();
    let attribution = load_rd_runtime_attribution_context(&db, &tenant_id, &task_id).await;
    let mut live_governance = RdRuntimeLiveGovernance::new(governance_plan, attribution);
    let mut flush_interval = tokio::time::interval(Duration::from_millis(750));
    loop {
        tokio::select! {
            event = receiver.recv() => {
                let Some(event) = event else {
                    break;
                };
        let Some((stage, status, message, detail)) = live_governance.event_to_task_event(event) else {
            continue;
        };
                if let Some(key) = rd_runtime_tool_result_dedupe_key(&detail) {
                    if !seen_tool_results.insert(key) {
                        continue;
                    }
                }
                pending.push((stage, status, message, detail));
                if pending.len() >= 10 {
                    flush_rd_runtime_event_batch(&db, &tenant_id, &task_id, &mut pending).await;
                }
            }
            _ = flush_interval.tick() => {
                flush_rd_runtime_event_batch(&db, &tenant_id, &task_id, &mut pending).await;
            }
        }
    }
    flush_rd_runtime_event_batch(&db, &tenant_id, &task_id, &mut pending).await;
}

async fn flush_rd_runtime_event_batch(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    pending: &mut Vec<(String, String, String, Value)>,
) {
    if pending.is_empty() {
        return;
    }
    let batch = std::mem::take(pending);
    let mut tx = match db.begin().await {
        Ok(tx) => tx,
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                task_id = %task_id,
                "failed to begin RD runtime event batch transaction: {}",
                error
            );
            return;
        }
    };
    for (stage, status, message, detail) in batch {
        if let Err(error) = sqlx::query("INSERT INTO rd_task_events (task_id, tenant_id, stage, status, message, detail_json) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(task_id)
            .bind(tenant_id)
            .bind(stage)
            .bind(status)
            .bind(message)
            .bind(detail)
            .execute(&mut *tx)
            .await
        {
            tracing::warn!(
                tenant_id = %tenant_id,
                task_id = %task_id,
                "failed to persist RD runtime event batch item: {}",
                error
            );
        }
    }
    if let Err(error) = tx.commit().await {
        tracing::warn!(
            tenant_id = %tenant_id,
            task_id = %task_id,
            "failed to commit RD runtime event batch: {}",
            error
        );
    }
}

fn rd_runtime_tool_result_dedupe_key(detail: &Value) -> Option<String> {
    if detail.get("event").and_then(Value::as_str) != Some("tool_result") {
        return None;
    }
    let call = detail
        .get("toolCalls")
        .and_then(Value::as_array)
        .and_then(|items| items.first())?;
    Some(format!(
        "{}:{}:{}",
        call.get("toolName")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        call.get("inputHash")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        call.get("outputHash")
            .and_then(Value::as_str)
            .unwrap_or_default()
    ))
}
