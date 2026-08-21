use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use std::sync::{atomic::AtomicBool, Arc, OnceLock};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
const MEMORY_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
const FILE_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const PROCESS_LIMIT: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementCapability {
    Full,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLauncher {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfinedOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
}

#[must_use]
pub fn capability() -> EnforcementCapability {
    static CAPABILITY: OnceLock<EnforcementCapability> = OnceLock::new();
    *CAPABILITY.get_or_init(probe)
}

#[cfg(not(target_os = "linux"))]
fn probe() -> EnforcementCapability {
    EnforcementCapability::Unavailable
}

#[cfg(target_os = "linux")]
fn probe() -> EnforcementCapability {
    if !command_exists("bwrap") || !command_exists("prlimit") {
        return EnforcementCapability::Unavailable;
    }
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    );
    let root = std::env::temp_dir().join(format!("aos-sandbox-probe-{nonce}"));
    let outside = std::env::temp_dir().join(format!("aos-sandbox-outside-{nonce}"));
    if std::fs::create_dir_all(root.join("generated")).is_err()
        || std::fs::create_dir_all(&outside).is_err()
        || std::fs::write(outside.join("private.txt"), b"secret").is_err()
    {
        return EnforcementCapability::Unavailable;
    }
    let outside_path = shell_single_quote(&outside.join("private.txt").to_string_lossy());
    let script = format!(
        "test ! -e /etc/passwd && test ! -e /root && test ! -e {outside_path} && \
         test -w /workspace/generated && test ! -w /workspace && \
         grep -Eq '^Max processes +64 +64 +processes$' /proc/self/limits && \
         grep -Eq '^Max address space +1073741824 +1073741824 +bytes$' /proc/self/limits && \
         {{ routes=0; while IFS= read -r _line; do routes=$((routes + 1)); done < /proc/net/route; \
         test \"$routes\" -le 1; }}"
    );
    let result = execute_internal(
        &root,
        Path::new("/"),
        &script,
        Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
        false,
        &[PathBuf::from("generated")],
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
    if result.is_ok_and(|output| !output.timed_out && output.exit_code == Some(0)) {
        EnforcementCapability::Full
    } else {
        EnforcementCapability::Unavailable
    }
}

#[must_use]
pub fn build_launcher(
    workspace_root: &Path,
    sandbox_cwd: &Path,
    command: &str,
    timeout: Duration,
    writable_workspace: bool,
    writable_paths: &[PathBuf],
) -> Option<SandboxLauncher> {
    if !cfg!(target_os = "linux") || capability() != EnforcementCapability::Full {
        return None;
    }
    build_launcher_unchecked(
        workspace_root,
        sandbox_cwd,
        command,
        timeout,
        writable_workspace,
        writable_paths,
    )
    .ok()
}

fn validate_relative_mount(path: &Path) -> Result<(), String> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "sandbox writable mount must stay inside the workspace: {}",
            path.display()
        ));
    }
    Ok(())
}

fn build_launcher_unchecked(
    workspace_root: &Path,
    sandbox_cwd: &Path,
    command: &str,
    timeout: Duration,
    writable_workspace: bool,
    writable_paths: &[PathBuf],
) -> Result<SandboxLauncher, String> {
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("sandbox workspace is unavailable: {error}"))?;
    let mut args = vec![
        format!("--cpu={}", timeout.as_secs().max(1)),
        format!("--as={MEMORY_LIMIT_BYTES}"),
        format!("--fsize={FILE_LIMIT_BYTES}"),
        format!("--nproc={PROCESS_LIMIT}"),
        "--".into(),
        "bwrap".into(),
        "--unshare-all".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--clearenv".into(),
    ];
    for path in ["/usr", "/bin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            args.extend(["--ro-bind".into(), path.into(), path.into()]);
        }
    }
    args.extend([
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--dir".into(),
        "/workspace".into(),
        if writable_workspace {
            "--bind".into()
        } else {
            "--ro-bind".into()
        },
        canonical_root.to_string_lossy().into_owned(),
        "/workspace".into(),
    ]);
    if !writable_workspace {
        for relative in writable_paths {
            validate_relative_mount(relative)?;
            let host = canonical_root.join(relative);
            let canonical_host = host
                .canonicalize()
                .map_err(|error| format!("sandbox writable mount is unavailable: {error}"))?;
            if !canonical_host.starts_with(&canonical_root) {
                return Err("sandbox writable mount escapes the workspace".into());
            }
            let target = Path::new("/workspace").join(relative);
            args.extend([
                "--bind".into(),
                canonical_host.to_string_lossy().into_owned(),
                target.to_string_lossy().into_owned(),
            ]);
        }
    }
    args.extend([
        "--setenv".into(),
        "HOME".into(),
        "/tmp".into(),
        "--setenv".into(),
        "PATH".into(),
        "/usr/bin:/bin".into(),
        "--chdir".into(),
        sandbox_cwd.to_string_lossy().into_owned(),
        "/bin/sh".into(),
        "-lc".into(),
        command.into(),
    ]);
    Ok(SandboxLauncher {
        program: "prlimit".into(),
        args,
    })
}

pub fn execute(
    workspace_root: &Path,
    sandbox_cwd: &Path,
    command: &str,
    timeout: Duration,
    cancellation: Arc<AtomicBool>,
    writable_workspace: bool,
    writable_paths: &[PathBuf],
) -> Result<ConfinedOutput, String> {
    if capability() != EnforcementCapability::Full {
        return Err("sandbox backend is unavailable; command was not executed".into());
    }
    execute_internal(
        workspace_root,
        sandbox_cwd,
        command,
        timeout,
        cancellation,
        writable_workspace,
        writable_paths,
    )
}

#[cfg(not(target_os = "linux"))]
fn execute_internal(
    _workspace_root: &Path,
    _sandbox_cwd: &Path,
    _command: &str,
    _timeout: Duration,
    _cancellation: Arc<AtomicBool>,
    _writable_workspace: bool,
    _writable_paths: &[PathBuf],
) -> Result<ConfinedOutput, String> {
    Err("sandbox backend is unavailable; command was not executed".into())
}

#[cfg(target_os = "linux")]
fn execute_internal(
    workspace_root: &Path,
    sandbox_cwd: &Path,
    command: &str,
    timeout: Duration,
    cancellation: Arc<AtomicBool>,
    writable_workspace: bool,
    writable_paths: &[PathBuf],
) -> Result<ConfinedOutput, String> {
    use std::sync::atomic::Ordering;
    if cancellation.load(Ordering::Acquire) {
        return Err("sandbox execution cancelled before start".into());
    }
    let launcher = build_launcher_unchecked(
        workspace_root,
        sandbox_cwd,
        command,
        timeout,
        writable_workspace,
        writable_paths,
    )?;
    let started = Instant::now();
    let mut child = Command::new(&launcher.program)
        .args(&launcher.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("sandbox runner failed before command dispatch: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "sandbox runner did not expose stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "sandbox runner did not expose stderr".to_string())?;
    let stdout_reader = std::thread::spawn(move || drain_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || drain_bounded(stderr));
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("sandbox runner status failed: {error}"))?
        {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        if cancellation.load(Ordering::Acquire) {
            cancelled = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "sandbox stdout reader panicked".to_string())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "sandbox stderr reader panicked".to_string())??;
    Ok(ConfinedOutput {
        stdout,
        stderr,
        exit_code: status.and_then(|status| status.code()),
        timed_out,
        cancelled,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

#[cfg(target_os = "linux")]
fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "linux")]
fn drain_bounded<R: std::io::Read>(mut reader: R) -> Result<String, String> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("sandbox output read failed: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(String::from_utf8_lossy(&retained).into_owned())
}
