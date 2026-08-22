use std::env;
use std::io;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::process::Command as TokioCommand;
use tokio::runtime::Builder;

use crate::sandbox::{
    build_linux_sandbox_command, resolve_sandbox_status_for_request, FilesystemIsolationMode,
    SandboxConfig, SandboxStatus,
};
use crate::ConfigLoader;

/// Input schema for the built-in bash execution tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BashCommandInput {
    pub command: String,
    pub timeout: Option<u64>,
    pub description: Option<String>,
    #[serde(rename = "run_in_background")]
    pub run_in_background: Option<bool>,
    #[serde(rename = "dangerouslyDisableSandbox")]
    pub dangerously_disable_sandbox: Option<bool>,
    #[serde(rename = "namespaceRestrictions")]
    pub namespace_restrictions: Option<bool>,
    #[serde(rename = "isolateNetwork")]
    pub isolate_network: Option<bool>,
    #[serde(rename = "filesystemMode")]
    pub filesystem_mode: Option<FilesystemIsolationMode>,
    #[serde(rename = "allowedMounts")]
    pub allowed_mounts: Option<Vec<String>>,
}

/// Output returned from a bash tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BashCommandOutput {
    pub stdout: String,
    pub stderr: String,
    #[serde(rename = "rawOutputPath")]
    pub raw_output_path: Option<String>,
    pub interrupted: bool,
    #[serde(rename = "isImage")]
    pub is_image: Option<bool>,
    #[serde(rename = "backgroundTaskId")]
    pub background_task_id: Option<String>,
    #[serde(rename = "backgroundedByUser")]
    pub backgrounded_by_user: Option<bool>,
    #[serde(rename = "assistantAutoBackgrounded")]
    pub assistant_auto_backgrounded: Option<bool>,
    #[serde(rename = "dangerouslyDisableSandbox")]
    pub dangerously_disable_sandbox: Option<bool>,
    #[serde(rename = "returnCodeInterpretation")]
    pub return_code_interpretation: Option<String>,
    #[serde(rename = "noOutputExpected")]
    pub no_output_expected: Option<bool>,
    #[serde(rename = "structuredContent")]
    pub structured_content: Option<Vec<serde_json::Value>>,
    #[serde(rename = "persistedOutputPath")]
    pub persisted_output_path: Option<String>,
    #[serde(rename = "persistedOutputSize")]
    pub persisted_output_size: Option<u64>,
    #[serde(rename = "sandboxStatus")]
    pub sandbox_status: Option<SandboxStatus>,
}

/// Executes a shell command with the requested sandbox settings.
pub fn execute_bash(input: BashCommandInput) -> io::Result<BashCommandOutput> {
    execute_bash_with_cancellation(input, Arc::new(AtomicBool::new(false)))
}

/// Executes a shell command while observing the owning tool invocation's
/// cancellation authority. Foreground children are assigned an independent
/// process group and explicitly terminated and reaped before this call returns
/// to the runtime's settle barrier.
pub fn execute_bash_with_cancellation(
    input: BashCommandInput,
    cancellation: Arc<AtomicBool>,
) -> io::Result<BashCommandOutput> {
    if cancellation.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "bash execution cancelled before dispatch",
        ));
    }
    let cwd = env::current_dir()?;
    let sandbox_status = sandbox_status_for_input(&input, &cwd);
    if sandbox_status.enabled && !sandbox_status.active {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox runner unavailable; command was not executed: {}",
                sandbox_status
                    .fallback_reason
                    .as_deref()
                    .unwrap_or("requested enforcement is unavailable")
            ),
        ));
    }

    if input.run_in_background.unwrap_or(false) {
        let mut child = prepare_command(&input.command, &cwd, &sandbox_status, false);
        let child = child
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        return Ok(BashCommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            raw_output_path: None,
            interrupted: false,
            is_image: None,
            background_task_id: Some(child.id().to_string()),
            backgrounded_by_user: Some(false),
            assistant_auto_backgrounded: Some(false),
            dangerously_disable_sandbox: input.dangerously_disable_sandbox,
            return_code_interpretation: None,
            no_output_expected: Some(true),
            structured_content: None,
            persisted_output_path: None,
            persisted_output_size: None,
            sandbox_status: Some(sandbox_status),
        });
    }

    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(execute_bash_async(input, sandbox_status, cwd, cancellation))
}

async fn execute_bash_async(
    input: BashCommandInput,
    sandbox_status: SandboxStatus,
    cwd: std::path::PathBuf,
    cancellation: Arc<AtomicBool>,
) -> io::Result<BashCommandOutput> {
    let mut command = prepare_tokio_command(&input.command, &cwd, &sandbox_status, true);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let process_group_id = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("bash child stdout pipe missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("bash child stderr pipe missing"))?;
    let stdout_reader = tokio::spawn(read_child_pipe(stdout));
    let stderr_reader = tokio::spawn(read_child_pipe(stderr));

    let (status, interruption) = if let Some(timeout_ms) = input.timeout {
        tokio::select! {
            result = child.wait() => (result?, None),
            () = wait_for_cancellation(Arc::clone(&cancellation)) => {
                let status = terminate_process_tree(&mut child, process_group_id).await?;
                (status, Some(("cancelled", "Command cancelled by the owning turn".to_string())))
            }
            () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                let status = terminate_process_tree(&mut child, process_group_id).await?;
                (status, Some(("timeout", format!("Command exceeded timeout of {timeout_ms} ms"))))
            }
        }
    } else {
        tokio::select! {
            result = child.wait() => (result?, None),
            () = wait_for_cancellation(cancellation) => {
                let status = terminate_process_tree(&mut child, process_group_id).await?;
                (status, Some(("cancelled", "Command cancelled by the owning turn".to_string())))
            }
        }
    };
    let stdout = join_child_pipe(stdout_reader).await?;
    let stderr = join_child_pipe(stderr_reader).await?;
    let stdout = truncate_output(&String::from_utf8_lossy(&stdout));
    let mut stderr = truncate_output(&String::from_utf8_lossy(&stderr));
    if let Some((_, message)) = interruption.as_ref() {
        if !stderr.trim().is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(message);
    }
    let no_output_expected = Some(stdout.trim().is_empty() && stderr.trim().is_empty());
    let return_code_interpretation = interruption.map_or_else(
        || {
            status.code().and_then(|code| {
                if code == 0 {
                    None
                } else {
                    Some(format!("exit_code:{code}"))
                }
            })
        },
        |(interpretation, _)| Some(interpretation.to_string()),
    );

    Ok(BashCommandOutput {
        stdout,
        stderr,
        raw_output_path: None,
        interrupted: return_code_interpretation
            .as_deref()
            .is_some_and(|value| matches!(value, "cancelled" | "timeout")),
        is_image: None,
        background_task_id: None,
        backgrounded_by_user: None,
        assistant_auto_backgrounded: None,
        dangerously_disable_sandbox: input.dangerously_disable_sandbox,
        return_code_interpretation,
        no_output_expected,
        structured_content: None,
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: Some(sandbox_status),
    })
}

async fn read_child_pipe<R: tokio::io::AsyncRead + Unpin>(mut pipe: R) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    pipe.read_to_end(&mut output).await?;
    Ok(output)
}

async fn join_child_pipe(
    reader: tokio::task::JoinHandle<io::Result<Vec<u8>>>,
) -> io::Result<Vec<u8>> {
    reader
        .await
        .map_err(|error| io::Error::other(format!("bash output reader failed: {error}")))?
}

async fn terminate_process_tree(
    child: &mut Child,
    process_group_id: Option<u32>,
) -> io::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    if let Some(process_group_id) = process_group_id {
        if let Err(error) =
            signal_process_group(process_group_id, nix::sys::signal::Signal::SIGTERM)
        {
            tracing::warn!(process_group_id, %error, "failed to terminate bash process group");
            child.start_kill()?;
        }
        let graceful = tokio::time::timeout(Duration::from_millis(250), child.wait()).await;
        if let Err(error) =
            signal_process_group(process_group_id, nix::sys::signal::Signal::SIGKILL)
        {
            tracing::warn!(process_group_id, %error, "failed to kill bash process group");
            if graceful.is_err() {
                child.start_kill()?;
            }
        }
        return match graceful {
            Ok(status) => status,
            Err(_) => child.wait().await,
        };
    }

    child.kill().await?;
    child.wait().await
}

#[cfg(unix)]
fn signal_process_group(process_group_id: u32, signal: nix::sys::signal::Signal) -> io::Result<()> {
    use nix::errno::Errno;
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;

    let process_group_id = i32::try_from(process_group_id)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process group id overflow"))?;
    match killpg(Pid::from_raw(process_group_id), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

async fn wait_for_cancellation(cancellation: Arc<AtomicBool>) {
    while !cancellation.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn sandbox_status_for_input(input: &BashCommandInput, cwd: &std::path::Path) -> SandboxStatus {
    let config = ConfigLoader::default_for(cwd).load().map_or_else(
        |_| SandboxConfig::default(),
        |runtime_config| runtime_config.sandbox().clone(),
    );
    let request = config.resolve_request(
        input.dangerously_disable_sandbox.map(|disabled| !disabled),
        input.namespace_restrictions,
        input.isolate_network,
        input.filesystem_mode,
        input.allowed_mounts.clone(),
    );
    resolve_sandbox_status_for_request(&request, cwd)
}

fn prepare_command(
    command: &str,
    cwd: &std::path::Path,
    sandbox_status: &SandboxStatus,
    create_dirs: bool,
) -> Command {
    if create_dirs {
        prepare_sandbox_dirs(cwd);
    }

    if let Some(launcher) = build_linux_sandbox_command(command, cwd, sandbox_status) {
        let mut prepared = Command::new(launcher.program);
        prepared.args(launcher.args);
        prepared.current_dir(cwd);
        prepared.envs(launcher.env);
        return prepared;
    }

    let mut prepared = Command::new("sh");
    prepared.arg("-lc").arg(command).current_dir(cwd);
    if sandbox_status.filesystem_active {
        prepared.env("HOME", cwd.join(".sandbox-home"));
        prepared.env("TMPDIR", cwd.join(".sandbox-tmp"));
    }
    prepared
}

fn prepare_tokio_command(
    command: &str,
    cwd: &std::path::Path,
    sandbox_status: &SandboxStatus,
    create_dirs: bool,
) -> TokioCommand {
    if create_dirs {
        prepare_sandbox_dirs(cwd);
    }

    if let Some(launcher) = build_linux_sandbox_command(command, cwd, sandbox_status) {
        let mut prepared = TokioCommand::new(launcher.program);
        prepared.args(launcher.args);
        prepared.current_dir(cwd);
        prepared.envs(launcher.env);
        prepared.kill_on_drop(true);
        #[cfg(unix)]
        prepared.process_group(0);
        return prepared;
    }

    let mut prepared = TokioCommand::new("sh");
    prepared.arg("-lc").arg(command).current_dir(cwd);
    prepared.kill_on_drop(true);
    #[cfg(unix)]
    prepared.process_group(0);
    if sandbox_status.filesystem_active {
        prepared.env("HOME", cwd.join(".sandbox-home"));
        prepared.env("TMPDIR", cwd.join(".sandbox-tmp"));
    }
    prepared
}

fn prepare_sandbox_dirs(cwd: &std::path::Path) {
    let _ = std::fs::create_dir_all(cwd.join(".sandbox-home"));
    let _ = std::fs::create_dir_all(cwd.join(".sandbox-tmp"));
}

#[cfg(test)]
mod tests {
    use super::{execute_bash, execute_bash_with_cancellation, BashCommandInput};
    use crate::sandbox::FilesystemIsolationMode;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn executes_simple_command() {
        let result = execute_bash(BashCommandInput {
            command: String::from("printf 'hello'"),
            timeout: Some(10_000),
            description: None,
            run_in_background: Some(false),
            dangerously_disable_sandbox: Some(false),
            namespace_restrictions: Some(false),
            isolate_network: Some(false),
            filesystem_mode: Some(FilesystemIsolationMode::WorkspaceOnly),
            allowed_mounts: None,
        });
        if crate::sandbox::sandbox_backend_capability()
            == crate::sandbox::EnforcementCapability::Full
        {
            let output = result.expect("bash command should execute in the probed sandbox");
            assert_eq!(output.stdout, "hello");
            assert!(!output.interrupted);
            assert!(output.sandbox_status.is_some());
        } else {
            let error = result.expect_err("unsupported sandbox must fail closed");
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("command was not executed"));
        }
    }

    #[test]
    #[cfg(unix)]
    fn cancellation_terminates_a_foreground_child_before_returning() {
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let child_pid_path = std::env::temp_dir().join(format!(
            "aos-bash-child-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        let command = format!(
            "sleep 30 & child=$!; printf '%s' \"$child\" > '{}'; wait \"$child\"",
            child_pid_path.display()
        );
        let worker = std::thread::spawn(move || {
            execute_bash_with_cancellation(
                BashCommandInput {
                    command,
                    timeout: Some(60_000),
                    description: None,
                    run_in_background: Some(false),
                    dangerously_disable_sandbox: Some(true),
                    namespace_restrictions: Some(false),
                    isolate_network: Some(false),
                    filesystem_mode: None,
                    allowed_mounts: None,
                },
                worker_cancellation,
            )
        });
        let child_pid_deadline = Instant::now() + Duration::from_secs(5);
        let child_pid = loop {
            match std::fs::read_to_string(&child_pid_path) {
                Ok(child_pid) if child_pid.trim().parse::<u32>().is_ok() => break child_pid,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    cancellation.store(true, Ordering::Release);
                    let _ = worker.join();
                    panic!("failed to read shell child pid: {error}");
                }
            }
            if Instant::now() >= child_pid_deadline {
                cancellation.store(true, Ordering::Release);
                let worker_result = worker.join();
                panic!(
                    "shell did not publish its child pid before the deadline; worker={worker_result:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let cancellation_started = Instant::now();
        cancellation.store(true, Ordering::Release);
        let output = worker
            .join()
            .expect("bash worker should join")
            .expect("cancelled bash should return an interrupted output");
        assert!(output.interrupted);
        assert_eq!(
            output.return_code_interpretation.as_deref(),
            Some("cancelled")
        );
        assert!(cancellation_started.elapsed() < Duration::from_secs(2));
        let still_running = std::process::Command::new("kill")
            .args(["-0", child_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!still_running, "cancelled shell child must be reaped");
        let _ = std::fs::remove_file(child_pid_path);
    }

    #[test]
    fn disables_sandbox_when_requested() {
        let output = execute_bash(BashCommandInput {
            command: String::from("printf 'hello'"),
            timeout: Some(10_000),
            description: None,
            run_in_background: Some(false),
            dangerously_disable_sandbox: Some(true),
            namespace_restrictions: None,
            isolate_network: None,
            filesystem_mode: None,
            allowed_mounts: None,
        })
        .expect("bash command should execute");

        assert!(!output.sandbox_status.expect("sandbox status").enabled);
    }
}

/// Maximum output bytes before truncation (16 KiB, matching upstream).
const MAX_OUTPUT_BYTES: usize = 16_384;

/// Truncate output to `MAX_OUTPUT_BYTES`, appending a marker when trimmed.
fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s.to_string();
    }
    // Find the last valid UTF-8 boundary at or before MAX_OUTPUT_BYTES
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = s[..end].to_string();
    truncated.push_str("\n\n[output truncated — exceeded 16384 bytes]");
    truncated
}

#[cfg(test)]
mod truncation_tests {
    use super::*;

    #[test]
    fn short_output_unchanged() {
        let s = "hello world";
        assert_eq!(truncate_output(s), s);
    }

    #[test]
    fn long_output_truncated() {
        let s = "x".repeat(20_000);
        let result = truncate_output(&s);
        assert!(result.len() < 20_000);
        assert!(result.ends_with("[output truncated — exceeded 16384 bytes]"));
    }

    #[test]
    fn exact_boundary_unchanged() {
        let s = "a".repeat(MAX_OUTPUT_BYTES);
        assert_eq!(truncate_output(&s), s);
    }

    #[test]
    fn one_over_boundary_truncated() {
        let s = "a".repeat(MAX_OUTPUT_BYTES + 1);
        let result = truncate_output(&s);
        assert!(result.contains("[output truncated"));
    }
}
