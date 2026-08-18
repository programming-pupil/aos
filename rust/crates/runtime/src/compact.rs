use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};
use crate::token_estimator::estimate_message_tokens;

const COMPACT_CONTINUATION_PREAMBLE: &str =
    "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n";
const COMPACT_RECENT_MESSAGES_NOTE: &str = "Recent messages are preserved verbatim.";
const COMPACT_DIRECT_RESUME_INSTRUCTION: &str = "Continue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, and do not preface with continuation text.";

/// Case-insensitive markers that flag a message as pinned. Mirrors the
/// orchestration-side `PIN_MARKERS` (web-server `super_assistant`) so the
/// compaction kernel enforces pinned retention independently. This forms a
/// double guarantee with the orchestration layer: even when compaction is
/// driven directly through this crate (no `super_assistant` orchestration in
/// the call path), pinned content is still excluded from the discardable set
/// and can never be summarized away (Req 4.9).
const PIN_MARKERS: &[&str] = &[
    "[pin]",
    "[pinned]",
    "#pin",
    "pin:",
    "📌",
    "务必记住",
    "请记住",
    "牢记",
    "重点记住",
];

/// Header introducing the verbatim pinned-content block appended to a
/// compaction summary. Pinned message text is listed verbatim under this header
/// so it is excluded from the discardable set during summary generation and can
/// never be summarized away (Req 4.9). Kept as a stable literal so downstream
/// readers can locate retained pins deterministically.
pub const PINNED_RETENTION_HEADER: &str =
    "Pinned key information (retained verbatim across compaction):";

/// Returns `true` when any text block in the message carries a pin marker.
fn message_is_pinned(message: &ConversationMessage) -> bool {
    message.blocks.iter().any(|block| match block {
        ContentBlock::Text { text } => contains_pin_marker(text),
        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => false,
    })
}

/// Case-insensitive pin-marker detection over raw text.
fn contains_pin_marker(text: &str) -> bool {
    let lowered = text.to_lowercase();
    PIN_MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Collects the verbatim text of every pinned message in `messages`. The full
/// (untruncated) text is preserved so pinned content survives compaction
/// byte-for-byte, unlike the timeline entries which are summarized/truncated.
fn collect_pinned_verbatim(messages: &[ConversationMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message_is_pinned(message))
        .filter_map(|message| {
            let text = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.trim()),
                    ContentBlock::Text { .. }
                    | ContentBlock::ToolUse { .. }
                    | ContentBlock::ToolResult { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        })
        .collect()
}

/// Appends pinned message contents verbatim to `summary` under
/// [`PINNED_RETENTION_HEADER`], excluding them from the discardable set (Req
/// 4.9). Idempotent: content already present in the summary (e.g. injected by
/// the orchestration-side pinned protection when a model summary is supplied to
/// [`compact_session_with_summary`]) is not duplicated. With no pinned messages
/// the summary is returned unchanged.
fn augment_summary_with_pinned_messages(summary: &str, messages: &[ConversationMessage]) -> String {
    let pinned = collect_pinned_verbatim(messages);
    if pinned.is_empty() {
        return summary.to_string();
    }

    let mut out = summary.to_string();
    let mut header_present = out.contains(PINNED_RETENTION_HEADER);
    for content in pinned {
        // Skip content already retained verbatim to keep the double guarantee
        // idempotent (no duplicated pins).
        if out.contains(&content) {
            continue;
        }
        if !header_present {
            if !out.trim_end().is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(PINNED_RETENTION_HEADER);
            header_present = true;
        }
        out.push_str("\n- ");
        out.push_str(&content);
    }
    out
}

/// Thresholds controlling when and how a session is compacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionConfig {
    pub preserve_recent_messages: usize,
    pub max_estimated_tokens: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            preserve_recent_messages: 4,
            max_estimated_tokens: 10_000,
        }
    }
}

/// Result of compacting a session into a summary plus preserved tail messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub summary: String,
    pub formatted_summary: String,
    pub compacted_session: Session,
    pub removed_message_count: usize,
    pub archived_messages: Vec<ConversationMessage>,
}

/// Returns `true` when the session exceeds the configured compaction budget.
#[must_use]
pub fn should_compact(session: &Session, config: CompactionConfig) -> bool {
    let start = compacted_summary_prefix_len(session);
    let compactable = &session.messages[start..];

    compactable.len() > config.preserve_recent_messages
        && compactable
            .iter()
            .map(estimate_message_tokens)
            .sum::<usize>()
            >= config.max_estimated_tokens
}

/// Normalizes a compaction summary into user-facing continuation text.
#[must_use]
pub fn format_compact_summary(summary: &str) -> String {
    let without_analysis = strip_tag_block(summary, "analysis");
    let formatted = if let Some(content) = extract_tag_block(&without_analysis, "summary") {
        without_analysis.replace(
            &format!("<summary>{content}</summary>"),
            &format!("Summary:\n{}", content.trim()),
        )
    } else {
        without_analysis
    };

    collapse_blank_lines(&formatted).trim().to_string()
}

/// Builds the synthetic system message used after session compaction.
#[must_use]
pub fn get_compact_continuation_message(
    summary: &str,
    suppress_follow_up_questions: bool,
    recent_messages_preserved: bool,
) -> String {
    let mut base = format!(
        "{COMPACT_CONTINUATION_PREAMBLE}{}",
        format_compact_summary(summary)
    );

    if recent_messages_preserved {
        base.push_str("\n\n");
        base.push_str(COMPACT_RECENT_MESSAGES_NOTE);
    }

    if suppress_follow_up_questions {
        base.push('\n');
        base.push_str(COMPACT_DIRECT_RESUME_INSTRUCTION);
    }

    base
}

/// Compacts a session by summarizing older messages and preserving the recent tail.
#[must_use]
pub fn compact_session(session: &Session, config: CompactionConfig) -> CompactionResult {
    if !should_compact(session, config) {
        return CompactionResult {
            summary: String::new(),
            formatted_summary: String::new(),
            compacted_session: session.clone(),
            removed_message_count: 0,
            archived_messages: Vec::new(),
        };
    }

    let existing_summary = existing_compacted_summary(session);
    let (archive_start, keep_from) =
        compaction_message_window(session, config).unwrap_or((0, session.messages.len()));
    // An existing continuation is a durable replacement artifact. When it is
    // replaced by a newer summary it must remain in the exact archive so the
    // persistence layer can record a parent compaction edge. It is excluded
    // from the new summary input below to avoid recursively expanding text.
    let summary_start = usize::from(existing_summary.is_some());
    let summary_messages = &session.messages[summary_start..keep_from];
    let archived_messages = session.messages[archive_start..keep_from].to_vec();
    let preserved = session.messages[keep_from..].to_vec();
    let summary = merge_compact_summaries(
        existing_summary.as_deref(),
        &summarize_messages(summary_messages),
    );
    // Kernel-side pinned protection (Req 4.9): exclude pinned messages from the
    // discardable set by retaining their content verbatim in the final summary,
    // regardless of whether an orchestration layer is present in the call path.
    let summary = augment_summary_with_pinned_messages(&summary, &archived_messages);
    let formatted_summary = format_compact_summary(&summary);
    let continuation = get_compact_continuation_message(&summary, true, !preserved.is_empty());

    let mut compacted_messages = vec![ConversationMessage {
        role: MessageRole::System,
        blocks: vec![ContentBlock::Text { text: continuation }],
        thinking: None,
        thinking_signature: None,
        usage: None,
    }];
    compacted_messages.extend(preserved);

    let mut compacted_session = session.clone();
    compacted_session.messages = compacted_messages;
    compacted_session.record_compaction_checkpoint_with_removed_start(
        summary.clone(),
        archive_start,
        archived_messages.len(),
        compacted_session.messages.clone(),
        archived_messages.clone(),
    );

    CompactionResult {
        summary,
        formatted_summary,
        compacted_session,
        removed_message_count: archived_messages.len(),
        archived_messages,
    }
}

/// Rebuilds a compacted session with a caller-provided summary while preserving
/// the exact tail selected by the existing compaction logic. This lets higher
/// layers use a model-generated summary without duplicating boundary handling
/// for tool-use / tool-result pairs.
#[must_use]
pub fn compact_session_with_summary(
    session: &Session,
    config: CompactionConfig,
    summary: impl Into<String>,
) -> CompactionResult {
    let baseline = compact_session(session, config);
    if baseline.removed_message_count == 0 {
        return baseline;
    }

    let removed_message_count = baseline.removed_message_count;
    let archived_messages = baseline.archived_messages.clone();
    // Kernel-side pinned protection (Req 4.9): even when a caller-provided
    // (e.g. model-generated) summary is used, retain pinned message content
    // verbatim so it is never summarized away. Idempotent w.r.t. the
    // orchestration-side pinned protection that may already have injected it.
    let summary = augment_summary_with_pinned_messages(&summary.into(), &archived_messages);
    let formatted_summary = format_compact_summary(&summary);
    let continuation = get_compact_continuation_message(
        &summary,
        true,
        baseline.compacted_session.messages.len() > 1,
    );

    let mut compacted_session = baseline.compacted_session;
    if let Some(first_message) = compacted_session.messages.first_mut() {
        first_message.role = MessageRole::System;
        first_message.blocks = vec![ContentBlock::Text { text: continuation }];
        first_message.thinking = None;
        first_message.thinking_signature = None;
        first_message.usage = None;
    }
    if let Some(compaction) = compacted_session.compaction.as_mut() {
        compaction.summary.clone_from(&summary);
        compaction.removed_message_count = baseline.removed_message_count;
        compaction
            .replacement_messages
            .clone_from(&compacted_session.messages);
        compaction.archived_messages.clone_from(&archived_messages);
    } else {
        compacted_session.record_compaction_checkpoint(
            summary.clone(),
            baseline.removed_message_count,
            compacted_session.messages.clone(),
            archived_messages.clone(),
        );
    }

    CompactionResult {
        summary,
        formatted_summary,
        compacted_session,
        removed_message_count,
        archived_messages,
    }
}

/// Returns the raw messages that would be removed by standard compaction.
/// Higher layers can persist selected entries as a recoverable history archive,
/// while keeping the model-visible compacted session small.
#[must_use]
pub fn collect_compaction_archive_messages(
    session: &Session,
    config: CompactionConfig,
) -> Vec<ConversationMessage> {
    compaction_message_window(session, config)
        .map(|(start, end)| session.messages[start..end].to_vec())
        .unwrap_or_default()
}

fn compacted_summary_prefix_len(session: &Session) -> usize {
    usize::from(
        session
            .messages
            .first()
            .and_then(extract_existing_compacted_summary)
            .is_some(),
    )
}

fn existing_compacted_summary(session: &Session) -> Option<String> {
    session
        .messages
        .first()
        .and_then(extract_existing_compacted_summary)
}

fn compaction_message_window(
    session: &Session,
    config: CompactionConfig,
) -> Option<(usize, usize)> {
    if !should_compact(session, config) {
        return None;
    }
    let existing_summary = existing_compacted_summary(session);
    let compacted_prefix_len = usize::from(existing_summary.is_some());
    // When preserve_recent_messages is 0, the caller wants maximum compaction
    // (no recent messages preserved). Without this guard, saturating_sub(0)
    // returns messages.len(), which later indexes past the end of the array
    // at session.messages[k] because keep_from == messages.len() is out of bounds.
    let raw_keep_from = if config.preserve_recent_messages == 0 {
        session.messages.len()
    } else {
        session
            .messages
            .len()
            .saturating_sub(config.preserve_recent_messages)
    };
    // Ensure we do not split a tool-use / tool-result pair at the compaction
    // boundary. If the first preserved message is a user message whose first
    // block is a ToolResult, the assistant message with the matching ToolUse
    // was slated for removal — that produces an orphaned tool role message on
    // the OpenAI-compat path (400: tool message must follow assistant with
    // tool_calls). Walk the boundary back until we start at a safe point.
    let keep_from = {
        let mut k = raw_keep_from;
        // If the first preserved message is a tool-result turn, ensure its
        // paired assistant tool-use turn is preserved too. Without this fix,
        // the OpenAI-compat adapter sends an orphaned 'tool' role message
        // with no preceding assistant 'tool_calls', which providers reject
        // with a 400. We walk back only if the immediately preceding message
        // is NOT an assistant message that contains a ToolUse block (i.e. the
        // pair is actually broken at the boundary).
        loop {
            if k == 0 || k <= compacted_prefix_len || k >= session.messages.len() {
                break;
            }
            let first_preserved = &session.messages[k];
            let starts_with_tool_result = first_preserved
                .blocks
                .first()
                .is_some_and(|b| matches!(b, ContentBlock::ToolResult { .. }));
            if !starts_with_tool_result {
                break;
            }
            // Check the message just before the current boundary.
            let preceding = &session.messages[k - 1];
            let preceding_has_tool_use = preceding
                .blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
            if preceding_has_tool_use {
                // Pair is intact — walk back one more to include the assistant turn.
                k = k.saturating_sub(1);
                break;
            }
            // Preceding message has no ToolUse but we have a ToolResult —
            // this is already an orphaned pair; walk back to try to fix it.
            k = k.saturating_sub(1);
        }
        k
    };
    // The previous continuation is itself being replaced. Keep it in the
    // exact archive so persistence can bind the parent compaction; only the
    // summary input remains anchored after that prefix.
    Some((0, keep_from))
}

fn summarize_messages(messages: &[ConversationMessage]) -> String {
    let user_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .count();
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .count();
    let tool_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .count();

    let mut tool_names = messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
            ContentBlock::ToolResult { tool_name, .. } => Some(tool_name.as_str()),
            ContentBlock::Text { .. } => None,
        })
        .collect::<Vec<_>>();
    tool_names.sort_unstable();
    tool_names.dedup();

    let mut lines = vec![
        "<summary>".to_string(),
        "Conversation summary:".to_string(),
        format!(
            "- Scope: {} earlier messages compacted (user={}, assistant={}, tool={}).",
            messages.len(),
            user_messages,
            assistant_messages,
            tool_messages
        ),
    ];

    if !tool_names.is_empty() {
        lines.push(format!("- Tools mentioned: {}.", tool_names.join(", ")));
    }

    let recent_user_requests = collect_recent_role_summaries(messages, MessageRole::User, 3);
    if !recent_user_requests.is_empty() {
        lines.push("- Recent user requests:".to_string());
        lines.extend(
            recent_user_requests
                .into_iter()
                .map(|request| format!("  - {request}")),
        );
    }

    let pending_work = infer_pending_work(messages);
    if !pending_work.is_empty() {
        lines.push("- Pending work:".to_string());
        lines.extend(pending_work.into_iter().map(|item| format!("  - {item}")));
    }

    let key_files = collect_key_files(messages);
    if !key_files.is_empty() {
        lines.push(format!("- Key files referenced: {}.", key_files.join(", ")));
    }

    if let Some(current_work) = infer_current_work(messages) {
        lines.push(format!("- Current work: {current_work}"));
    }

    lines.push("- Key timeline:".to_string());
    for message in messages {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let content = message
            .blocks
            .iter()
            .map(summarize_block)
            .collect::<Vec<_>>()
            .join(" | ");
        lines.push(format!("  - {role}: {content}"));
    }
    lines.push("</summary>".to_string());
    lines.join("\n")
}

fn merge_compact_summaries(existing_summary: Option<&str>, new_summary: &str) -> String {
    let Some(existing_summary) = existing_summary else {
        return new_summary.to_string();
    };

    let previous_highlights = extract_summary_highlights(existing_summary);
    let new_formatted_summary = format_compact_summary(new_summary);
    let new_highlights = extract_summary_highlights(&new_formatted_summary);
    let new_timeline = extract_summary_timeline(&new_formatted_summary);

    let mut lines = vec!["<summary>".to_string(), "Conversation summary:".to_string()];

    if !previous_highlights.is_empty() {
        // Flatten prior highlights directly — do NOT re-nest them under
        // "- Previously compacted context:" or the nesting compounds with each
        // compaction cycle, inflating the summary by ~depth * overhead per turn.
        lines.extend(
            previous_highlights
                .into_iter()
                .map(|line| format!("- {line}")),
        );
    }

    if !new_highlights.is_empty() {
        lines.push("- Newly compacted context:".to_string());
        lines.extend(new_highlights.into_iter().map(|line| format!("  {line}")));
    }

    if !new_timeline.is_empty() {
        lines.push("- Key timeline:".to_string());
        lines.extend(new_timeline.into_iter().map(|line| format!("  {line}")));
    }

    lines.push("</summary>".to_string());
    lines.join("\n")
}

fn summarize_block(block: &ContentBlock) -> String {
    let raw = match block {
        ContentBlock::Text { text } => text.clone(),
        ContentBlock::ToolUse { name, input, .. } if is_memory_tool_name(name) => {
            format!(
                "tool_use {name}({})",
                memory_reference_summary(Some(input), None)
            )
        }
        ContentBlock::ToolUse { name, input, .. } => format!("tool_use {name}({input})"),
        ContentBlock::ToolResult {
            tool_name,
            output,
            is_error,
            ..
        } if is_memory_tool_name(tool_name) => format!(
            "tool_result {tool_name}: {}{}",
            if *is_error { "error " } else { "" },
            memory_reference_summary(None, Some(output))
        ),
        ContentBlock::ToolResult {
            tool_name,
            output,
            is_error,
            ..
        } => format!(
            "tool_result {tool_name}: {}{output}",
            if *is_error { "error " } else { "" }
        ),
    };
    truncate_summary(&raw, 160)
}

fn is_memory_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name
            .trim()
            .replace('.', "_")
            .to_ascii_lowercase()
            .as_str(),
        "memory_list" | "memory_search" | "memory_read" | "memory_note"
    )
}

fn memory_reference_summary(input: Option<&str>, output: Option<&str>) -> String {
    let mut refs = Vec::new();
    if let Some(input) = input {
        if let Some(value) = json_string_field(input, "query") {
            refs.push(format!("query={}", truncate_summary(&value, 80)));
        }
        if let Some(value) = json_string_field(input, "path") {
            refs.push(format!("path={}", truncate_summary(&value, 80)));
        }
    }
    if let Some(output) = output {
        if let Some(value) = json_string_field(output, "path") {
            refs.push(format!("path={}", truncate_summary(&value, 80)));
        }
        let paths = json_memory_item_paths(output);
        if !paths.is_empty() {
            refs.push(format!(
                "paths={}",
                truncate_summary(&paths.join(", "), 120)
            ));
        }
    }
    if refs.is_empty() {
        "memory reference used; content omitted from compaction".to_string()
    } else {
        format!(
            "memory reference used: {}; content omitted from compaction",
            refs.join("; ")
        )
    }
}

fn json_string_field(raw: &str, key: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn json_memory_item_paths(raw: &str) -> Vec<String> {
    let Some(items) = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.get("items").cloned())
        .and_then(|value| value.as_array().cloned())
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.get("path").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(8)
        .map(ToOwned::to_owned)
        .collect()
}

fn collect_recent_role_summaries(
    messages: &[ConversationMessage],
    role: MessageRole,
    limit: usize,
) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == role)
        .rev()
        .filter_map(|message| first_text_block(message))
        .take(limit)
        .map(|text| truncate_summary(text, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn infer_pending_work(messages: &[ConversationMessage]) -> Vec<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text_block)
        .filter(|text| {
            let lowered = text.to_ascii_lowercase();
            lowered.contains("todo")
                || lowered.contains("next")
                || lowered.contains("pending")
                || lowered.contains("follow up")
                || lowered.contains("remaining")
        })
        .take(3)
        .map(|text| truncate_summary(text, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn collect_key_files(messages: &[ConversationMessage]) -> Vec<String> {
    let mut files = messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .map(|block| match block {
            ContentBlock::Text { text } => text.as_str(),
            ContentBlock::ToolUse { input, .. } => input.as_str(),
            ContentBlock::ToolResult { output, .. } => output.as_str(),
        })
        .flat_map(extract_file_candidates)
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files.into_iter().take(8).collect()
}

fn infer_current_work(messages: &[ConversationMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text_block)
        .find(|text| !text.trim().is_empty())
        .map(|text| truncate_summary(text, 200))
}

fn first_text_block(message: &ConversationMessage) -> Option<&str> {
    message.blocks.iter().find_map(|block| match block {
        ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
        ContentBlock::ToolUse { .. }
        | ContentBlock::ToolResult { .. }
        | ContentBlock::Text { .. } => None,
    })
}

fn has_interesting_extension(candidate: &str) -> bool {
    std::path::Path::new(candidate)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["rs", "ts", "tsx", "js", "json", "md"]
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

fn extract_file_candidates(content: &str) -> Vec<String> {
    content
        .split_whitespace()
        .filter_map(|token| {
            let candidate = token.trim_matches(|char: char| {
                matches!(char, ',' | '.' | ':' | ';' | ')' | '(' | '"' | '\'' | '`')
            });
            if candidate.contains('/') && has_interesting_extension(candidate) {
                Some(candidate.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn truncate_summary(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let mut truncated = content.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

fn extract_tag_block(content: &str, tag: &str) -> Option<String> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let start_index = content.find(&start)? + start.len();
    let end_index = content[start_index..].find(&end)? + start_index;
    Some(content[start_index..end_index].to_string())
}

fn strip_tag_block(content: &str, tag: &str) -> String {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    if let (Some(start_index), Some(end_index_rel)) = (content.find(&start), content.find(&end)) {
        let end_index = end_index_rel + end.len();
        let mut stripped = String::new();
        stripped.push_str(&content[..start_index]);
        stripped.push_str(&content[end_index..]);
        stripped
    } else {
        content.to_string()
    }
}

fn collapse_blank_lines(content: &str) -> String {
    let mut result = String::new();
    let mut last_blank = false;
    for line in content.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && last_blank {
            continue;
        }
        result.push_str(line);
        result.push('\n');
        last_blank = is_blank;
    }
    result
}

fn extract_existing_compacted_summary(message: &ConversationMessage) -> Option<String> {
    if message.role != MessageRole::System {
        return None;
    }

    let text = first_text_block(message)?;
    let summary = text.strip_prefix(COMPACT_CONTINUATION_PREAMBLE)?;
    let summary = summary
        .split_once(&format!("\n\n{COMPACT_RECENT_MESSAGES_NOTE}"))
        .map_or(summary, |(value, _)| value);
    let summary = summary
        .split_once(&format!("\n{COMPACT_DIRECT_RESUME_INSTRUCTION}"))
        .map_or(summary, |(value, _)| value);
    Some(summary.trim().to_string())
}

fn extract_summary_highlights(summary: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_timeline = false;

    for line in format_compact_summary(summary).lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed == "Summary:" || trimmed == "Conversation summary:" {
            continue;
        }
        if trimmed == "- Key timeline:" {
            in_timeline = true;
            continue;
        }
        if in_timeline {
            continue;
        }
        lines.push(trimmed.to_string());
    }

    lines
}

fn extract_summary_timeline(summary: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_timeline = false;

    for line in format_compact_summary(summary).lines() {
        let trimmed = line.trim_end();
        if trimmed == "- Key timeline:" {
            in_timeline = true;
            continue;
        }
        if !in_timeline {
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        lines.push(trimmed.to_string());
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::{
        collect_key_files, compact_session, compact_session_with_summary, format_compact_summary,
        get_compact_continuation_message, infer_pending_work, should_compact, CompactionConfig,
        PINNED_RETENTION_HEADER,
    };
    use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};

    #[test]
    fn formats_compact_summary_like_upstream() {
        let summary = "<analysis>scratch</analysis>\n<summary>Kept work</summary>";
        assert_eq!(format_compact_summary(summary), "Summary:\nKept work");
    }

    // A long verbatim pinned string that the timeline summarizer would truncate
    // (>160 chars), so plain summarization alone could not retain it as-is.
    fn pinned_secret() -> String {
        format!(
            "[pin] Deployment key is DEPLOY-{} — never discard this. {} end.",
            "A".repeat(120),
            "trailing detail".repeat(5)
        )
    }

    fn many_message_session(pinned: &str) -> Session {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text(pinned.to_string()),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "one ".repeat(200),
            }]),
            ConversationMessage::user_text("two ".repeat(200)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "three ".repeat(200),
            }]),
            ConversationMessage::user_text("recent message".to_string()),
        ];
        session
    }

    #[test]
    fn compact_session_retains_pinned_content_verbatim() {
        let pinned = pinned_secret();
        let session = many_message_session(&pinned);
        let config = CompactionConfig {
            preserve_recent_messages: 1,
            max_estimated_tokens: 1,
        };

        let result = compact_session(&session, config);

        assert!(
            result.removed_message_count > 0,
            "the pinned message must be part of the discarded window"
        );
        // Kernel-side protection: pinned content is retained byte-for-byte and
        // the retention header is present (Req 4.9).
        assert!(
            result.summary.contains(&pinned),
            "pinned content must be retained verbatim in the summary"
        );
        assert!(result.summary.contains(PINNED_RETENTION_HEADER));
        assert!(
            result.compacted_session.messages[0]
                .blocks
                .iter()
                .any(|block| matches!(
                    block,
                    ContentBlock::Text { text } if text.contains(&pinned)
                )),
            "pinned content must survive into the compacted system message"
        );
    }

    #[test]
    fn compact_session_with_summary_retains_pinned_content_verbatim() {
        let pinned = pinned_secret();
        let session = many_message_session(&pinned);
        let config = CompactionConfig {
            preserve_recent_messages: 1,
            max_estimated_tokens: 1,
        };

        // A model-style summary that entirely omits the pinned content.
        let result = compact_session_with_summary(
            &session,
            config,
            "<summary>Model summary that drops the pin.</summary>",
        );

        assert!(result.removed_message_count > 0);
        assert!(
            result.summary.contains(&pinned),
            "pinned content must be retained verbatim even with a caller summary"
        );
        assert!(result.summary.contains(PINNED_RETENTION_HEADER));
    }

    #[test]
    fn pinned_augmentation_is_idempotent() {
        let pinned = pinned_secret();
        let session = many_message_session(&pinned);
        let config = CompactionConfig {
            preserve_recent_messages: 1,
            max_estimated_tokens: 1,
        };

        // Supply a summary that already carries the pin verbatim: it must not be
        // duplicated by the kernel-side protection.
        let preset = format!("<summary>base</summary>\n\n{PINNED_RETENTION_HEADER}\n- {pinned}");
        let result = compact_session_with_summary(&session, config, preset);

        assert_eq!(
            result.summary.matches(&pinned).count(),
            1,
            "pinned content must not be duplicated"
        );
        assert_eq!(
            result.summary.matches(PINNED_RETENTION_HEADER).count(),
            1,
            "retention header must not be duplicated"
        );
    }

    #[test]
    fn compaction_without_pins_leaves_summary_unchanged() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("one ".repeat(200)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "two ".repeat(200),
            }]),
            ConversationMessage::user_text("three ".repeat(200)),
            ConversationMessage::user_text("recent".to_string()),
        ];
        let config = CompactionConfig {
            preserve_recent_messages: 1,
            max_estimated_tokens: 1,
        };

        let result = compact_session(&session, config);
        assert!(
            !result.summary.contains(PINNED_RETENTION_HEADER),
            "no retention block should be added when there are no pins"
        );
    }

    #[test]
    fn leaves_small_sessions_unchanged() {
        let mut session = Session::new();
        session.messages = vec![ConversationMessage::user_text("hello")];

        let result = compact_session(&session, CompactionConfig::default());
        assert_eq!(result.removed_message_count, 0);
        assert_eq!(result.compacted_session, session);
        assert!(result.summary.is_empty());
        assert!(result.formatted_summary.is_empty());
    }

    #[test]
    fn compacts_older_messages_into_a_system_summary() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("one ".repeat(200)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "two ".repeat(200),
            }]),
            ConversationMessage::tool_result("1", "bash", "ok ".repeat(200), false),
            ConversationMessage {
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock::Text {
                    text: "recent".to_string(),
                }],
                thinking: None,
                thinking_signature: None,
                usage: None,
            },
        ];

        let result = compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
        );

        // With the tool-use/tool-result boundary fix, the compaction preserves
        // one extra message to avoid an orphaned tool result at the boundary.
        // messages[1] (assistant) must be kept along with messages[2] (tool result).
        assert!(
            result.removed_message_count <= 2,
            "expected at most 2 removed, got {}",
            result.removed_message_count
        );
        assert_eq!(
            result.compacted_session.messages[0].role,
            MessageRole::System
        );
        assert!(matches!(
            &result.compacted_session.messages[0].blocks[0],
            ContentBlock::Text { text } if text.contains("Summary:")
        ));
        assert!(result.formatted_summary.contains("Scope:"));
        assert!(result.formatted_summary.contains("Key timeline:"));
        assert!(should_compact(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            }
        ));
        // Note: with the tool-use/tool-result boundary guard the compacted session
        // may preserve one extra message at the boundary, so token reduction is
        // not guaranteed for small sessions. The invariant that matters is that
        // the removed_message_count is non-zero (something was compacted).
        assert!(
            result.removed_message_count > 0,
            "compaction must remove at least one message"
        );
    }

    #[test]
    fn keeps_previous_compacted_context_when_compacting_again() {
        let mut initial_session = Session::new();
        initial_session.messages = vec![
            ConversationMessage::user_text("Investigate rust/crates/runtime/src/compact.rs"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "I will inspect the compact flow.".to_string(),
            }]),
            ConversationMessage::user_text("Also update rust/crates/runtime/src/conversation.rs"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Next: preserve prior summary context during auto compact.".to_string(),
            }]),
        ];
        let config = CompactionConfig {
            preserve_recent_messages: 2,
            max_estimated_tokens: 1,
        };

        let first = compact_session(&initial_session, config);
        let mut follow_up_messages = first.compacted_session.messages.clone();
        follow_up_messages.extend([
            ConversationMessage::user_text("Please add regression tests for compaction."),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Working on regression coverage now.".to_string(),
            }]),
        ]);

        let mut second_session = Session::new();
        second_session.messages = follow_up_messages;
        let second = compact_session(&second_session, config);

        // "Previously compacted context:" header is intentionally flattened
        // (no re-nesting) to avoid summary inflation on repeated compaction.
        assert!(!second
            .formatted_summary
            .contains("Previously compacted context:"));
        assert!(second
            .formatted_summary
            .contains("Scope: 2 earlier messages compacted"));
        assert!(second
            .formatted_summary
            .contains("Newly compacted context:"));
        assert!(second
            .formatted_summary
            .contains("Also update rust/crates/runtime/src/conversation.rs"));
        assert!(matches!(
            &second.compacted_session.messages[0].blocks[0],
            ContentBlock::Text { text }
                if !text.contains("Previously compacted context:")
                    && text.contains("Newly compacted context:")
        ));
        assert!(matches!(
            &second.compacted_session.messages[1].blocks[0],
            ContentBlock::Text { text } if text.contains("Please add regression tests for compaction.")
        ));
        assert_eq!(
            second.archived_messages.len(),
            3,
            "replacing an existing continuation archives the parent replacement"
        );
        assert!(matches!(
            second.archived_messages[0].role,
            MessageRole::System
        ));
    }

    #[test]
    fn ignores_existing_compacted_summary_when_deciding_to_recompact() {
        let summary = "<summary>Conversation summary:\n- Scope: earlier work preserved.\n- Key timeline:\n  - user: large preserved context\n</summary>";
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage {
                role: MessageRole::System,
                blocks: vec![ContentBlock::Text {
                    text: get_compact_continuation_message(summary, true, true),
                }],
                thinking: None,
                thinking_signature: None,
                usage: None,
            },
            ConversationMessage::user_text("tiny"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "recent".to_string(),
            }]),
        ];

        assert!(!should_compact(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            }
        ));
    }

    #[test]
    fn compact_session_with_summary_replaces_summary_but_preserves_tail() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("Old requirement: keep context continuity."),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "I inspected the earlier implementation.".to_string(),
            }]),
            ConversationMessage::user_text("Recent request should remain verbatim."),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Recent answer should remain verbatim.".to_string(),
            }]),
        ];
        let result = compact_session_with_summary(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
            "<summary>Conversation summary:\n- Model-generated compact summary.\n</summary>",
        );

        assert_eq!(result.removed_message_count, 2);
        assert!(result
            .formatted_summary
            .contains("Model-generated compact summary"));
        assert_eq!(
            result
                .compacted_session
                .compaction
                .as_ref()
                .map(|value| value.summary.as_str()),
            Some("<summary>Conversation summary:\n- Model-generated compact summary.\n</summary>")
        );
        assert!(matches!(
            &result.compacted_session.messages[1].blocks[0],
            ContentBlock::Text { text } if text == "Recent request should remain verbatim."
        ));
    }

    #[test]
    fn compact_session_keeps_removed_messages_as_archive_snapshot() {
        let sql = format!(
            "WITH base AS (SELECT user_id, SUM(revenue) AS revenue FROM fact GROUP BY user_id)\nSELECT * FROM base WHERE revenue > 0;\n{}",
            "metric_a,metric_b,roi\n1,2,0.5\n".repeat(80)
        );
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text(sql.clone()),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "I will analyze it.".to_string(),
            }]),
            ConversationMessage::user_text("Recent request remains visible."),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Recent answer remains visible.".to_string(),
            }]),
        ];

        let result = compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
        );

        assert_eq!(result.removed_message_count, 2);
        assert_eq!(result.archived_messages.len(), 2);
        assert_eq!(
            result
                .compacted_session
                .compaction
                .as_ref()
                .expect("compaction")
                .archived_messages,
            result.archived_messages
        );
        assert!(matches!(
            &result.archived_messages[0].blocks[0],
            ContentBlock::Text { text } if text == &sql
        ));
        assert!(matches!(
            &result.compacted_session.messages[1].blocks[0],
            ContentBlock::Text { text } if text == "Recent request remains visible."
        ));
    }

    #[test]
    fn compact_session_with_summary_preserves_archive_snapshot() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("CREATE TABLE t (id bigint); ".repeat(80)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Parsed.".to_string(),
            }]),
            ConversationMessage::user_text("Now continue."),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Continuing.".to_string(),
            }]),
        ];

        let result = compact_session_with_summary(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
            "<summary>Conversation summary:\n- Custom compact summary.\n</summary>",
        );

        assert_eq!(result.removed_message_count, 2);
        assert_eq!(result.archived_messages.len(), 2);
        let compaction = result
            .compacted_session
            .compaction
            .as_ref()
            .expect("compaction");
        assert_eq!(compaction.count, 1);
        assert_eq!(compaction.window_number, 1);
        assert_eq!(compaction.archived_messages, result.archived_messages);
        assert_eq!(compaction.archive_windows.len(), 1);
        assert_eq!(
            compaction.archive_windows[0].archived_messages,
            result.archived_messages
        );
        assert!(matches!(
            &result.archived_messages[0].blocks[0],
            ContentBlock::Text { text } if text.contains("CREATE TABLE")
        ));
        assert!(result.formatted_summary.contains("Custom compact summary"));
    }

    #[test]
    fn truncates_long_blocks_in_summary() {
        let summary = super::summarize_block(&ContentBlock::Text {
            text: "x".repeat(400),
        });
        assert!(summary.ends_with('…'));
        assert!(summary.chars().count() <= 161);
    }

    #[test]
    fn memory_tool_outputs_are_not_baked_into_compaction_summary() {
        let secret_memory = "以后所有 PM 报告都用老板汇报版，产品代号 Falcon";
        let summary = super::summarize_block(&ContentBlock::ToolResult {
            tool_use_id: "call_memory".to_string(),
            tool_name: "memory_search".to_string(),
            output: serde_json::json!({
                "items": [{
                    "path": "MEMORY.md",
                    "excerpt": secret_memory,
                    "lineStart": 1,
                    "lineEnd": 1
                }]
            })
            .to_string(),
            is_error: false,
        });

        assert!(summary.contains("memory reference used"));
        assert!(summary.contains("MEMORY.md"));
        assert!(!summary.contains(secret_memory));
        assert!(!summary.contains("Falcon"));
    }

    #[test]
    fn extracts_key_files_from_message_content() {
        let files = collect_key_files(&[ConversationMessage::user_text(
            "Update rust/crates/runtime/src/compact.rs and rust/crates/rusty-claude-cli/src/main.rs next.",
        )]);
        assert!(files.contains(&"rust/crates/runtime/src/compact.rs".to_string()));
        assert!(files.contains(&"rust/crates/rusty-claude-cli/src/main.rs".to_string()));
    }

    /// Regression: compaction must not split an assistant(ToolUse) /
    /// user(ToolResult) pair at the boundary. An orphaned tool-result message
    /// without the preceding assistant `tool_calls` causes a 400 on the
    /// OpenAI-compat path (gaebal-gajae repro 2026-04-09).
    #[test]
    fn compaction_does_not_split_tool_use_tool_result_pair() {
        use crate::session::{ContentBlock, Session};

        let tool_id = "call_abc";
        let mut session = Session::default();
        // Turn 1: user prompt
        session
            .push_message(ConversationMessage::user_text("Search for files"))
            .unwrap();
        // Turn 2: assistant calls a tool
        session
            .push_message(ConversationMessage::assistant(vec![
                ContentBlock::ToolUse {
                    id: tool_id.to_string(),
                    name: "search".to_string(),
                    input: "{\"q\":\"*.rs\"}".to_string(),
                },
            ]))
            .unwrap();
        // Turn 3: tool result
        session
            .push_message(ConversationMessage::tool_result(
                tool_id,
                "search",
                "found 5 files",
                false,
            ))
            .unwrap();
        // Turn 4: assistant final response
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Done.".to_string(),
            }]))
            .unwrap();

        // Compact preserving only 1 recent message — without the fix this
        // would cut the boundary so that the tool result (turn 3) is first,
        // without its preceding assistant tool_calls (turn 2).
        let config = CompactionConfig {
            preserve_recent_messages: 1,
            ..CompactionConfig::default()
        };
        let result = compact_session(&session, config);
        // After compaction, no two consecutive messages should have the pattern
        // tool_result immediately following a non-assistant message (i.e. an
        // orphaned tool result without a preceding assistant ToolUse).
        let messages = &result.compacted_session.messages;
        for i in 1..messages.len() {
            let curr_is_tool_result = messages[i]
                .blocks
                .first()
                .is_some_and(|b| matches!(b, ContentBlock::ToolResult { .. }));
            if curr_is_tool_result {
                let prev_has_tool_use = messages[i - 1]
                    .blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
                assert!(
                    prev_has_tool_use,
                    "message[{}] is a ToolResult but message[{}] has no ToolUse: {:?}",
                    i,
                    i - 1,
                    &messages[i - 1].blocks
                );
            }
        }
    }

    #[test]
    fn infers_pending_work_from_recent_messages() {
        let pending = infer_pending_work(&[
            ConversationMessage::user_text("done"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Next: update tests and follow up on remaining CLI polish.".to_string(),
            }]),
        ]);
        assert_eq!(pending.len(), 1);
        assert!(pending[0].contains("Next: update tests"));
    }
}
