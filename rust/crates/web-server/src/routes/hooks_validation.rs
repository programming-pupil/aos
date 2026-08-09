//! Hook code validation — syntax checking and security scanning.
//!
//! Performs:
//! - Syntax validation: `python3 -m py_compile` for Python, `bash -n` for Shell
//! - Security scan: detects dangerous function calls and suspicious patterns

use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Deserialize)]
pub struct HookValidationRequest {
    pub code: String,
    pub language: String,
}

#[derive(Debug, Serialize)]
pub struct HookValidationResponse {
    pub valid: bool,
    pub errors: Vec<HookValidationError>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HookValidationError {
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
}

pub fn validate_hook_code(code: &str, language: &str) -> HookValidationResponse {
    if code.trim().is_empty() {
        return HookValidationResponse {
            valid: false,
            errors: vec![HookValidationError {
                line: None,
                column: None,
                message: "Code cannot be empty".to_string(),
            }],
            warnings: vec![],
        };
    }

    let warnings = scan_security(code, language);
    let syntax_result = check_syntax(code, language);

    // valid means syntax is correct; security warnings are informational, not blockers
    let valid = syntax_result.is_empty();

    HookValidationResponse {
        valid,
        errors: syntax_result,
        warnings,
    }
}

fn check_syntax(code: &str, language: &str) -> Vec<HookValidationError> {
    match language {
        "python" => check_python_syntax(code),
        "shell" | "bash" | "sh" => check_shell_syntax(code),
        _ => {
            vec![HookValidationError {
                line: None,
                column: None,
                message: format!("Unknown language: '{language}'. Supported: python, shell."),
            }]
        }
    }
}

fn check_python_syntax(code: &str) -> Vec<HookValidationError> {
    let tmp_dir = std::env::temp_dir();
    let file_name = format!("hook_check_{}.py", uuid::Uuid::new_v4());
    let file_path = tmp_dir.join(&file_name);

    let write_result = std::fs::write(&file_path, code);
    if let Err(e) = write_result {
        return vec![HookValidationError {
            line: None,
            column: None,
            message: format!("Failed to write temp file: {e}"),
        }];
    }

    let Some(file_path_str) = file_path.to_str() else {
        let _ = std::fs::remove_file(&file_path);
        return vec![HookValidationError {
            line: None,
            column: None,
            message: "Temporary file path is not valid UTF-8".to_string(),
        }];
    };

    let output = Command::new("python3")
        .args(["-m", "py_compile", file_path_str])
        .output();

    let _ = std::fs::remove_file(&file_path);

    match output {
        Ok(output) if output.status.success() => vec![],
        Ok(output) => parse_python_errors(&output.stderr),
        Err(e) => vec![HookValidationError {
            line: None,
            column: None,
            message: format!("Failed to run python3: {e}"),
        }],
    }
}

fn parse_python_errors(stderr: &[u8]) -> Vec<HookValidationError> {
    let stderr_str = String::from_utf8_lossy(stderr);
    let mut errors = Vec::new();

    for line in stderr_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "Python" || trimmed.starts_with("Python ") {
            continue;
        }

        // Parse "  File "/path/to/file.py", line N" traceback line
        if trimmed.starts_with("  File ") && trimmed.contains(", line ") {
            if let Some(rest) = trimmed.strip_prefix("  File ") {
                if let Some((_path_part, rest_part)) = rest.split_once(", line ") {
                    if let Some((line_str, _)) = rest_part.split_once(' ') {
                        if let Ok(line_num) = line_str.parse::<u32>() {
                            errors.push(HookValidationError {
                                line: Some(line_num),
                                column: None,
                                message: trimmed.to_string(),
                            });
                            continue;
                        }
                    }
                }
            }
        }

        // Parse "  ^" carets (just skip, the error line is already pushed)
        if trimmed == "^" || trimmed.starts_with("    ") && !trimmed.contains(':') {
            continue;
        }

        // Parse "/path/to/file.py:line: error: message" or "/path/to/file.py:line: SyntaxError: ..."
        if trimmed.starts_with('/') || trimmed.starts_with("File ") {
            if let Some(col_pos) = trimmed.find(':') {
                let rest = &trimmed[col_pos + 1..];
                if let Some((line_str, msg)) = rest.split_once(':') {
                    if let Ok(line_num) = line_str.trim().parse::<u32>() {
                        errors.push(HookValidationError {
                            line: Some(line_num),
                            column: None,
                            message: msg.trim().to_string(),
                        });
                        continue;
                    }
                }
            }
        }

        // Parse "SyntaxError: ..." or "IndentationError: ..." alone on a line
        if trimmed.starts_with("SyntaxError")
            || trimmed.starts_with("IndentationError")
            || trimmed.starts_with("TabError")
        {
            errors.push(HookValidationError {
                line: None,
                column: None,
                message: trimmed.to_string(),
            });
            continue;
        }

        // Skip blank or purely decorative lines
        if trimmed.is_empty() || trimmed == "^" {
            continue;
        }

        errors.push(HookValidationError {
            line: None,
            column: None,
            message: trimmed.to_string(),
        });
    }

    if errors.is_empty() {
        errors.push(HookValidationError {
            line: None,
            column: None,
            message: stderr_str.to_string(),
        });
    }

    errors
}

fn check_shell_syntax(code: &str) -> Vec<HookValidationError> {
    let tmp_dir = std::env::temp_dir();
    let file_name = format!("hook_check_{}.sh", uuid::Uuid::new_v4());
    let file_path = tmp_dir.join(&file_name);

    let write_result = std::fs::write(&file_path, code);
    if let Err(e) = write_result {
        return vec![HookValidationError {
            line: None,
            column: None,
            message: format!("Failed to write temp file: {e}"),
        }];
    }

    let Some(file_path_str) = file_path.to_str() else {
        let _ = std::fs::remove_file(&file_path);
        return vec![HookValidationError {
            line: None,
            column: None,
            message: "Temporary file path is not valid UTF-8".to_string(),
        }];
    };

    let output = Command::new("bash").args(["-n", file_path_str]).output();

    let _ = std::fs::remove_file(&file_path);

    match output {
        Ok(output) if output.status.success() => vec![],
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim().is_empty() {
                vec![HookValidationError {
                    line: None,
                    column: None,
                    message: "Shell syntax error".to_string(),
                }]
            } else {
                stderr
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| {
                        let (line, msg) = parse_shell_error_line(l);
                        HookValidationError {
                            line,
                            column: None,
                            message: msg,
                        }
                    })
                    .collect()
            }
        }
        Err(e) => vec![HookValidationError {
            line: None,
            column: None,
            message: format!("Failed to run bash: {e}"),
        }],
    }
}

fn parse_shell_error_line(line: &str) -> (Option<u32>, String) {
    let trimmed = line.trim();
    // Try to extract line number: "-c: line 5: ..."
    if let Some(rest) = trimmed.find(": line ") {
        let before = &trimmed[..rest];
        if let Some(line_str) = before.strip_suffix(":") {
            if let Ok(num) = line_str.parse::<u32>() {
                let msg = trimmed[rest + 8..].trim_start().to_string();
                return (Some(num), msg);
            }
        }
    }
    (None, trimmed.to_string())
}

#[allow(clippy::too_many_lines)]
fn scan_security(code: &str, language: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    let patterns: &[(&str, &str)] = match language {
        "python" => &[
            (
                "os.system",
                "dangerous: os.system allows arbitrary shell commands",
            ),
            (
                "subprocess",
                "dangerous: subprocess module allows arbitrary process execution",
            ),
            ("eval(", "dangerous: eval() allows arbitrary code execution"),
            ("exec(", "dangerous: exec() allows arbitrary code execution"),
            (
                "__import__",
                "dangerous: __import__() allows dynamic module loading",
            ),
            (
                "open(",
                "file I/O: open() is allowed but consider restricting to specific paths",
            ),
            (
                "requests.",
                "network access: requests library can make outbound HTTP calls",
            ),
            (
                "urllib",
                "network access: urllib can make outbound HTTP calls",
            ),
            (
                "socket",
                "network access: socket allows low-level network connections",
            ),
            (
                "os.popen",
                "dangerous: os.popen allows shell command execution",
            ),
            (
                "pty.spawn",
                "dangerous: pty.spawn allows pseudo-terminal creation",
            ),
            (
                "os.remove",
                "file deletion: os.remove permanently deletes files",
            ),
            (
                "os.unlink",
                "file deletion: os.unlink permanently deletes files",
            ),
            (
                "shutil.rmtree",
                "directory deletion: shutil.rmtree recursively removes directories",
            ),
            (
                "os.chmod",
                "permission change: os.chmod modifies file permissions",
            ),
            (
                "os.chown",
                "ownership change: os.chown modifies file ownership",
            ),
            (
                "os.kill",
                "process control: os.kill sends signals to processes",
            ),
            (
                "os.rename",
                "file rename/move: os.rename changes file locations",
            ),
            (
                "signal(",
                "signal handling: signal module intercepts system signals",
            ),
            (
                "ctypes.",
                "dangerous: ctypes allows direct memory manipulation",
            ),
        ],
        "shell" | "bash" | "sh" => &[
            (
                "curl.*\\|.*sh",
                "dangerous: piping curl/wget output to shell allows remote code execution",
            ),
            (
                "wget.*\\|.*sh",
                "dangerous: piping wget output to shell allows remote code execution",
            ),
            (
                "eval.*\\$",
                "dangerous: eval with variable expansion allows arbitrary code execution",
            ),
            (
                "exec\\s+",
                "process replacement: exec replaces current shell process",
            ),
            (">/dev/sd", "dangerous: direct device access"),
            ("dd\\s+if=", "dangerous: dd with raw device access"),
            ("mkfs", "dangerous: mkfs formats filesystems"),
            (":(){ :|:& };:", "dangerous: fork bomb pattern detected"),
            (
                "nohup.*&",
                "background execution: nohup with backgrounding detaches from terminal",
            ),
        ],
        _ => &[],
    };

    let code_lower = code.to_lowercase();

    for (pattern, message) in patterns {
        if *pattern == "curl.*\\|.*sh" || *pattern == "wget.*\\|.*sh" {
            if regex_lite(pattern, &code_lower) {
                warnings.push(message.to_string());
            }
        } else if *pattern == "eval.*\\$" {
            if code_lower.contains("eval ") && code_lower.contains('$') {
                warnings.push(message.to_string());
            }
        } else if code_lower.contains(*pattern) {
            warnings.push(message.to_string());
        }
    }

    warnings
}

/// Minimal regex-lite pattern matching for shell security patterns
fn regex_lite(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split(".*").collect();
    if parts.len() == 2 {
        let before = parts[0].to_lowercase();
        let after = parts[1].to_lowercase();
        return text.contains(&before) && text.contains(&after);
    }
    text.contains(&pattern.to_lowercase())
}
