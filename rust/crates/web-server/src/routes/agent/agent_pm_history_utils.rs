use super::*;

pub(super) fn push_history_message_dedup(messages: &mut Vec<MessageDto>, candidate: MessageDto) {
    if let Some(last) = messages.last_mut() {
        if last.role == candidate.role && last.content.trim() == candidate.content.trim() {
            if last.tool_calls.is_none() && candidate.tool_calls.is_some() {
                last.tool_calls = candidate.tool_calls;
            }
            if last.usage.is_none() && candidate.usage.is_some() {
                last.usage = candidate.usage;
            }
            if last.thinking.is_none() && candidate.thinking.is_some() {
                last.thinking = candidate.thinking;
            }
            if last.pm_task_id.is_none() && candidate.pm_task_id.is_some() {
                last.pm_task_id = candidate.pm_task_id;
            }
            if last.pm_task_status.is_none() && candidate.pm_task_status.is_some() {
                last.pm_task_status = candidate.pm_task_status;
            }
            return;
        }
    }
    messages.push(candidate);
}

/// Collapse model iterations that belong to one visible user turn.  The
/// runtime ledger intentionally records every assistant iteration for exact
/// recovery, but chat history should expose one assistant answer rather than
/// intermediate drafts.  The newest textual candidate wins while useful
/// metadata from an earlier iteration is retained.
pub(super) fn collapse_runtime_assistant_iterations(messages: &mut Vec<MessageDto>) {
    let mut current_user_seen = false;
    let mut assistant_index: Option<usize> = None;
    let mut collapsed = Vec::with_capacity(messages.len());
    for candidate in messages.drain(..) {
        if candidate.role == "user" {
            current_user_seen = true;
            assistant_index = None;
            collapsed.push(candidate);
            continue;
        }
        if candidate.role != "assistant" || !current_user_seen {
            collapsed.push(candidate);
            continue;
        }
        if let Some(index) = assistant_index {
            let existing = &mut collapsed[index];
            let candidate_has_text = !candidate.content.trim().is_empty();
            if candidate_has_text {
                let mut replacement = candidate;
                if replacement.tool_calls.is_none() {
                    replacement.tool_calls = existing.tool_calls.take();
                }
                if replacement.usage.is_none() {
                    replacement.usage = existing.usage.take();
                }
                if replacement.thinking.is_none() {
                    replacement.thinking = existing.thinking.take();
                }
                if replacement.pm_task_id.is_none() {
                    replacement.pm_task_id = existing.pm_task_id.take();
                }
                if replacement.pm_task_status.is_none() {
                    replacement.pm_task_status = existing.pm_task_status.take();
                }
                *existing = replacement;
            } else {
                if existing.tool_calls.is_none() {
                    existing.tool_calls = candidate.tool_calls;
                }
                if existing.usage.is_none() {
                    existing.usage = candidate.usage;
                }
                if existing.thinking.is_none() {
                    existing.thinking = candidate.thinking;
                }
                if existing.pm_task_id.is_none() {
                    existing.pm_task_id = candidate.pm_task_id;
                }
                if existing.pm_task_status.is_none() {
                    existing.pm_task_status = candidate.pm_task_status;
                }
            }
        } else {
            assistant_index = Some(collapsed.len());
            collapsed.push(candidate);
        }
    }
    *messages = collapsed;
}

const HISTORY_DEFAULT_LIMIT_TURNS: usize = 8;
const HISTORY_MAX_LIMIT_TURNS: usize = 30;
const HISTORY_DEFAULT_MAX_BYTES: usize = 256 * 1024;
const HISTORY_MIN_MAX_BYTES: usize = 32 * 1024;
const HISTORY_MAX_MAX_BYTES: usize = 2 * 1024 * 1024;

fn estimate_history_message_bytes(message: &MessageDto) -> usize {
    let mut bytes = message.role.len().saturating_add(message.content.len());
    bytes = bytes.saturating_add(
        message
            .thinking
            .as_ref()
            .map(|value| value.len())
            .unwrap_or(0),
    );
    if let Some(calls) = message.tool_calls.as_ref() {
        for call in calls {
            bytes = bytes
                .saturating_add(call.id.len())
                .saturating_add(call.name.len())
                .saturating_add(call.input.len());
            if let Some(result) = call.result.as_ref() {
                bytes = bytes
                    .saturating_add(result.tool_use_id.len())
                    .saturating_add(result.tool_name.len())
                    .saturating_add(result.output.len());
            }
        }
    }
    if let Some(result) = message.tool_result.as_ref() {
        bytes = bytes
            .saturating_add(result.tool_use_id.len())
            .saturating_add(result.tool_name.len())
            .saturating_add(result.output.len());
    }
    bytes.saturating_add(128)
}

pub(super) fn paginate_history_messages(
    messages: &[MessageDto],
    before_turn_cursor: Option<usize>,
    limit_turns: Option<usize>,
    max_bytes: Option<usize>,
) -> (Vec<MessageDto>, SessionHistoryPageDto) {
    let limit_turns = limit_turns
        .unwrap_or(HISTORY_DEFAULT_LIMIT_TURNS)
        .clamp(1, HISTORY_MAX_LIMIT_TURNS);
    let max_bytes = max_bytes
        .unwrap_or(HISTORY_DEFAULT_MAX_BYTES)
        .clamp(HISTORY_MIN_MAX_BYTES, HISTORY_MAX_MAX_BYTES);

    if messages.is_empty() {
        return (
            Vec::new(),
            SessionHistoryPageDto {
                before_turn_cursor: 0,
                next_before_turn_cursor: None,
                has_more: false,
                returned_turns: 0,
                total_turns: 0,
                limit_turns,
                max_bytes,
                approx_payload_bytes: 0,
            },
        );
    }

    let mut turn_starts: Vec<usize> = vec![0];
    for (idx, message) in messages.iter().enumerate().skip(1) {
        if message.role == "user" {
            turn_starts.push(idx);
        }
    }
    let mut turn_ranges: Vec<(usize, usize)> = Vec::with_capacity(turn_starts.len());
    for (idx, start) in turn_starts.iter().enumerate() {
        let end = if idx + 1 < turn_starts.len() {
            turn_starts[idx + 1]
        } else {
            messages.len()
        };
        turn_ranges.push((*start, end));
    }

    let total_turns = turn_ranges.len();
    let before_turn_cursor = before_turn_cursor.unwrap_or(total_turns).min(total_turns);
    if before_turn_cursor == 0 {
        return (
            Vec::new(),
            SessionHistoryPageDto {
                before_turn_cursor,
                next_before_turn_cursor: None,
                has_more: false,
                returned_turns: 0,
                total_turns,
                limit_turns,
                max_bytes,
                approx_payload_bytes: 0,
            },
        );
    }

    let mut selected_turn_indices_rev: Vec<usize> = Vec::new();
    let mut payload_bytes = 0usize;
    for turn_idx in (0..before_turn_cursor).rev() {
        let (start, end) = turn_ranges[turn_idx];
        let turn_bytes = messages[start..end]
            .iter()
            .map(estimate_history_message_bytes)
            .sum::<usize>();

        let reach_limit_turns = selected_turn_indices_rev.len() >= limit_turns;
        let exceed_max_bytes = payload_bytes.saturating_add(turn_bytes) > max_bytes;
        if !selected_turn_indices_rev.is_empty() && (reach_limit_turns || exceed_max_bytes) {
            break;
        }

        selected_turn_indices_rev.push(turn_idx);
        payload_bytes = payload_bytes.saturating_add(turn_bytes);
    }

    selected_turn_indices_rev.reverse();
    let selected_start_turn = selected_turn_indices_rev
        .first()
        .copied()
        .unwrap_or(before_turn_cursor.saturating_sub(1));
    let selected_end_turn = selected_turn_indices_rev
        .last()
        .map(|idx| idx.saturating_add(1))
        .unwrap_or(before_turn_cursor);

    let start_msg_idx = turn_ranges[selected_start_turn].0;
    let end_msg_idx = turn_ranges[selected_end_turn.saturating_sub(1)].1;
    let page_messages = messages[start_msg_idx..end_msg_idx].to_vec();
    let next_before_turn_cursor = if selected_start_turn > 0 {
        Some(selected_start_turn)
    } else {
        None
    };

    (
        page_messages,
        SessionHistoryPageDto {
            before_turn_cursor,
            next_before_turn_cursor,
            has_more: next_before_turn_cursor.is_some(),
            returned_turns: selected_turn_indices_rev.len(),
            total_turns,
            limit_turns,
            max_bytes,
            approx_payload_bytes: payload_bytes,
        },
    )
}

fn message_has_non_empty_tool_calls(message: &MessageDto) -> bool {
    message
        .tool_calls
        .as_ref()
        .map(|calls| !calls.is_empty())
        .unwrap_or(false)
}

fn message_has_visible_payload(message: &MessageDto) -> bool {
    !message.content.trim().is_empty()
        || message_has_non_empty_tool_calls(message)
        || message
            .thinking
            .as_ref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn pending_pm_assistant_score(message: &MessageDto) -> usize {
    let text_score = message.content.trim().chars().count().min(4000);
    let tool_score = message
        .tool_calls
        .as_ref()
        .map(|calls| calls.len() * 120)
        .unwrap_or(0);
    let thinking_score = message
        .thinking
        .as_ref()
        .map(|value| value.trim().chars().count().min(400))
        .unwrap_or(0);
    text_score + tool_score + thinking_score
}

pub(super) fn merge_pending_pm_assistant(
    pending_assistant: &mut Option<MessageDto>,
    candidate: MessageDto,
) {
    match pending_assistant {
        None => {
            *pending_assistant = Some(candidate);
        }
        Some(existing) => {
            let existing_has_text = !existing.content.trim().is_empty();
            let candidate_has_text = !candidate.content.trim().is_empty();
            if candidate_has_text {
                // Always prefer the latest textual assistant message in the same
                // PM orchestration question chain.
                *existing = candidate;
                return;
            }
            if !existing_has_text {
                if pending_pm_assistant_score(&candidate) >= pending_pm_assistant_score(existing) {
                    *existing = candidate;
                }
                return;
            }
            // Existing has text while candidate is tool-only or empty.
            // Keep existing text, but merge missing metadata when possible.
            if existing.tool_calls.is_none() && candidate.tool_calls.is_some() {
                existing.tool_calls = candidate.tool_calls;
            }
            if existing.usage.is_none() && candidate.usage.is_some() {
                existing.usage = candidate.usage;
            }
            if existing.thinking.is_none() && candidate.thinking.is_some() {
                existing.thinking = candidate.thinking;
            }
            if existing.pm_task_id.is_none() && candidate.pm_task_id.is_some() {
                existing.pm_task_id = candidate.pm_task_id;
            }
            if existing.pm_task_status.is_none() && candidate.pm_task_status.is_some() {
                existing.pm_task_status = candidate.pm_task_status;
            }
        }
    }
}

pub(super) fn flush_pending_pm_internal_history(
    messages: &mut Vec<MessageDto>,
    pending_question: &mut Option<String>,
    pending_assistant: &mut Option<MessageDto>,
) {
    if let Some(question) = pending_question.take() {
        let trimmed = question.trim();
        if !trimmed.is_empty() {
            push_history_message_dedup(
                messages,
                MessageDto {
                    role: "user".to_string(),
                    content: trimmed.to_string(),
                    tool_calls: None,
                    tool_result: None,
                    usage: None,
                    thinking: None,
                    pm_task_id: None,
                    pm_task_status: None,
                },
            );
        }
    }
    if let Some(assistant) = pending_assistant.take() {
        if !message_has_visible_payload(&assistant) {
            return;
        }
        push_history_message_dedup(messages, assistant);
    }
}
