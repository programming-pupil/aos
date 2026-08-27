use std::path::{Component, Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::{Command, Stdio};
use std::sync::{atomic::AtomicBool, Arc, OnceLock};
use std::time::Duration;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Instant;

#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MEMORY_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(target_os = "linux")]
const FILE_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(target_os = "linux")]
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe() -> EnforcementCapability {
    EnforcementCapability::Unavailable
}

#[cfg(target_os = "macos")]
fn probe() -> EnforcementCapability {
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return EnforcementCapability::Unavailable;
    }
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    );
    let root = std::env::temp_dir().join(format!("aos-seatbelt-probe-{nonce}"));
    // macOS exposes the process temp directory below /private/var/folders,
    // which is not a stable privacy boundary. Probe a path covered by the
    // explicit external-temp deny rule instead.
    let outside = PathBuf::from("/private/tmp").join(format!("aos-seatbelt-outside-{nonce}"));
    if std::fs::create_dir_all(root.join("generated")).is_err()
        || std::fs::create_dir_all(&outside).is_err()
        || std::fs::write(outside.join("private.txt"), b"secret").is_err()
    {
        return EnforcementCapability::Unavailable;
    }
    let outside_path = shell_single_quote(&outside.join("private.txt").to_string_lossy());
    let result = execute_internal(
        &root,
        Path::new("/workspace"),
        &format!(
            "test ! -r {outside_path} && test ! -w /tmp && \
             test ! -r /Users/Shared && touch generated/ok"
        ),
        Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
        false,
        &[PathBuf::from("generated")],
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
    if result.is_ok_and(|output| !output.timed_out && output.exit_code == Some(0)) {
        EnforcementCapability::Full
    } else {
        EnforcementCapability::Unavailable
    }
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
    if capability() != EnforcementCapability::Full {
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
    build_platform_launcher(
        workspace_root,
        sandbox_cwd,
        command,
        timeout,
        writable_workspace,
        writable_paths,
    )
}

#[cfg(target_os = "linux")]
fn build_platform_launcher(
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

#[cfg(target_os = "macos")]
fn build_platform_launcher(
    workspace_root: &Path,
    sandbox_cwd: &Path,
    command: &str,
    _timeout: Duration,
    writable_workspace: bool,
    writable_paths: &[PathBuf],
) -> Result<SandboxLauncher, String> {
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("sandbox workspace is unavailable: {error}"))?;
    let relative_cwd = sandbox_cwd
        .strip_prefix("/workspace")
        .map_err(|_| "sandbox cwd must stay inside /workspace".to_string())?;
    validate_relative_mount(relative_cwd)?;
    let host_cwd = canonical_root.join(relative_cwd);
    let canonical_cwd = host_cwd
        .canonicalize()
        .map_err(|error| format!("sandbox cwd is unavailable: {error}"))?;
    if !canonical_cwd.starts_with(&canonical_root) {
        return Err("sandbox cwd escapes the workspace".into());
    }

    let sandbox_home = canonical_root.join(".sandbox-home");
    let sandbox_tmp = canonical_root.join(".sandbox-tmp");
    std::fs::create_dir_all(&sandbox_home)
        .and_then(|()| std::fs::create_dir_all(&sandbox_tmp))
        .map_err(|error| format!("failed to initialize sandbox private directories: {error}"))?;

    let mut writable_roots = vec![sandbox_home.clone(), sandbox_tmp.clone()];
    if writable_workspace {
        writable_roots.push(canonical_root.clone());
    } else {
        for relative in writable_paths {
            validate_relative_mount(relative)?;
            let host = canonical_root.join(relative);
            let canonical_host = host
                .canonicalize()
                .map_err(|error| format!("sandbox writable path is unavailable: {error}"))?;
            if !canonical_host.starts_with(&canonical_root) {
                return Err("sandbox writable path escapes the workspace".into());
            }
            writable_roots.push(canonical_host);
        }
    }

    let canonical_root_path = canonical_root.clone();
    let canonical_root = seatbelt_string(&canonical_root.to_string_lossy());
    let mut profile = format!(
        "(version 1)\n\
         (allow default)\n\
         (deny network*)\n\
         (deny file-read* (subpath \"/private/tmp\") (subpath \"/Volumes\"))\n\
         (deny file-write*)\n\
         (deny signal (target others))\n\
         (allow file-read* (subpath \"{canonical_root}\"))\n\
         (allow file-write* (literal \"/dev/null\"))\n"
    );
    append_macos_workspace_path_policy(&mut profile, &canonical_root_path)?;
    for writable_root in writable_roots {
        let writable_root = seatbelt_string(&writable_root.to_string_lossy());
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{writable_root}\"))\n"
        ));
    }
    let command = format!(
        "export HOME={} TMPDIR={}; exec /bin/sh -lc {}",
        shell_single_quote(&sandbox_home.to_string_lossy()),
        shell_single_quote(&sandbox_tmp.to_string_lossy()),
        shell_single_quote(command),
    );
    Ok(SandboxLauncher {
        program: "/usr/bin/sandbox-exec".into(),
        args: vec!["-p".into(), profile, "/bin/sh".into(), "-c".into(), command],
    })
}

#[cfg(target_os = "macos")]
fn append_macos_workspace_path_policy(
    profile: &mut String,
    workspace_root: &Path,
) -> Result<(), String> {
    // Seatbelt denies take precedence over descendant allows. A blanket deny
    // on /Users or /private/var/folders therefore makes an otherwise allowed
    // workspace look like a non-directory when a shell tries to `cd` into a
    // child. Keep the parent chain traversable and deny every sibling at each
    // level, so the command can reach only this authenticated workspace.
    let mut child = workspace_root.to_path_buf();
    while let Some(parent) = child.parent() {
        let child_name = child
            .file_name()
            .ok_or_else(|| "sandbox workspace has an invalid path component".to_string())?;
        let entries = std::fs::read_dir(parent)
            .map_err(|error| format!("sandbox workspace parent is unavailable: {error}"))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("sandbox workspace parent read failed: {error}"))?;
            if entry.file_name() == child_name {
                continue;
            }
            let sibling = seatbelt_string(&entry.path().to_string_lossy());
            profile.push_str(&format!("(deny file-read* (subpath \"{sibling}\"))\n"));
        }
        child = parent.to_path_buf();
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn build_platform_launcher(
    _workspace_root: &Path,
    _sandbox_cwd: &Path,
    _command: &str,
    _timeout: Duration,
    _writable_workspace: bool,
    _writable_paths: &[PathBuf],
) -> Result<SandboxLauncher, String> {
    Err("sandbox backend is unavailable; command was not executed".into())
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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
    let mut prepared = Command::new(&launcher.program);
    prepared
        .args(&launcher.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        prepared.process_group(0);
    }
    #[cfg(target_os = "macos")]
    prepared.current_dir(resolve_macos_host_cwd(workspace_root, sandbox_cwd)?);
    let mut child = prepared
        .spawn()
        .map_err(|error| format!("sandbox runner failed before command dispatch: {error}"))?;
    let process_group_id = child.id();
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
            terminate_process_group(&mut child, process_group_id);
            break child.wait().ok();
        }
        if cancellation.load(Ordering::Acquire) {
            cancelled = true;
            terminate_process_group(&mut child, process_group_id);
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn seatbelt_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn resolve_macos_host_cwd(workspace_root: &Path, sandbox_cwd: &Path) -> Result<PathBuf, String> {
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("sandbox workspace is unavailable: {error}"))?;
    let relative = sandbox_cwd
        .strip_prefix("/workspace")
        .map_err(|_| "sandbox cwd must stay inside /workspace".to_string())?;
    validate_relative_mount(relative)?;
    let canonical_cwd = canonical_root
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("sandbox cwd is unavailable: {error}"))?;
    if !canonical_cwd.starts_with(&canonical_root) {
        return Err("sandbox cwd escapes the workspace".into());
    }
    Ok(canonical_cwd)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn terminate_process_group(child: &mut std::process::Child, process_group_id: u32) {
    use nix::errno::Errno;
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    let Ok(process_group_id) = i32::try_from(process_group_id) else {
        let _ = child.kill();
        return;
    };
    let process_group = Pid::from_raw(process_group_id);
    if let Err(error) = killpg(process_group, Signal::SIGTERM) {
        if error != Errno::ESRCH {
            let _ = child.kill();
        }
    }
    let grace_started = Instant::now();
    while grace_started.elapsed() < Duration::from_millis(250) {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // The shell may exit before its descendants. Always signal the process
    // group again so cancellation cannot leave a detached foreground child.
    if let Err(error) = killpg(process_group, Signal::SIGKILL) {
        if error != Errno::ESRCH {
            let _ = child.kill();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn test_workspace(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aos-sandbox-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn confined_command_writes_only_to_the_workspace() {
        if capability() != EnforcementCapability::Full {
            return;
        }
        let root = test_workspace("write");
        std::fs::create_dir_all(&root).expect("create sandbox test workspace");
        let outside_name = format!(
            "{}-outside",
            root.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .expect("test workspace name")
        );
        let outside = if cfg!(target_os = "macos") {
            PathBuf::from("/private/tmp").join(&outside_name)
        } else {
            std::env::temp_dir().join(&outside_name)
        };
        let command = format!(
            "printf inside > allowed.txt; printf outside > {} 2>/dev/null || true",
            shell_single_quote(&outside.to_string_lossy())
        );
        let output = execute(
            &root,
            Path::new("/workspace"),
            &command,
            Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
            true,
            &[],
        )
        .expect("execute confined command");
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(
            std::fs::read_to_string(root.join("allowed.txt")).expect("workspace output"),
            "inside"
        );
        let outside_created = outside.exists();
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(root);
        assert!(!outside_created, "sandbox wrote outside its workspace");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cancellation_terminates_the_sandbox_process_group_promptly() {
        if capability() != EnforcementCapability::Full {
            return;
        }
        let root = test_workspace("cancel");
        std::fs::create_dir_all(&root).expect("create sandbox test workspace");
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || {
            execute(
                &worker_root,
                Path::new("/workspace"),
                "printf started > started; sleep 30",
                Duration::from_secs(35),
                worker_cancellation,
                true,
                &[],
            )
            .expect("execute cancellable confined command")
        });
        let marker = root.join("started");
        let wait_started = Instant::now();
        while !marker.exists() && wait_started.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists(), "sandbox command should start");
        let cancellation_started = Instant::now();
        cancellation.store(true, Ordering::Release);
        let output = worker.join().expect("sandbox worker should not panic");
        assert!(output.cancelled);
        assert!(cancellation_started.elapsed() < Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(root);
    }
}
