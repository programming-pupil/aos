//! Runtime tool governance summaries and target/reason inference.

use super::*;

pub(super) fn summarize_rd_tool_calls(tool_calls: &[agent_gateway::ToolCallRecord]) -> Vec<Value> {
    tool_calls
        .iter()
        .take(50)
        .map(|call| {
            let input_summary = summarize_rd_tool_text(&call.input, 900);
            let output_summary = summarize_rd_tool_text(&call.output, 1_200);
            json!({
                "index": call.index,
                "toolName": call.tool_name,
                "source": call.source,
                "sourceName": call.source_name,
                "isError": call.is_error,
                "durationMs": call.duration_ms,
                "input": input_summary.text,
                "output": output_summary.text,
                "inputChars": input_summary.original_chars,
                "outputChars": output_summary.original_chars,
                "inputHash": input_summary.hash,
                "outputHash": output_summary.hash,
                "inputTruncated": input_summary.truncated,
                "outputTruncated": output_summary.truncated,
                "dedupedOutputLines": output_summary.deduped_lines,
            })
        })
        .collect()
}

pub(super) fn build_rd_runtime_tool_governance(
    plan: &RdRuntimeToolGovernancePlan,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> Value {
    let mut tool_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut target_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut target_tools: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut input_hash_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut input_hash_targets: BTreeMap<String, String> = BTreeMap::new();
    let mut failed_target_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut failed_input_hash_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut empty_result_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut failed_tool_calls = 0usize;
    let mut total_duration_ms = 0u64;
    let mut slow_tools = Vec::new();

    for call in tool_calls {
        *tool_counts.entry(call.tool_name.clone()).or_default() += 1;
        let input_hash = stable_hash_hex(&call.input);
        let input_key = format!("{}::{input_hash}", call.tool_name);
        *input_hash_counts.entry(input_key.clone()).or_default() += 1;
        let target = rd_runtime_tool_target(&call.tool_name, &call.input);
        if let Some(target) = target.as_deref() {
            input_hash_targets
                .entry(input_key.clone())
                .or_insert_with(|| truncate_text(target, 240));
        }
        if call.is_error {
            failed_tool_calls += 1;
            *failed_input_hash_counts.entry(input_key).or_default() += 1;
        }
        if rd_runtime_tool_output_signal(&call.tool_name, &call.output, call.is_error)
            == Some("empty_result")
        {
            *empty_result_counts
                .entry(call.tool_name.clone())
                .or_default() += 1;
        }
        total_duration_ms = total_duration_ms.saturating_add(call.duration_ms);
        if call.duration_ms >= 10_000 {
            slow_tools.push(json!({
                "index": call.index,
                "toolName": &call.tool_name,
                "durationMs": call.duration_ms,
                "target": target.clone(),
            }));
        }
        if let Some(target) = target {
            let key = format!("{}::{target}", call.tool_name);
            *target_counts.entry(key.clone()).or_default() += 1;
            if call.is_error {
                *failed_target_counts.entry(key.clone()).or_default() += 1;
            }
            target_tools
                .entry(key)
                .or_default()
                .insert(call.tool_name.clone());
        }
    }

    let read_file_count = *tool_counts.get("read_file").unwrap_or(&0);
    let grep_count = *tool_counts.get("grep_search").unwrap_or(&0);
    let glob_count = *tool_counts.get("glob_search").unwrap_or(&0);
    let search_count = grep_count.saturating_add(glob_count);
    let over_suggested_read = read_file_count > plan.soft_read_threshold();
    let over_suggested_search = search_count > plan.soft_search_threshold();
    let repeated_targets = target_counts
        .iter()
        .filter_map(|(key, count)| {
            (*count > 1).then(|| {
                let target = key
                    .split_once("::")
                    .map(|(_, target)| target)
                    .unwrap_or(key);
                json!({
                    "target": target,
                    "count": *count,
                    "tools": target_tools
                        .get(key)
                        .map(|tools| tools.iter().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                })
            })
        })
        .take(12)
        .collect::<Vec<_>>();
    let repeated_inputs = input_hash_counts
        .iter()
        .filter_map(|(key, count)| {
            (*count > 1).then(|| {
                let (tool_name, input_hash) = key.split_once("::").unwrap_or((key, ""));
                json!({
                    "toolName": tool_name,
                    "inputHash": input_hash,
                    "target": input_hash_targets.get(key).cloned(),
                    "count": *count,
                })
            })
        })
        .take(12)
        .collect::<Vec<_>>();
    let failed_targets = failed_target_counts
        .iter()
        .filter_map(|(key, count)| {
            (*count > 0).then(|| {
                let (tool_name, target) = key.split_once("::").unwrap_or(("", key));
                json!({
                    "toolName": tool_name,
                    "target": target,
                    "count": *count,
                })
            })
        })
        .take(12)
        .collect::<Vec<_>>();
    let empty_results = empty_result_counts
        .iter()
        .map(|(tool_name, count)| {
            json!({
                "toolName": tool_name,
                "count": *count,
            })
        })
        .take(8)
        .collect::<Vec<_>>();

    let mut recommendations = Vec::new();
    if over_suggested_read {
        recommendations.push(format!(
            "read_file 已调用 {read_file_count} 次，超过当前上下文计划软阈值 {}（建议值 {}）；如果仍需扩大范围，请在结论中说明证据缺口并建议/确认更深模式。",
            plan.soft_read_threshold(),
            plan.suggested_read_file_count,
        ));
    }
    if over_suggested_search {
        recommendations.push(format!(
            "grep/glob 已调用 {search_count} 次，超过当前上下文计划软阈值 {}（建议值 {}）；建议优先收敛候选模块，必要时进入深度审计。",
            plan.soft_search_threshold(),
            plan.suggested_search_count,
        ));
    }
    if !repeated_targets.is_empty() {
        recommendations.push(
            "检测到重复读取/搜索同一目标；后续应优先复用已读证据或说明为什么需要复核。".to_string(),
        );
    }
    if !repeated_inputs.is_empty() {
        recommendations.push(
            "检测到完全相同的工具输入重复出现；后续应调整参数、复用结果，避免原样重试。"
                .to_string(),
        );
    }
    if failed_tool_calls > 0 {
        recommendations.push(format!(
            "有 {failed_tool_calls} 次工具调用失败；后续应优先根据失败原因调整路径/搜索词，而不是重复尝试相同输入。"
        ));
    }
    if !empty_results.is_empty() {
        recommendations.push(
            "检测到空搜索/空读取结果；建议改用符号名、错误关键词或相邻目录继续定位。".to_string(),
        );
    }
    let deep_mode_recommended =
        (over_suggested_read || over_suggested_search) && !plan.should_deep_scan;
    let max_failed_target_count = failed_target_counts
        .values()
        .copied()
        .max()
        .unwrap_or_default();
    let max_failed_input_count = failed_input_hash_counts
        .values()
        .copied()
        .max()
        .unwrap_or_default();
    let soft_feedback = rd_runtime_soft_feedback_json(
        "runtime_summary",
        !repeated_targets.is_empty(),
        !repeated_inputs.is_empty(),
        failed_tool_calls > 0,
        max_failed_target_count,
        max_failed_input_count,
        (!empty_results.is_empty()).then_some("empty_result"),
        false,
        over_suggested_read,
        over_suggested_search,
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
    } else if over_suggested_read || over_suggested_search {
        "expanded"
    } else if !repeated_targets.is_empty()
        || !repeated_inputs.is_empty()
        || failed_tool_calls > 0
        || !empty_results.is_empty()
    {
        "watch"
    } else {
        "ok"
    };

    json!({
        "level": level,
        "recommendationLevel": level,
        "profile": plan.profile.as_str(),
        "profileName": plan.profile.display_name(),
        "depth": &plan.depth,
        "shouldDeepScan": plan.should_deep_scan,
        "effectFirst": true,
        "blocking": false,
        "plan": plan.to_json(),
        "toolCallCount": tool_calls.len(),
        "failedToolCalls": failed_tool_calls,
        "totalDurationMs": total_duration_ms,
        "suggestedReadFileCount": plan.suggested_read_file_count,
        "suggestedSearchCount": plan.suggested_search_count,
        "softReadThreshold": plan.soft_read_threshold(),
        "softSearchThreshold": plan.soft_search_threshold(),
        "overSuggestedReadFile": over_suggested_read,
        "overSuggestedSearch": over_suggested_search,
        "deepModeRecommended": deep_mode_recommended,
        "readFileCount": read_file_count,
        "grepSearchCount": grep_count,
        "globSearchCount": glob_count,
        "searchCount": search_count,
        "toolCounts": tool_counts,
        "repeatedTargets": repeated_targets,
        "repeatedInputs": repeated_inputs,
        "failedTargets": failed_targets,
        "emptyResults": empty_results,
        "slowTools": slow_tools.into_iter().take(8).collect::<Vec<_>>(),
        "softFeedback": soft_feedback,
        "recommendations": recommendations,
    })
}

pub(super) fn build_rd_runtime_tool_governance_plan(
    mode: &str,
    context_strategy: Option<&RdTaskContextStrategy>,
) -> RdRuntimeToolGovernancePlan {
    let strategy = context_strategy.cloned().unwrap_or_else(|| {
        let profile = match mode {
            "modify" => RdContextProfile::Modify,
            "review" => RdContextProfile::Review,
            "explain" => RdContextProfile::Explain,
            _ => RdContextProfile::FocusedAsk,
        };
        RdTaskContextStrategy {
            profile,
            depth: default_rd_context_depth(profile).to_string(),
            should_deep_scan: profile == RdContextProfile::DeepReview,
        }
    });
    let profile = normalize_rd_profile_for_mode(strategy.profile, mode);
    let depth =
        normalize_rd_context_depth(Some(&strategy.depth), profile, strategy.should_deep_scan);
    let should_deep_scan =
        strategy.should_deep_scan || depth == "deep" || profile == RdContextProfile::DeepReview;
    let budget = profile.budget();
    let mut suggested_read_file_count = budget.runtime_read_file_budget;
    let mut suggested_search_count = budget.runtime_search_budget;
    if should_deep_scan || depth == "deep" {
        suggested_read_file_count = suggested_read_file_count
            .saturating_mul(3)
            .saturating_add(1)
            / 2;
        suggested_search_count = suggested_search_count.saturating_mul(3).saturating_add(1) / 2;
    } else if depth == "shallow" {
        suggested_read_file_count = suggested_read_file_count.min(10);
        suggested_search_count = suggested_search_count.min(5);
    }
    let soft_limit_multiplier = if should_deep_scan || depth == "deep" {
        2.5
    } else if profile == RdContextProfile::Overview {
        2.0
    } else {
        2.2
    };
    let mut instructions = vec![
        "先使用已注入的仓库摘要、embedding/词法/symbol/import 召回和显式文件上下文建立候选范围。"
            .to_string(),
        "优先读取少量最相关真实文件核对事实；不要为了“更完整”重复读取同一目标。".to_string(),
        "如果证据已经足够回答或生成 Diff，应停止继续搜索并产出结果。".to_string(),
        "如果超过软阈值仍必须继续，请说明证据缺口、扩展范围和为什么这不会降低结论质量。"
            .to_string(),
    ];
    match profile {
        RdContextProfile::Overview => instructions.push(
            "项目概览/架构问题默认不逐文件扫描；重点回答项目目的、模块分层、入口、数据流和架构图。".to_string(),
        ),
        RdContextProfile::Modify => instructions.push(
            "修改代码时读取范围应围绕最小可变更文件集、接口边界、调用链和测试约束渐进扩大。".to_string(),
        ),
        RdContextProfile::Review | RdContextProfile::DeepReview => instructions.push(
            "审查任务优先高风险路径和文件/行级证据；低置信问题要标注覆盖范围，不要用海量读取替代判断。".to_string(),
        ),
        RdContextProfile::Explain => instructions.push(
            "报错解释优先围绕错误栈、日志关键词、失败命令和直接调用链定位，不扫描无关模块。".to_string(),
        ),
        RdContextProfile::FocusedAsk => instructions.push(
            "定向问答优先复用缓存摘要和召回结果，证据不足时列出还需读取的具体文件。".to_string(),
        ),
    }

    RdRuntimeToolGovernancePlan {
        profile,
        depth,
        should_deep_scan,
        suggested_read_file_count,
        suggested_search_count,
        soft_limit_multiplier,
        effect_first: true,
        blocking: false,
        strategy: "adaptive_progressive_context",
        instructions,
    }
}

pub(super) fn rd_runtime_tool_target(tool_name: &str, input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(target) = rd_runtime_tool_target_from_json(tool_name, &value) {
            return Some(target);
        }
    }
    if trimmed.len() <= 160 && !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return Some(trimmed.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    None
}

pub(super) fn rd_runtime_tool_reason(tool_name: &str, target: Option<&str>, input: &str) -> String {
    let target_lower = target.unwrap_or_default().to_ascii_lowercase();
    match tool_name {
        "read_file" => {
            if rd_runtime_target_is_manifest_or_doc(&target_lower) {
                "读取 manifest/README/配置文件，用于确认项目定位、启动方式或依赖边界。".to_string()
            } else if rd_runtime_target_is_entrypoint(&target_lower) {
                "读取入口/路由/核心模块文件，用于核对架构和真实执行路径。".to_string()
            } else if rd_runtime_target_is_test(&target_lower) {
                "读取测试或用例文件，用于理解验证方式和失败边界。".to_string()
            } else {
                "读取真实文件核对索引召回、上下文摘要或模型假设，避免只凭摘要下结论。".to_string()
            }
        }
        "grep_search" => {
            let pattern = serde_json::from_str::<Value>(input)
                .ok()
                .and_then(|value| {
                    value
                        .get("pattern")
                        .or_else(|| value.get("query"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| target.unwrap_or_default().to_string());
            if pattern.trim().is_empty() {
                "使用文本搜索缩小候选模块范围。".to_string()
            } else {
                format!(
                    "按关键词/符号 `{}` 搜索候选文件，用于渐进定位相关代码。",
                    truncate_text(pattern.trim(), 120)
                )
            }
        }
        "glob_search" => {
            if rd_runtime_target_is_manifest_or_doc(&target_lower) {
                "按文件模式寻找 README/manifest/配置入口，用于建立项目地图。".to_string()
            } else {
                "按文件模式缩小候选文件集合，避免直接全仓逐文件读取。".to_string()
            }
        }
        "bash" | "shell" => {
            if target_lower.contains("test")
                || target_lower.contains("cargo test")
                || target_lower.contains("npm test")
                || target_lower.contains("pnpm test")
            {
                "运行测试命令验证修改或复现问题。".to_string()
            } else if target_lower.contains("build")
                || target_lower.contains("cargo check")
                || target_lower.contains("tsc")
            {
                "运行构建/类型检查命令验证项目状态。".to_string()
            } else {
                "运行只读或验证类命令获取仓库状态；危险写操作会被策略限制。".to_string()
            }
        }
        "rd_validate_diff" => "校验候选 Diff 是否可应用，避免输出不可落地的补丁。".to_string(),
        _ if tool_name.starts_with("mcp__") => {
            "调用 MCP 工具补充外部系统上下文或执行已配置能力。".to_string()
        }
        _ if tool_name.starts_with("skill__") => {
            "调用已启用 Skill，为当前研发任务补充专项能力。".to_string()
        }
        _ => "工具调用由 runtime 根据当前任务计划选择，用于补充证据或推进验证。".to_string(),
    }
}

fn rd_runtime_target_is_manifest_or_doc(target_lower: &str) -> bool {
    [
        "readme",
        "package.json",
        "cargo.toml",
        "pom.xml",
        "build.gradle",
        "go.mod",
        "pyproject.toml",
        "dockerfile",
        "docker-compose",
        "compose.yml",
        "tsconfig",
        "vite.config",
        "next.config",
    ]
    .iter()
    .any(|needle| target_lower.contains(needle))
}

fn rd_runtime_target_is_entrypoint(target_lower: &str) -> bool {
    [
        "main.",
        "lib.",
        "app.",
        "server.",
        "router",
        "route",
        "controller",
        "handler",
        "bootstrap",
        "index.",
        "mod.rs",
    ]
    .iter()
    .any(|needle| target_lower.contains(needle))
}

fn rd_runtime_target_is_test(target_lower: &str) -> bool {
    [
        "test",
        "tests/",
        "__tests__",
        ".spec.",
        ".test.",
        "fixtures",
        "mock",
    ]
    .iter()
    .any(|needle| target_lower.contains(needle))
}

fn rd_runtime_tool_target_from_json(tool_name: &str, value: &Value) -> Option<String> {
    let object = value.as_object()?;
    let keys: &[&str] = match tool_name {
        "read_file" => &["path", "file_path", "filePath", "target_path", "targetPath"],
        "grep_search" => &["pattern", "query", "path", "include", "glob"],
        "glob_search" => &["pattern", "glob", "path", "query"],
        "bash" | "shell" => &["command", "cmd"],
        _ => &[
            "path",
            "file_path",
            "filePath",
            "target",
            "query",
            "pattern",
            "command",
            "url",
        ],
    };
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| truncate_text(value, 240))
    })
}
