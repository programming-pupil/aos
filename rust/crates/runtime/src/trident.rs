use crate::compact::{
    compact_session, replace_compaction_summary, CompactionConfig, CompactionResult,
};
use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};
use crate::token_estimator::estimate_message_tokens;
use std::collections::{BTreeMap, BTreeSet};

fn usize_ratio(numerator: usize, denominator: usize) -> f64 {
    let numerator = u32::try_from(numerator).unwrap_or(u32::MAX);
    let denominator = u32::try_from(denominator).unwrap_or(u32::MAX).max(1);
    f64::from(numerator) / f64::from(denominator)
}

/// Configuration for the Trident compaction pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TridentConfig {
    pub supersede_enabled: bool,
    pub collapse_enabled: bool,
    pub cluster_enabled: bool,
    pub collapse_threshold: usize,
    pub cluster_min_size: usize,
    pub cluster_similarity_threshold: f64,
    pub max_file_operations: usize,
}

impl Default for TridentConfig {
    fn default() -> Self {
        Self {
            supersede_enabled: true,
            collapse_enabled: true,
            cluster_enabled: true,
            collapse_threshold: 4,
            cluster_min_size: 3,
            cluster_similarity_threshold: 0.6,
            max_file_operations: 100,
        }
    }
}

/// Statistics from a Trident compaction run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TridentStats {
    pub superseded_count: usize,
    pub collapsed_chains: usize,
    pub messages_collapsed: usize,
    pub clusters_found: usize,
    pub messages_clustered: usize,
    pub tokens_saved_estimate: usize,
    pub original_message_count: usize,
    pub final_message_count: usize,
}

impl TridentStats {
    #[must_use]
    pub fn format_report(&self) -> String {
        let compression = if self.final_message_count > 0 {
            usize_ratio(self.original_message_count, self.final_message_count)
        } else {
            1.0
        };
        let mut lines = vec![
            "Trident Compaction Complete".to_string(),
            format!(
                "  Stage 1 (Supersede): {} obsolete removed",
                self.superseded_count
            ),
            format!(
                "  Stage 2 (Collapse):  {} -> {} summaries",
                self.messages_collapsed, self.collapsed_chains
            ),
            format!(
                "  Stage 3 (Cluster):   {} -> {} clusters",
                self.messages_clustered, self.clusters_found
            ),
            format!("  Original: {} messages", self.original_message_count),
            format!(
                "  Final:    {} messages ({:.1}x compression)",
                self.final_message_count, compression
            ),
        ];
        if self.tokens_saved_estimate > 0 {
            lines.push(format!(
                "  Est. tokens saved: ~{}",
                self.tokens_saved_estimate
            ));
        }
        lines.join("\n")
    }
}

/// Result of the Trident compaction pipeline.
#[derive(Debug, Clone)]
pub struct TridentResult {
    pub compacted_session: Session,
    pub stats: TridentStats,
}

/// Run the full Trident compaction pipeline on a session, then apply
/// the standard summary-based compaction.
pub fn trident_compact_session(
    session: &Session,
    compaction_config: CompactionConfig,
    trident_config: &TridentConfig,
) -> CompactionResult {
    let baseline = compact_session(session, compaction_config);
    if baseline.removed_message_count == 0 {
        return baseline;
    }
    let (sanitized_input, pre_sanitized_count) = sanitize_tool_pairing_session(session);
    let original_count = sanitized_input.messages.len();
    let original_tokens: usize = sanitized_input
        .messages
        .iter()
        .map(estimate_message_tokens)
        .sum();

    let mut stats = TridentStats {
        original_message_count: original_count,
        ..TridentStats::default()
    };

    // Trident optimizes only the older segment that standard compaction is
    // about to summarize. Keeping the active tail byte-for-byte avoids
    // breaking live tool-use/tool-result adjacency near the current turn.
    let prefix_len = existing_compacted_summary_prefix_len(&sanitized_input);
    let protected_tail_len = compaction_config
        .preserve_recent_messages
        .min(sanitized_input.messages.len().saturating_sub(prefix_len));
    let optimize_end = sanitized_input
        .messages
        .len()
        .saturating_sub(protected_tail_len);
    let mut messages = sanitized_input.messages[prefix_len..optimize_end].to_vec();

    if trident_config.supersede_enabled {
        let (kept, superseded_count) = stage1_supersede(&messages);
        stats.superseded_count = superseded_count;
        messages = kept;
    }

    if trident_config.collapse_enabled {
        let (collapsed, chains, collapsed_count) =
            stage2_collapse(&messages, trident_config.collapse_threshold);
        stats.collapsed_chains = chains;
        stats.messages_collapsed = collapsed_count;
        messages = collapsed;
    }

    if trident_config.cluster_enabled {
        let (clustered, clusters_found, messages_clustered) = stage3_cluster(
            &messages,
            trident_config.cluster_min_size,
            trident_config.cluster_similarity_threshold,
        );
        stats.clusters_found = clusters_found;
        stats.messages_clustered = messages_clustered;
        messages = clustered;
    }

    stats.final_message_count = messages.len();

    let final_tokens: usize = messages.iter().map(estimate_message_tokens).sum();
    stats.tokens_saved_estimate = original_tokens.saturating_sub(final_tokens);

    let mut trident_messages = Vec::with_capacity(
        prefix_len + messages.len() + session.messages.len().saturating_sub(optimize_end),
    );
    trident_messages.extend_from_slice(&sanitized_input.messages[..prefix_len]);
    trident_messages.extend(messages);
    trident_messages.extend_from_slice(&sanitized_input.messages[optimize_end..]);

    let mut trident_session = sanitized_input.clone();
    trident_session.messages = trident_messages;
    let (trident_session, post_stage_sanitized_count) =
        sanitize_tool_pairing_session(&trident_session);

    // The reducer may shrink the prefix below the original trigger threshold.
    // Force a candidate summary when possible, then install it on the original
    // baseline so archives and removal counts describe exact source messages.
    let reduced_candidate = compact_session(
        &trident_session,
        CompactionConfig {
            max_estimated_tokens: 0,
            ..compaction_config
        },
    );
    let reduced_summary = if reduced_candidate.summary.trim().is_empty() {
        baseline.summary.clone()
    } else {
        reduced_candidate.summary
    };
    let result = replace_compaction_summary(baseline, reduced_summary);

    if stats.superseded_count > 0 || stats.collapsed_chains > 0 || stats.clusters_found > 0 {
        tracing::debug!(
            target: "runtime::trident",
            report = %stats.format_report(),
            pre_sanitized_count,
            post_stage_sanitized_count,
            "trident compaction complete"
        );
    }

    result
}

fn existing_compacted_summary_prefix_len(session: &Session) -> usize {
    usize::from(
        session
            .messages
            .first()
            .is_some_and(is_compacted_continuation_message),
    )
}

fn is_compacted_continuation_message(message: &ConversationMessage) -> bool {
    message.role == MessageRole::System && message.blocks.iter().any(|block| {
        matches!(
            block,
            ContentBlock::Text { text }
                if text.starts_with("This session is being continued from a previous conversation")
        )
    })
}

fn sanitize_tool_pairing_session(session: &Session) -> (Session, usize) {
    let messages = sanitize_tool_pairing_messages(&session.messages);
    let removed = session.messages.len().saturating_sub(messages.len());
    if removed == 0 {
        return (session.clone(), 0);
    }
    let mut sanitized = session.clone();
    sanitized.messages = messages;
    (sanitized, removed)
}

fn sanitize_tool_pairing_messages(messages: &[ConversationMessage]) -> Vec<ConversationMessage> {
    let mut sanitized = Vec::with_capacity(messages.len());
    let mut i = 0usize;
    while i < messages.len() {
        let message = &messages[i];
        if message.role == MessageRole::Assistant {
            let expected_tool_use_ids = tool_use_ids(message);
            if !expected_tool_use_ids.is_empty() {
                let mut j = i + 1;
                let mut seen_tool_result_ids = BTreeSet::new();
                let mut invalid_segment = false;

                while j < messages.len() && has_tool_result(&messages[j]) {
                    let result_ids = tool_result_ids(&messages[j]);
                    if result_ids.is_empty()
                        || result_ids
                            .iter()
                            .any(|id| !expected_tool_use_ids.contains(id))
                    {
                        invalid_segment = true;
                    }
                    seen_tool_result_ids.extend(result_ids);
                    j += 1;
                }

                let missing_result = expected_tool_use_ids
                    .iter()
                    .any(|id| !seen_tool_result_ids.contains(id));
                if invalid_segment || missing_result {
                    i = j;
                    continue;
                }

                sanitized.push(message.clone());
                sanitized.extend_from_slice(&messages[(i + 1)..j]);
                i = j;
                continue;
            }
        }

        if has_tool_result(message) {
            i += 1;
            continue;
        }

        sanitized.push(message.clone());
        i += 1;
    }
    sanitized
}

fn tool_use_ids(message: &ConversationMessage) -> BTreeSet<String> {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
            ContentBlock::Text { .. } | ContentBlock::ToolResult { .. } => None,
        })
        .collect()
}

fn tool_result_ids(message: &ConversationMessage) -> BTreeSet<String> {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            ContentBlock::Text { .. } | ContentBlock::ToolUse { .. } => None,
        })
        .collect()
}

fn has_tool_result(message: &ConversationMessage) -> bool {
    message
        .blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

// =============================================================================
// STAGE 1: SUPERSEDE — Zero-cost factual pruning
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOp {
    Read,
    Write,
    Edit,
}

#[derive(Debug)]
struct FileOperation {
    index: usize,
    op_type: FileOp,
}

fn stage1_supersede(messages: &[ConversationMessage]) -> (Vec<ConversationMessage>, usize) {
    let mut file_ops: BTreeMap<String, Vec<FileOperation>> = BTreeMap::new();

    for (i, msg) in messages.iter().enumerate() {
        for block in &msg.blocks {
            if let Some((path, op_type)) = extract_file_operation(block) {
                file_ops
                    .entry(path)
                    .or_default()
                    .push(FileOperation { index: i, op_type });
            }
        }
    }

    let mut obsolete_indices: BTreeSet<usize> = BTreeSet::new();

    for ops in file_ops.values() {
        if ops.len() < 2 {
            continue;
        }

        let last_write_idx = ops
            .iter()
            .rev()
            .find(|op| op.op_type == FileOp::Write || op.op_type == FileOp::Edit)
            .map(|op| op.index);

        if let Some(last_write) = last_write_idx {
            for op in ops {
                if (op.op_type == FileOp::Read
                    || op.op_type == FileOp::Write
                    || op.op_type == FileOp::Edit)
                    && op.index < last_write
                {
                    obsolete_indices.insert(op.index);
                }
            }
        }
    }

    let superseded_count = obsolete_indices.len();
    let kept: Vec<ConversationMessage> = messages
        .iter()
        .enumerate()
        .filter(|(i, _)| !obsolete_indices.contains(i))
        .map(|(_, msg)| msg.clone())
        .collect();

    (kept, superseded_count)
}

fn extract_file_operation(block: &ContentBlock) -> Option<(String, FileOp)> {
    match block {
        ContentBlock::ToolUse { name, input, .. } => {
            let path = extract_path_from_tool_input(name, input)?;
            let op_type = match name.as_str() {
                "read_file" | "Read" => FileOp::Read,
                "write_file" | "Write" => FileOp::Write,
                "edit_file" | "Edit" => FileOp::Edit,
                _ => return None,
            };
            Some((path, op_type))
        }
        ContentBlock::ToolResult {
            tool_name, output, ..
        } => {
            let path = extract_path_from_tool_output(tool_name, output)?;
            let op_type = match tool_name.as_str() {
                "read_file" | "Read" => FileOp::Read,
                "write_file" | "Write" => FileOp::Write,
                "edit_file" | "Edit" => FileOp::Edit,
                _ => return None,
            };
            Some((path, op_type))
        }
        ContentBlock::Text { .. } => None,
    }
}

fn extract_path_from_tool_input(tool_name: &str, input: &str) -> Option<String> {
    if !matches!(
        tool_name,
        "read_file" | "write_file" | "edit_file" | "Read" | "Write" | "Edit"
    ) {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|v| v.get("path")?.as_str().map(String::from))
        .or_else(|| {
            serde_json::from_str::<serde_json::Value>(input)
                .ok()
                .and_then(|v| v.get("file_path")?.as_str().map(String::from))
        })
}

fn extract_path_from_tool_output(tool_name: &str, output: &str) -> Option<String> {
    if !matches!(
        tool_name,
        "read_file" | "write_file" | "edit_file" | "Read" | "Write" | "Edit"
    ) {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|v| v.get("path")?.as_str().map(String::from))
        .or_else(|| {
            output
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("path: "))
                .map(String::from)
        })
}

// =============================================================================
// STAGE 2: COLLAPSE — Summarize chatty exchanges
// =============================================================================

fn stage2_collapse(
    messages: &[ConversationMessage],
    threshold: usize,
) -> (Vec<ConversationMessage>, usize, usize) {
    if messages.len() < threshold {
        return (messages.to_vec(), 0, 0);
    }

    let mut result: Vec<ConversationMessage> = Vec::new();
    let mut buffer: Vec<ConversationMessage> = Vec::new();
    let mut total_chains = 0;
    let mut total_collapsed = 0;

    for msg in messages {
        if is_chatty_message(msg) {
            buffer.push(msg.clone());
        } else {
            if buffer.len() >= threshold {
                let summary = generate_collapse_summary(&buffer);
                total_chains += 1;
                total_collapsed += buffer.len();
                result.push(ConversationMessage {
                    role: MessageRole::System,
                    blocks: vec![ContentBlock::Text {
                        text: format!("[Collapsed Conversation]\n{summary}"),
                    }],
                    thinking: None,
                    thinking_signature: None,
                    usage: None,
                });
            } else {
                result.append(&mut buffer);
            }
            buffer.clear();
            result.push(msg.clone());
        }
    }

    if buffer.len() >= threshold {
        let summary = generate_collapse_summary(&buffer);
        total_chains += 1;
        total_collapsed += buffer.len();
        result.push(ConversationMessage {
            role: MessageRole::System,
            blocks: vec![ContentBlock::Text {
                text: format!("[Collapsed Conversation]\n{summary}"),
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        });
    } else {
        result.extend(buffer);
    }

    (result, total_chains, total_collapsed)
}

fn is_chatty_message(msg: &ConversationMessage) -> bool {
    let total_chars: usize = msg
        .blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { input, .. } => input.len(),
            ContentBlock::ToolResult { output, .. } => output.len(),
        })
        .sum::<usize>()
        + msg.thinking.as_deref().map_or(0, str::len);

    let has_tool_use = msg
        .blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let has_tool_result = msg
        .blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult { .. }));

    if has_tool_use || has_tool_result {
        return false;
    }

    total_chars < 200
}

fn generate_collapse_summary(messages: &[ConversationMessage]) -> String {
    let user_count = messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .count();
    let assistant_count = messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .count();

    let mut topics: Vec<String> = messages
        .iter()
        .filter_map(|m| {
            m.blocks.iter().find_map(|b| match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => {
                    Some(truncate_text(text, 80))
                }
                _ => None,
            })
        })
        .take(5)
        .collect();
    topics.dedup();

    let mut lines = vec![format!(
        "Collapsed {} messages ({} user, {} assistant).",
        messages.len(),
        user_count,
        assistant_count
    )];

    if !topics.is_empty() {
        lines.push("Topics:".to_string());
        for topic in &topics {
            lines.push(format!("  - {topic}"));
        }
    }

    lines.join("\n")
}

// =============================================================================
// STAGE 3: CLUSTER — Semantic grouping and deep storage
// =============================================================================

fn stage3_cluster(
    messages: &[ConversationMessage],
    min_cluster_size: usize,
    similarity_threshold: f64,
) -> (Vec<ConversationMessage>, usize, usize) {
    if messages.len() < min_cluster_size {
        return (messages.to_vec(), 0, 0);
    }

    let fingerprints: Vec<MessageFingerprint> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, msg)| fingerprint_message(i, msg))
        .collect();

    if fingerprints.len() < min_cluster_size {
        return (messages.to_vec(), 0, 0);
    }

    let mut cluster_assignments: BTreeMap<usize, usize> = BTreeMap::new();
    let mut cluster_id = 0;

    for i in 0..fingerprints.len() {
        if cluster_assignments.contains_key(&fingerprints[i].index) {
            continue;
        }

        let mut cluster_members: Vec<usize> = vec![fingerprints[i].index];

        for j in (i + 1)..fingerprints.len() {
            if cluster_assignments.contains_key(&fingerprints[j].index) {
                continue;
            }

            let similarity = compute_similarity(&fingerprints[i], &fingerprints[j]);
            if similarity >= similarity_threshold {
                cluster_members.push(fingerprints[j].index);
            }
        }

        if cluster_members.len() >= min_cluster_size {
            for member_idx in &cluster_members {
                cluster_assignments.insert(*member_idx, cluster_id);
            }
            cluster_id += 1;
        }
    }

    if cluster_assignments.is_empty() {
        return (messages.to_vec(), 0, 0);
    }

    let total_clustered: usize = cluster_assignments.len();
    let clusters_found = cluster_id;

    let mut result: Vec<ConversationMessage> = Vec::new();
    let mut cluster_buffers: BTreeMap<usize, Vec<usize>> = BTreeMap::new();

    for (msg_idx, &cid) in &cluster_assignments {
        cluster_buffers.entry(cid).or_default().push(*msg_idx);
    }

    for (i, msg) in messages.iter().enumerate() {
        if let Some(&cid) = cluster_assignments.get(&i) {
            if let Some(buffer) = cluster_buffers.get_mut(&cid) {
                if buffer[0] == i {
                    let cluster_messages: Vec<&ConversationMessage> =
                        buffer.iter().filter_map(|&idx| messages.get(idx)).collect();
                    let summary = generate_cluster_summary(&cluster_messages);
                    result.push(ConversationMessage {
                        role: MessageRole::System,
                        blocks: vec![ContentBlock::Text {
                            text: format!("[Clustered {} messages]\n{summary}", buffer.len()),
                        }],
                        thinking: None,
                        thinking_signature: None,
                        usage: None,
                    });
                }
            }
        } else {
            result.push(msg.clone());
        }
    }

    (result, clusters_found, total_clustered)
}

#[derive(Debug)]
struct MessageFingerprint {
    index: usize,
    tool_names: BTreeSet<String>,
    file_paths: BTreeSet<String>,
    role: MessageRole,
    text_length: usize,
}

fn fingerprint_message(index: usize, msg: &ConversationMessage) -> Option<MessageFingerprint> {
    if msg.role == MessageRole::System {
        return None;
    }
    if msg.blocks.iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
        )
    }) {
        return None;
    }

    let mut tool_names: BTreeSet<String> = BTreeSet::new();
    let mut file_paths: BTreeSet<String> = BTreeSet::new();
    let mut text_length = 0;

    for block in &msg.blocks {
        match block {
            ContentBlock::ToolUse { name, input, .. } => {
                tool_names.insert(name.clone());
                if let Some(path) = extract_path_from_tool_input(name, input) {
                    file_paths.insert(path);
                }
                text_length += input.len();
            }
            ContentBlock::ToolResult {
                tool_name, output, ..
            } => {
                tool_names.insert(tool_name.clone());
                if let Some(path) = extract_path_from_tool_output(tool_name, output) {
                    file_paths.insert(path);
                }
                text_length += output.len();
            }
            ContentBlock::Text { text } => {
                text_length += text.len();
            }
        }
    }
    text_length += msg.thinking.as_deref().map_or(0, str::len);

    Some(MessageFingerprint {
        index,
        tool_names,
        file_paths,
        role: msg.role,
        text_length,
    })
}

fn compute_similarity(a: &MessageFingerprint, b: &MessageFingerprint) -> f64 {
    if a.role != b.role {
        return 0.0;
    }

    let tool_overlap = if a.tool_names.is_empty() && b.tool_names.is_empty() {
        1.0
    } else if a.tool_names.is_empty() || b.tool_names.is_empty() {
        0.0
    } else {
        let intersection: usize = a.tool_names.intersection(&b.tool_names).count();
        let union: usize = a.tool_names.union(&b.tool_names).count();
        usize_ratio(intersection, union)
    };

    let file_overlap = if a.file_paths.is_empty() && b.file_paths.is_empty() {
        1.0
    } else if a.file_paths.is_empty() || b.file_paths.is_empty() {
        0.0
    } else {
        let intersection: usize = a.file_paths.intersection(&b.file_paths).count();
        let union: usize = a.file_paths.union(&b.file_paths).count();
        usize_ratio(intersection, union)
    };

    let length_similarity = if a.text_length == 0 && b.text_length == 0 {
        1.0
    } else if a.text_length == 0 || b.text_length == 0 {
        0.0
    } else {
        usize_ratio(
            a.text_length.min(b.text_length),
            a.text_length.max(b.text_length),
        )
    };

    0.4 * tool_overlap + 0.4 * file_overlap + 0.2 * length_similarity
}

fn generate_cluster_summary(messages: &[&ConversationMessage]) -> String {
    let mut tool_names: BTreeSet<String> = BTreeSet::new();
    let mut file_paths: BTreeSet<String> = BTreeSet::new();

    for msg in messages {
        for block in &msg.blocks {
            match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    tool_names.insert(name.clone());
                    if let Some(path) = extract_path_from_tool_input(name, input) {
                        file_paths.insert(path);
                    }
                }
                ContentBlock::ToolResult {
                    tool_name, output, ..
                } => {
                    tool_names.insert(tool_name.clone());
                    if let Some(path) = extract_path_from_tool_output(tool_name, output) {
                        file_paths.insert(path);
                    }
                }
                ContentBlock::Text { .. } => {}
            }
        }
    }

    let mut lines = vec![format!("{} similar messages grouped.", messages.len())];

    if !tool_names.is_empty() {
        lines.push(format!(
            "Tools: {}.",
            tool_names.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }

    if !file_paths.is_empty() {
        let paths: Vec<String> = file_paths.iter().take(5).cloned().collect();
        lines.push(format!("Files: {}.", paths.join(", ")));
    }

    lines.join("\n")
}

// =============================================================================
// Utilities
// =============================================================================

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::CompactionConfig;
    use crate::session::{ContentBlock, ConversationMessage, Session};

    #[test]
    fn stage1_removes_obsolete_file_reads() {
        let messages = vec![
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "1",
                "read_file",
                r#"{"path":"src/main.rs","content":"old"}"#,
                false,
            ),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "2".to_string(),
                name: "edit_file".to_string(),
                input: r#"{"path":"src/main.rs","old":"old","new":"new"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "2",
                "edit_file",
                r#"{"path":"src/main.rs","ok":true}"#,
                false,
            ),
        ];

        let (kept, superseded) = stage1_supersede(&messages);
        assert!(superseded > 0, "should supersede the earlier read");
        assert!(kept.len() < messages.len());
    }

    #[test]
    fn stage1_keeps_standalone_reads() {
        let messages = vec![
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "1",
                "read_file",
                r#"{"path":"src/main.rs","content":"data"}"#,
                false,
            ),
        ];

        let (kept, superseded) = stage1_supersede(&messages);
        assert_eq!(superseded, 0);
        assert_eq!(kept.len(), messages.len());
    }

    #[test]
    fn stage2_collapses_chatty_messages() {
        let mut messages = vec![];
        for i in 0..6 {
            messages.push(ConversationMessage::user_text(format!("ok {i}")));
            messages.push(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: format!("got {i}"),
            }]));
        }
        messages.push(ConversationMessage::assistant(vec![
            ContentBlock::ToolUse {
                id: "t".to_string(),
                name: "bash".to_string(),
                input: r#"{"command":"ls"}"#.to_string(),
            },
        ]));

        let (result, chains, collapsed) = stage2_collapse(&messages, 4);
        assert!(chains > 0, "should collapse at least one chain");
        assert!(collapsed > 0);
        assert!(result.len() < messages.len());
    }

    #[test]
    fn stage3_clusters_similar_messages() {
        let mut messages = vec![];
        for i in 0..5 {
            messages.push(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: format!("Reviewed similar frontend state issue {i} and found no blockers."),
            }]));
        }

        let (result, clusters, clustered) = stage3_cluster(&messages, 3, 0.4);
        assert!(clusters > 0, "should find at least one cluster");
        assert!(clustered > 0);
        assert!(result.len() < messages.len());
    }

    #[test]
    fn stage3_does_not_cluster_tool_pairs() {
        let mut messages = vec![];
        for i in 0..5 {
            messages.push(ConversationMessage::assistant(vec![
                ContentBlock::ToolUse {
                    id: format!("read_{i}"),
                    name: "read_file".to_string(),
                    input: format!(r#"{{"path":"src/{i}.rs"}}"#),
                },
            ]));
            messages.push(ConversationMessage::tool_result(
                format!("read_{i}"),
                "read_file",
                format!(r#"{{"path":"src/{i}.rs","content":"data {i}"}}"#),
                false,
            ));
        }

        let (result, clusters, clustered) = stage3_cluster(&messages, 3, 0.4);
        assert_eq!(clusters, 0);
        assert_eq!(clustered, 0);
        assert_eq!(result, messages);
    }

    #[test]
    fn trident_sanitizer_removes_orphaned_tool_results_and_dangling_tool_uses() {
        let messages = vec![
            ConversationMessage::user_text("start"),
            ConversationMessage::tool_result("call_orphan", "glob_search", "orphaned", true),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "call_missing".to_string(),
                name: "grep_search".to_string(),
                input: r#"{"pattern":"x"}"#.to_string(),
            }]),
            ConversationMessage::user_text("next"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "call_ok".to_string(),
                name: "glob_search".to_string(),
                input: r#"{"pattern":"*.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result("call_ok", "glob_search", "ok", false),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "done".to_string(),
            }]),
        ];

        let sanitized = sanitize_tool_pairing_messages(&messages);

        assert_eq!(sanitized.len(), 5);
        assert!(matches!(sanitized[0].role, MessageRole::User));
        assert!(matches!(sanitized[1].role, MessageRole::User));
        assert!(matches!(sanitized[2].role, MessageRole::Assistant));
        assert!(has_tool_result(&sanitized[3]));
        assert!(matches!(sanitized[4].role, MessageRole::Assistant));
    }

    #[test]
    fn trident_full_pipeline_outputs_valid_tool_pairing() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("start"),
            ConversationMessage::tool_result("call_orphan", "glob_search", "orphaned", true),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "call_ok".to_string(),
                name: "glob_search".to_string(),
                input: r#"{"pattern":"*.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result("call_ok", "glob_search", "ok", false),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "done".to_string(),
            }]),
        ];

        let result = trident_compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
            &TridentConfig::default(),
        );

        assert_eq!(
            result.compacted_session.messages,
            sanitize_tool_pairing_messages(&result.compacted_session.messages)
        );
    }

    #[test]
    fn trident_full_pipeline_preserves_important_content() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("Read and fix main.rs"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "1",
                "read_file",
                r#"{"path":"src/main.rs","content":"fn main() { buggy }"}"#,
                false,
            ),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "2".to_string(),
                name: "edit_file".to_string(),
                input: r#"{"path":"src/main.rs","old":"buggy","new":"fixed"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "2",
                "edit_file",
                r#"{"path":"src/main.rs","ok":true}"#,
                false,
            ),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Fixed the bug in main.rs".to_string(),
            }]),
        ];

        let trident_config = TridentConfig::default();
        let result = trident_compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 4,
                max_estimated_tokens: 1,
            },
            &trident_config,
        );

        assert!(
            result.removed_message_count > 0
                || result.compacted_session.messages.len() < session.messages.len()
        );
    }

    #[test]
    fn trident_archive_metadata_matches_the_exact_source_window() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("inspect src/main.rs"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "read-1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "read-1",
                "read_file",
                r#"{"path":"src/main.rs","content":"old"}"#,
                false,
            ),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "write-1".to_string(),
                name: "write_file".to_string(),
                input: r#"{"path":"src/main.rs","content":"new"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "write-1",
                "write_file",
                r#"{"path":"src/main.rs","ok":true}"#,
                false,
            ),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "updated".to_string(),
            }]),
            ConversationMessage::user_text("verify it"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "verified".to_string(),
            }]),
        ];

        let result = trident_compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
            &TridentConfig::default(),
        );
        let compaction = result
            .compacted_session
            .compaction
            .as_ref()
            .expect("compaction metadata");
        assert_eq!(result.removed_message_count, result.archived_messages.len());
        assert_eq!(
            compaction.removed_message_count,
            result.removed_message_count
        );
        assert_eq!(compaction.archived_messages, result.archived_messages);
        assert_eq!(
            compaction
                .archive_windows
                .last()
                .expect("archive window")
                .archived_messages,
            result.archived_messages
        );
    }

    #[test]
    fn trident_archives_unsanitized_source_messages_without_hidden_loss() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("inspect the workspace"),
            ConversationMessage::tool_result(
                "orphan-result",
                "read_file",
                "orphaned but recoverable",
                true,
            ),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "dangling-use".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"README.md"}"#.to_string(),
            }]),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "continue safely".to_string(),
            }]),
            ConversationMessage::user_text("final objective"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "final answer".to_string(),
            }]),
        ];
        let config = CompactionConfig {
            preserve_recent_messages: 2,
            max_estimated_tokens: 1,
        };
        let baseline = compact_session(&session, config);
        let result = trident_compact_session(&session, config, &TridentConfig::default());

        assert_eq!(result.archived_messages, baseline.archived_messages);
        assert_eq!(result.removed_message_count, baseline.removed_message_count);
        assert!(result.archived_messages.iter().any(has_tool_result));
        assert!(result.archived_messages.iter().any(|message| {
            message.blocks.iter().any(
                |block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "dangling-use"),
            )
        }));
    }

    #[test]
    fn trident_stats_report() {
        let stats = TridentStats {
            superseded_count: 5,
            collapsed_chains: 2,
            messages_collapsed: 8,
            clusters_found: 1,
            messages_clustered: 3,
            tokens_saved_estimate: 1200,
            original_message_count: 20,
            final_message_count: 8,
        };
        let report = stats.format_report();
        assert!(report.contains("Stage 1 (Supersede): 5"));
        assert!(report.contains("Stage 2 (Collapse):  8 -> 2"));
        assert!(report.contains("Stage 3 (Cluster):   3 -> 1"));
        assert!(report.contains("1200") || report.contains("1,200"));
    }
}
