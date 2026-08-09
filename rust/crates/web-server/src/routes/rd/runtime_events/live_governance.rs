//! Runtime tool live governance and attribution-aware event enrichment.

use super::attribution::{
    normalize_rd_runtime_attribution_path, rd_runtime_retrieval_reason_from_sources,
    RdRuntimeAttributionContext,
};
use super::mapping::{
    classify_rd_runtime_tool, rd_runtime_event_to_task_event, summarize_rd_tool_text,
};
use super::*;

pub(super) struct RdRuntimeLiveGovernance {
    plan: Option<RdRuntimeToolGovernancePlan>,
    attribution: RdRuntimeAttributionContext,
    active_tools: HashMap<u32, String>,
    observed_inputs: HashSet<u32>,
    observed_results: HashSet<u32>,
    tool_counts: BTreeMap<String, usize>,
    target_counts: BTreeMap<String, usize>,
    input_hash_counts: BTreeMap<String, usize>,
    failed_target_counts: BTreeMap<String, usize>,
    failed_input_hash_counts: BTreeMap<String, usize>,
    empty_result_counts: BTreeMap<String, usize>,
    unattributed_read_targets: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
struct RdRuntimeToolAttribution {
    matched: bool,
    reason: Option<String>,
    sources: Vec<String>,
    score: Option<f64>,
    rank: Option<u64>,
    notes: Vec<String>,
}

impl RdRuntimeToolAttribution {
    fn fallback() -> Self {
        Self::default()
    }

    fn to_json(&self) -> Value {
        json!({
            "matched": self.matched,
            "reason": self.reason,
            "sources": self.sources,
            "score": self.score,
            "rank": self.rank,
            "notes": self.notes,
        })
    }
}

impl RdRuntimeLiveGovernance {
    pub(super) fn new(
        plan: Option<RdRuntimeToolGovernancePlan>,
        attribution: RdRuntimeAttributionContext,
    ) -> Self {
        Self {
            plan,
            attribution,
            active_tools: HashMap::new(),
            observed_inputs: HashSet::new(),
            observed_results: HashSet::new(),
            tool_counts: BTreeMap::new(),
            target_counts: BTreeMap::new(),
            input_hash_counts: BTreeMap::new(),
            failed_target_counts: BTreeMap::new(),
            failed_input_hash_counts: BTreeMap::new(),
            empty_result_counts: BTreeMap::new(),
            unattributed_read_targets: BTreeSet::new(),
        }
    }

    pub(super) fn event_to_task_event(
        &mut self,
        event: agent_gateway::AgentEvent,
    ) -> Option<(String, String, String, Value)> {
        match event {
            agent_gateway::AgentEvent::ToolUseStart { index, id, name } => {
                self.active_tools.insert(index, name.clone());
                Some((
                    "runtime_tool".to_string(),
                    "running".to_string(),
                    format!("runtime 准备调用工具 {name}"),
                    json!({
                        "event": "tool_use_start",
                        "index": index,
                        "toolUseId": id,
                        "toolName": name,
                    }),
                ))
            }
            agent_gateway::AgentEvent::ToolUseInput { index, input } => {
                let tool_name = self
                    .active_tools
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let input_summary = summarize_rd_tool_text(&input, 900);
                let target = rd_runtime_tool_target(&tool_name, &input);
                let attribution = self.attribution_for_tool(&tool_name, target.as_deref(), &input);
                let reason = attribution.reason.as_deref().map_or_else(
                    || rd_runtime_tool_reason(&tool_name, target.as_deref(), &input),
                    ToOwned::to_owned,
                );
                let governance_snapshot = self.observe_tool_input(
                    index,
                    &tool_name,
                    target.as_deref(),
                    &input_summary.hash,
                    &attribution,
                );
                let target_label = target.as_deref().unwrap_or("目标待解析");
                Some((
                    "runtime_tool".to_string(),
                    "running".to_string(),
                    format!("runtime 工具 {tool_name} 准备访问：{target_label}"),
                    json!({
                        "event": "tool_use_input",
                        "index": index,
                        "toolName": tool_name.clone(),
                        "target": target.clone(),
                        "reason": reason.clone(),
                        "attribution": attribution.to_json(),
                        "governanceSnapshot": governance_snapshot.clone(),
                        "toolCalls": [{
                            "index": index,
                            "toolName": tool_name,
                            "input": input_summary.text,
                            "inputChars": input_summary.original_chars,
                            "inputHash": input_summary.hash,
                            "inputTruncated": input_summary.truncated,
                            "target": target,
                            "reason": reason,
                            "attribution": attribution.to_json(),
                            "governanceSnapshot": governance_snapshot,
                        }],
                    }),
                ))
            }
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
                let target = rd_runtime_tool_target(&tool_name, &input);
                let attribution = self.attribution_for_tool(&tool_name, target.as_deref(), &input);
                let reason = attribution.reason.as_deref().map_or_else(
                    || rd_runtime_tool_reason(&tool_name, target.as_deref(), &input),
                    ToOwned::to_owned,
                );
                let governance_snapshot = self.observe_tool_result(
                    index,
                    &tool_name,
                    target.as_deref(),
                    &input_summary.hash,
                    &output,
                    is_error,
                    &attribution,
                );
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
                        "target": target.clone(),
                        "reason": reason.clone(),
                        "attribution": attribution.to_json(),
                        "governanceSnapshot": governance_snapshot.clone(),
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
                            "target": target,
                            "reason": reason,
                            "attribution": attribution.to_json(),
                            "governanceSnapshot": governance_snapshot,
                        }],
                    }),
                ))
            }
            other => rd_runtime_event_to_task_event(other),
        }
    }

    fn attribution_for_tool(
        &self,
        tool_name: &str,
        target: Option<&str>,
        input: &str,
    ) -> RdRuntimeToolAttribution {
        let Some(target) = target else {
            return RdRuntimeToolAttribution::fallback();
        };
        if tool_name != "read_file" {
            return RdRuntimeToolAttribution::fallback();
        }
        let normalized = normalize_rd_runtime_attribution_path(target);
        if normalized.is_empty() {
            return RdRuntimeToolAttribution::fallback();
        }
        if self.attribution.explicit_files.contains(&normalized) {
            return RdRuntimeToolAttribution {
                matched: true,
                reason: Some("用户在问题中显式 @ 了该文件，因此优先读取核对。".to_string()),
                sources: vec!["explicit_file".to_string()],
                score: None,
                rank: None,
                notes: Vec::new(),
            };
        }
        if let Some(evidence) = self.attribution.files.get(&normalized) {
            let reason = evidence
                .reason
                .clone()
                .unwrap_or_else(|| rd_runtime_retrieval_reason_from_sources(&evidence.sources));
            return RdRuntimeToolAttribution {
                matched: true,
                reason: Some(reason),
                sources: evidence.sources.iter().cloned().collect(),
                score: Some(evidence.score),
                rank: evidence.rank,
                notes: evidence.notes.iter().take(4).cloned().collect(),
            };
        }
        let fallback_reason = rd_runtime_tool_reason(tool_name, Some(target), input);
        RdRuntimeToolAttribution {
            matched: false,
            reason: Some(fallback_reason),
            sources: Vec::new(),
            score: None,
            rank: None,
            notes: Vec::new(),
        }
    }

    fn observe_tool_input(
        &mut self,
        index: u32,
        tool_name: &str,
        target: Option<&str>,
        input_hash: &str,
        attribution: &RdRuntimeToolAttribution,
    ) -> Value {
        if self.observed_inputs.insert(index) {
            *self.tool_counts.entry(tool_name.to_string()).or_default() += 1;
            if let Some(target) = target.filter(|value| !value.trim().is_empty()) {
                *self
                    .target_counts
                    .entry(format!("{tool_name}::{target}"))
                    .or_default() += 1;
                if tool_name == "read_file" && !attribution.matched {
                    self.unattributed_read_targets.insert(target.to_string());
                }
            }
            if !input_hash.trim().is_empty() {
                *self
                    .input_hash_counts
                    .entry(format!("{tool_name}::{input_hash}"))
                    .or_default() += 1;
            }
        }
        self.snapshot(
            tool_name,
            target,
            Some(input_hash),
            None,
            false,
            attribution,
        )
    }

    fn observe_tool_result(
        &mut self,
        index: u32,
        tool_name: &str,
        target: Option<&str>,
        input_hash: &str,
        output: &str,
        is_error: bool,
        attribution: &RdRuntimeToolAttribution,
    ) -> Value {
        if !self.observed_inputs.contains(&index) {
            let _ = self.observe_tool_input(index, tool_name, target, input_hash, attribution);
        }
        let output_signal = rd_runtime_tool_output_signal(tool_name, output, is_error);
        if self.observed_results.insert(index) {
            if is_error {
                if let Some(target) = target.filter(|value| !value.trim().is_empty()) {
                    *self
                        .failed_target_counts
                        .entry(format!("{tool_name}::{target}"))
                        .or_default() += 1;
                }
                if !input_hash.trim().is_empty() {
                    *self
                        .failed_input_hash_counts
                        .entry(format!("{tool_name}::{input_hash}"))
                        .or_default() += 1;
                }
            }
            if output_signal == Some("empty_result") {
                *self
                    .empty_result_counts
                    .entry(tool_name.to_string())
                    .or_default() += 1;
            }
        }
        self.snapshot(
            tool_name,
            target,
            Some(input_hash),
            output_signal,
            is_error,
            attribution,
        )
    }

    fn snapshot(
        &self,
        tool_name: &str,
        target: Option<&str>,
        input_hash: Option<&str>,
        output_signal: Option<&'static str>,
        is_error: bool,
        attribution: &RdRuntimeToolAttribution,
    ) -> Value {
        let read_file_count = *self.tool_counts.get("read_file").unwrap_or(&0);
        let grep_count = *self.tool_counts.get("grep_search").unwrap_or(&0);
        let glob_count = *self.tool_counts.get("glob_search").unwrap_or(&0);
        let search_count = grep_count.saturating_add(glob_count);
        let input_repeat_count = input_hash
            .filter(|hash| !hash.trim().is_empty())
            .map(|hash| {
                self.input_hash_counts
                    .get(&format!("{tool_name}::{hash}"))
                    .copied()
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let repeat_count = target
            .map(|target| {
                self.target_counts
                    .get(&format!("{tool_name}::{target}"))
                    .copied()
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let repeated_target = repeat_count > 1;
        let repeated_input = input_repeat_count > 1;
        let failed_target_count = target
            .map(|target| {
                self.failed_target_counts
                    .get(&format!("{tool_name}::{target}"))
                    .copied()
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let failed_input_count = input_hash
            .filter(|hash| !hash.trim().is_empty())
            .map(|hash| {
                self.failed_input_hash_counts
                    .get(&format!("{tool_name}::{hash}"))
                    .copied()
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let empty_result_count = *self.empty_result_counts.get(tool_name).unwrap_or(&0);
        let unattributed_read_count = self.unattributed_read_targets.len();

        let (
            suggested_read,
            suggested_search,
            soft_read,
            soft_search,
            over_read,
            over_search,
            deep_mode_recommended,
            plan_json,
        ) = self
            .plan
            .as_ref()
            .map_or((0, 0, 0, 0, false, false, false, Value::Null), |plan| {
                let soft_read = plan.soft_read_threshold();
                let soft_search = plan.soft_search_threshold();
                let over_read = read_file_count > soft_read;
                let over_search = search_count > soft_search;
                (
                    plan.suggested_read_file_count,
                    plan.suggested_search_count,
                    soft_read,
                    soft_search,
                    over_read,
                    over_search,
                    (over_read || over_search) && !plan.should_deep_scan,
                    plan.to_json(),
                )
            });

        let mut recommendations = Vec::new();
        if repeated_target {
            recommendations.push(format!(
                "目标已重复访问 {repeat_count} 次，建议优先复用已读证据或说明复核原因。"
            ));
        }
        if over_read {
            recommendations.push(format!(
                "read_file 当前 {read_file_count} 次，已超过软阈值 {soft_read}；如需继续扩大，请说明证据缺口。"
            ));
        }
        if over_search {
            recommendations.push(format!(
                "grep/glob 当前 {search_count} 次，已超过软阈值 {soft_search}；建议先收敛候选模块。"
            ));
        }
        if repeated_input {
            recommendations.push(format!(
                "同一工具输入已重复 {input_repeat_count} 次；建议调整参数、复用已有输出，或说明为什么必须复核。"
            ));
        }
        if failed_target_count > 0 {
            recommendations.push(format!(
                "当前目标已有 {failed_target_count} 次失败；建议换搜索词/路径定位，不要连续重试同一路径。"
            ));
        }
        if output_signal == Some("empty_result") {
            recommendations.push(
                "本次搜索/读取结果为空；建议扩大到相邻目录、改用符号名/错误关键词，或回到召回候选。"
                    .to_string(),
            );
        }
        if tool_name == "read_file" && !attribution.matched {
            recommendations.push(
                "该读取未命中结构化召回证据；如果继续扩展，请在结论中说明证据缺口。".to_string(),
            );
        }
        let soft_feedback = rd_runtime_soft_feedback_json(
            tool_name,
            repeated_target,
            repeated_input,
            is_error,
            failed_target_count,
            failed_input_count,
            output_signal,
            tool_name == "read_file" && !attribution.matched,
            over_read,
            over_search,
            deep_mode_recommended,
            &recommendations,
        );
        let soft_level = soft_feedback
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("ok");
        let level = if deep_mode_recommended {
            "deep_mode_suggested"
        } else if soft_level == "correction_needed" {
            "correction_needed"
        } else if over_read || over_search {
            "expanded"
        } else if repeated_target || repeated_input || output_signal == Some("empty_result") {
            "watch"
        } else {
            "ok"
        };

        json!({
            "phase": "live",
            "level": level,
            "plan": plan_json,
            "toolName": tool_name,
            "target": target,
            "readFileCount": read_file_count,
            "grepSearchCount": grep_count,
            "globSearchCount": glob_count,
            "searchCount": search_count,
            "suggestedReadFileCount": suggested_read,
            "suggestedSearchCount": suggested_search,
            "softReadThreshold": soft_read,
            "softSearchThreshold": soft_search,
            "overSuggestedReadFile": over_read,
            "overSuggestedSearch": over_search,
            "deepModeRecommended": deep_mode_recommended,
            "repeatedTarget": repeated_target,
            "repeatedInput": repeated_input,
            "targetVisitCount": repeat_count,
            "inputRepeatCount": input_repeat_count,
            "failedTargetCount": failed_target_count,
            "failedInputCount": failed_input_count,
            "emptyResultCount": empty_result_count,
            "unattributedReadCount": unattributed_read_count,
            "outputSignal": output_signal,
            "softFeedback": soft_feedback,
            "effectFirst": true,
            "blocking": false,
            "recommendations": recommendations,
        })
    }
}
