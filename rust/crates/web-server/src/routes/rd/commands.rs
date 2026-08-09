//! Test-command execution, output summarization, and candidate fix prompts.

use super::*;

pub(super) fn summarize_rd_test_output_for_prompt(
    stdout: &str,
    stderr: &str,
    max_chars: usize,
) -> String {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for (source, text) in [("STDERR", stderr), ("STDOUT", stdout)] {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if rd_test_output_line_is_signal(trimmed) {
                let item = format!("{source}: {trimmed}");
                if seen.insert(item.clone()) {
                    selected.push(item);
                }
            }
            if selected.len() >= 120 {
                break;
            }
        }
    }

    if selected.is_empty() {
        selected.extend(
            stderr
                .lines()
                .rev()
                .take(80)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|line| format!("STDERR: {}", line.trim()))
                .filter(|line| line.trim() != "STDERR:"),
        );
        selected.extend(
            stdout
                .lines()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|line| format!("STDOUT: {}", line.trim()))
                .filter(|line| line.trim() != "STDOUT:"),
        );
    }

    let summary = selected.join("\n");
    let header = format!(
        "original_stdout_chars={} original_stderr_chars={} selected_lines={}\n",
        stdout.chars().count(),
        stderr.chars().count(),
        selected.len()
    );
    truncate_text(&(header + &summary), max_chars)
}

fn rd_test_output_line_is_signal(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "exception",
        "traceback",
        "assert",
        "expected",
        "actual",
        "mismatch",
        "timeout",
        "cannot find",
        "not found",
        "undefined",
        "unresolved",
        "stack backtrace",
        "caused by",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || looks_like_file_line_reference(line)
}

fn looks_like_file_line_reference(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes
        .windows(2)
        .any(|pair| pair[0] == b':' && pair[1].is_ascii_digit())
        && (line.contains(".rs")
            || line.contains(".ts")
            || line.contains(".tsx")
            || line.contains(".js")
            || line.contains(".jsx")
            || line.contains(".java")
            || line.contains(".go")
            || line.contains(".py")
            || line.contains(".sql"))
}

/// Outcome of running a shell command inside a candidate worktree.
pub(super) struct CommandRunResult {
    pub(super) status: String,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout_text: String,
    pub(super) stderr_text: String,
}

/// Run a shell command inside `dir`, honouring the configured test-command
/// timeout. Used to verify candidate-worktree edits. The command is the repo's
/// own test command (already validated against the dangerous-command denylist
/// before being persisted/used).
pub(super) async fn run_command_in_dir(dir: &Path, command: &str) -> CommandRunResult {
    let timeout_secs = rd_test_command_timeout_secs();
    let output_result = timeout(
        Duration::from_secs(timeout_secs),
        tokio::process::Command::new("sh")
            .arg("-lc")
            .arg(command)
            .current_dir(dir)
            .output(),
    )
    .await;
    match output_result {
        Ok(Ok(output)) => CommandRunResult {
            status: if output.status.success() {
                "passed".to_string()
            } else {
                "failed".to_string()
            },
            exit_code: output.status.code(),
            stdout_text: truncate_text(String::from_utf8_lossy(&output.stdout).as_ref(), 60_000),
            stderr_text: truncate_text(String::from_utf8_lossy(&output.stderr).as_ref(), 60_000),
        },
        Ok(Err(err)) => CommandRunResult {
            status: "failed".to_string(),
            exit_code: None,
            stdout_text: String::new(),
            stderr_text: err.to_string(),
        },
        Err(_) => CommandRunResult {
            status: "timeout".to_string(),
            exit_code: None,
            stdout_text: String::new(),
            stderr_text: format!("test command timed out after {timeout_secs}s"),
        },
    }
}

pub(super) async fn run_command_in_dir_with_agent_runtime(
    state: &AppState,
    tenant_id: &str,
    runtime_session_id: &str,
    dir: &Path,
    command: &str,
) -> CommandRunResult {
    let timeout_secs = rd_test_command_timeout_secs();
    match crate::routes::agent_runtime::run_runtime_command(
        state,
        crate::routes::agent_runtime::RuntimeCommandInput {
            tenant_id: tenant_id.to_string(),
            runtime_session_id: runtime_session_id.to_string(),
            agent_task_id: None,
            command: command.to_string(),
            cwd: dir.to_path_buf(),
            timeout_secs,
        },
    )
    .await
    {
        Ok(output) => CommandRunResult {
            status: if output.status
                == crate::routes::agent_runtime::RUNTIME_PROCESS_STATUS_COMPLETED
            {
                "passed".to_string()
            } else if output.status
                == crate::routes::agent_runtime::RUNTIME_PROCESS_STATUS_TIMED_OUT
            {
                "timeout".to_string()
            } else if output.status
                == crate::routes::agent_runtime::RUNTIME_PROCESS_STATUS_CANCELLED
            {
                "cancelled".to_string()
            } else {
                "failed".to_string()
            },
            exit_code: output.exit_code,
            stdout_text: output.stdout_text,
            stderr_text: output.stderr_text,
        },
        Err(error) => CommandRunResult {
            status: "failed".to_string(),
            exit_code: None,
            stdout_text: String::new(),
            stderr_text: error.to_string(),
        },
    }
}

/// Resolve the repository's configured test command for candidate verification.
/// Returns `None` when no command is configured or when it fails the
/// dangerous-command safety check (in which case verification is skipped rather
/// than risking a destructive command in the worktree).
pub(super) async fn resolve_rd_test_command(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
) -> Option<String> {
    let setting = load_repo_setting(&state.db, &claims.tenant_id, &claims.sub, repository_id)
        .await
        .ok()
        .flatten();
    let command = setting.and_then(|(test_command, _)| test_command)?;
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    if reject_dangerous_command(trimmed).is_err() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Build the follow-up prompt that feeds a failing test run back to the
/// candidate-worktree agent so it can fix its own changes in place.
pub(super) fn build_rd_candidate_fix_prompt(
    test_command: &str,
    verify: &CommandRunResult,
) -> String {
    let output_digest =
        summarize_rd_test_output_for_prompt(&verify.stdout_text, &verify.stderr_text, 10_000);
    format!(
        "你刚才在候选工作区做的修改没有通过测试命令。请基于下面的关键输出摘要，继续使用 read_file/grep_search/glob_search 阅读相关文件，并用 edit_file/write_file 在当前候选工作区直接修复问题，不要回退到只输出 diff。\n\n## 测试命令\n{command}\n\n## 状态\n{status}（exit_code={exit_code}）\n\n## 关键输出摘要\n{output_digest}\n\n修复要求：\n- 先定位失败根因，再做最小必要修改。\n- 不要声称已修复而不实际改文件；AOS 会重新运行同一测试命令验证。\n- 如果摘要不足，优先读取摘要中提到的文件/行号以及相关调用链。\n- 如果失败与本次任务无关（例如预先存在的失败用例），请在 answerMd 中说明并保持改动最小。\n- 完成后输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string|null,\"prTitle\":string|null,\"prDescription\":string|null,\"unifiedDiff\":null,\"touchedFiles\":array}}。",
        command = test_command,
        status = verify.status,
        exit_code = verify
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
    )
}

pub(super) fn rd_test_command_timeout_secs() -> u64 {
    std::env::var("AOS_RD_TEST_COMMAND_TIMEOUT_SECS")
        .or_else(|_| std::env::var("RD_CODE_TEST_COMMAND_TIMEOUT_SECS"))
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| (10..=7_200).contains(v))
        .unwrap_or(180)
}
