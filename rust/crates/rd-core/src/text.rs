use serde_json::Value;

#[derive(Debug)]
pub struct ParsedRdOutput {
    pub plan_md: String,
    pub answer_md: String,
    pub review_md: Option<String>,
    pub pr_title: Option<String>,
    pub pr_description: Option<String>,
    pub unified_diff: Option<String>,
    pub touched_files: Vec<String>,
}

pub fn normalize_mode(mode: Option<&str>) -> String {
    match mode.unwrap_or("ask").trim() {
        "modify" | "code" => "modify",
        "review" => "review",
        "explain" | "error" => "explain",
        _ => "ask",
    }
    .to_string()
}

pub fn derive_title(prompt: &str) -> String {
    let title = prompt.lines().next().unwrap_or(prompt).trim();
    let title = if title.chars().count() > 40 {
        format!("{}...", title.chars().take(40).collect::<String>())
    } else {
        title.to_string()
    };
    if title.is_empty() {
        "研发任务".to_string()
    } else {
        title
    }
}

pub fn build_rd_runtime_user_prompt(
    mode: &str,
    repository_id: Option<&str>,
    prompt: &str,
    governance_section: Option<&str>,
) -> String {
    let repo_hint = repository_id.map_or_else(
        || "当前任务未绑定仓库。".to_string(),
        |repo_id| {
            format!(
                "当前任务绑定 repositoryId={repo_id}，runtime 工作目录就是仓库根目录。请像 CLI 编程助手一样优先使用 glob_search/grep_search/read_file 定位和阅读真实文件；需要改代码时，先生成候选 unified diff，再使用 rd_validate_diff 校验可应用性并根据错误修正。"
            )
        },
    );
    let governance_section = governance_section
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(|section| format!("\n\n{section}"))
        .unwrap_or_default();
    let diff_contract = if mode == "modify" {
        "- 当前是代码修改任务：只在 unifiedDiff 字段输出可 git apply 的完整 diff，等待用户在 AOS 中确认应用；在最终输出前优先调用 rd_validate_diff 校验候选 diff，若校验失败必须读取相关文件并修正。".to_string()
    } else {
        "- 当前不是代码修改任务：unifiedDiff 必须为 null，touchedFiles 只允许表示你阅读/审查过的文件，绝不能把仓库已有未提交变更、工具看到的 git diff 或建议补丁归属为本任务产物。".to_string()
    };
    format!(
        "{prompt}\n\n## Runtime 执行约束\n{repo_hint}\n- 当前模式：{mode}\n- 你可以读取仓库、搜索符号、分析依赖和引用 MCP/Skills。\n- 默认采用渐进式读取：先用索引/摘要/搜索定位，再读取少量关键文件核对；不要把项目概览、架构说明、启动说明等轻量问题升级成全仓库逐文件扫描。\n- 如果证据已经足够回答，必须停止继续搜索；只有用户明确要求全量审计、深度巡检或所有风险时，才扩大到深度扫描。\n- 禁止直接修改文件、提交代码、删除文件、安装依赖或执行具有写入副作用的命令。\n{diff_contract}\n- 输出必须尽量保持 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string|null,\"prTitle\":string|null,\"prDescription\":string|null,\"unifiedDiff\":string|null,\"touchedFiles\":array}}。\n- 如果工具信息不足，明确说明缺口，不要假装已经执行。{governance_section}"
    )
}

pub fn rd_system_prompt(mode: &str) -> &'static str {
    match mode {
        "modify" => "你是严谨的代码修改 Agent。先给计划，再在 unifiedDiff 中输出可 git apply 的完整 unified diff。不要声称已经应用修改。JSON keys: planMd, answerMd, reviewMd, prTitle, prDescription, unifiedDiff, touchedFiles。",
        "review" => "你是代码审查 Agent。优先输出 findings，包含严重级别、文件路径、风险和建议。不要生成补丁；unifiedDiff 必须为 null。JSON keys: planMd, answerMd, reviewMd, prTitle, prDescription, unifiedDiff, touchedFiles。",
        "explain" => "你是报错解释 Agent。解释原因、定位文件、给出验证步骤。不要生成补丁；unifiedDiff 必须为 null。JSON keys: planMd, answerMd, reviewMd, prTitle, prDescription, unifiedDiff, touchedFiles。",
        _ => "你是代码库问答 Agent。基于仓库上下文回答，缺信息时明确说明。不要生成补丁；unifiedDiff 必须为 null。JSON keys: planMd, answerMd, reviewMd, prTitle, prDescription, unifiedDiff, touchedFiles。",
    }
}

pub fn parse_rd_output(raw: &str, mode: &str) -> ParsedRdOutput {
    if let Some(value) = parse_json_from_model_output(raw) {
        return ParsedRdOutput {
            plan_md: value
                .get("planMd")
                .or_else(|| value.get("plan_md"))
                .and_then(Value::as_str)
                .unwrap_or("已生成执行计划。")
                .to_string(),
            answer_md: value
                .get("answerMd")
                .or_else(|| value.get("answer_md"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| raw.trim())
                .to_string(),
            review_md: value
                .get("reviewMd")
                .or_else(|| value.get("review_md"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            pr_title: value
                .get("prTitle")
                .or_else(|| value.get("pr_title"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            pr_description: value
                .get("prDescription")
                .or_else(|| value.get("pr_description"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            unified_diff: value
                .get("unifiedDiff")
                .or_else(|| value.get("unified_diff"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            touched_files: value
                .get("touchedFiles")
                .or_else(|| value.get("touched_files"))
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        };
    }
    ParsedRdOutput {
        plan_md: "已生成研发任务结果。".to_string(),
        answer_md: raw.to_string(),
        review_md: (mode == "review").then(|| raw.to_string()),
        pr_title: None,
        pr_description: None,
        unified_diff: extract_diff(raw),
        touched_files: Vec::new(),
    }
}

pub fn parse_spec_output(raw: &str) -> (String, String, String, String) {
    if let Some(value) = parse_json_from_model_output(raw) {
        return (
            value
                .get("requirementsMd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            value
                .get("designMd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            value
                .get("tasksMd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            value
                .get("acceptanceMd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
    }
    (raw.to_string(), String::new(), String::new(), String::new())
}

pub fn truncate_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n...[truncated]", &text[..end])
}

pub fn extract_diff(raw: &str) -> Option<String> {
    if let Some(block) =
        extract_fenced_block(raw, Some("diff")).or_else(|| extract_fenced_block(raw, Some("patch")))
    {
        if looks_like_unified_diff(&block) {
            return Some(ensure_trailing_newline(block));
        }
    }
    if let Some(block) = extract_fenced_block(raw, None) {
        if looks_like_unified_diff(&block) {
            return Some(ensure_trailing_newline(block));
        }
    }

    let lines: Vec<&str> = raw.lines().collect();
    let start = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("diff --git ")
            || trimmed.starts_with("--- ")
            || trimmed.starts_with("*** Begin Patch")
    })?;
    let candidate = lines[start..].join("\n");
    if looks_like_unified_diff(&candidate) {
        return Some(ensure_trailing_newline(candidate));
    }
    None
}

pub fn infer_first_file_from_diff(diff: &str) -> Option<String> {
    diff.lines().find_map(|line| {
        line.strip_prefix("+++ b/")
            .or_else(|| line.strip_prefix("--- a/"))
            .map(|path| path.split_whitespace().next().unwrap_or(path))
            .map(ToOwned::to_owned)
    })
}

pub fn parse_json_from_model_output(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    let mut candidates = Vec::new();
    candidates.push(strip_wrapping_fence(trimmed).to_string());
    if let Some(block) = extract_fenced_block(raw, Some("json")) {
        candidates.push(block);
    }
    if let Some(block) = extract_fenced_block(raw, None) {
        candidates.push(block);
    }
    if let Some(block) = extract_first_json_object(raw) {
        candidates.push(block);
    }

    candidates
        .into_iter()
        .map(|candidate| candidate.trim().to_string())
        .filter(|candidate| !candidate.is_empty())
        .find_map(|candidate| serde_json::from_str::<Value>(&candidate).ok())
}

fn strip_wrapping_fence(value: &str) -> &str {
    let trimmed = value.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let after_lang = rest
        .strip_prefix("json")
        .or_else(|| rest.strip_prefix("JSON"))
        .unwrap_or(rest)
        .trim_start_matches(|ch: char| ch.is_whitespace());
    after_lang
        .strip_suffix("```")
        .map(str::trim)
        .unwrap_or(trimmed)
}

fn extract_fenced_block(raw: &str, language: Option<&str>) -> Option<String> {
    let mut lines = raw.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("```") {
            continue;
        }
        let fence_language = trimmed
            .trim_start_matches("```")
            .trim()
            .to_ascii_lowercase();
        let mut block = Vec::new();
        for block_line in lines.by_ref() {
            if block_line.trim_start().starts_with("```") {
                break;
            }
            block.push(block_line);
        }
        let matches_language = language
            .map(|expected| fence_language.starts_with(&expected.to_ascii_lowercase()))
            .unwrap_or(true);
        if matches_language {
            return Some(block.join("\n"));
        }
    }
    None
}

fn extract_first_json_object(raw: &str) -> Option<String> {
    let start = raw.char_indices().find(|(_, ch)| *ch == '{')?.0;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(raw[start..end].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn looks_like_unified_diff(value: &str) -> bool {
    value.contains("diff --git")
        || value.contains("*** Begin Patch")
        || value.contains("--- ") && value.contains("+++ ") && value.contains("@@")
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.ends_with('\n') {
        value.push('\n');
    }
    value
}
