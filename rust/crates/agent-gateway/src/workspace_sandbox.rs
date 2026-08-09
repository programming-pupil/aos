use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use std::sync::{atomic::AtomicBool, Arc, OnceLock};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MEMORY_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(target_os = "linux")]
const FILE_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(target_os = "linux")]
const PROCESS_LIMIT: u64 = 64;

#[derive(Debug)]
pub(crate) struct SandboxOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
}

pub(crate) fn isolation_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(probe_isolation)
}

#[cfg(not(target_os = "linux"))]
fn probe_isolation() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn probe_isolation() -> bool {
    if !command_exists("bwrap") || !command_exists("prlimit") {
        return false;
    }
    let probe_root =
        std::env::temp_dir().join(format!("aos-workspace-probe-{}", uuid::Uuid::new_v4()));
    let generated = probe_root.join("generated");
    let other_user_root = std::env::temp_dir().join(format!(
        "aos-workspace-probe-other-user-{}",
        uuid::Uuid::new_v4()
    ));
    if std::fs::create_dir_all(&generated).is_err()
        || std::fs::create_dir_all(&other_user_root).is_err()
        || std::fs::write(other_user_root.join("private.txt"), b"secret").is_err()
    {
        return false;
    }
    let script = format!(
        r#"
        test ! -e /etc/passwd &&
        test ! -e /root &&
        test ! -e /home &&
        test ! -e {} &&
        test -w /workspace/generated &&
        test ! -w /workspace &&
        grep -Eq '^Max processes +64 +64 +processes$' /proc/self/limits &&
        grep -Eq '^Max address space +1073741824 +1073741824 +bytes$' /proc/self/limits &&
        grep -Eq '^Max file size +268435456 +268435456 +bytes$' /proc/self/limits &&
        routes=0
        while IFS= read -r _line; do routes=$((routes + 1)); done < /proc/net/route
        test "$routes" -le 1
    "#,
        shell_single_quote(&other_user_root.join("private.txt").to_string_lossy())
    );
    let result = run_linux_sandbox(
        &probe_root,
        Path::new("/"),
        &script,
        Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    let _ = std::fs::remove_dir_all(probe_root);
    let _ = std::fs::remove_dir_all(other_user_root);
    result.is_ok_and(|output| !output.timed_out && output.exit_code == Some(0))
}

#[cfg(target_os = "linux")]
fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn execute(
    snapshot_root: &Path,
    cwd: &Path,
    command: &str,
    timeout: Duration,
    cancellation: Arc<AtomicBool>,
) -> Result<SandboxOutput, String> {
    if !isolation_available() {
        return Err("workspace execution isolation is unavailable".to_string());
    }
    #[cfg(target_os = "linux")]
    {
        run_linux_sandbox(snapshot_root, cwd, command, timeout, cancellation)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (snapshot_root, cwd, command, timeout, cancellation);
        Err("workspace execution isolation is unavailable".to_string())
    }
}

#[cfg(target_os = "linux")]
fn run_linux_sandbox(
    snapshot_root: &Path,
    cwd: &Path,
    command: &str,
    timeout: Duration,
    cancellation: Arc<AtomicBool>,
) -> Result<SandboxOutput, String> {
    use std::sync::atomic::Ordering;

    if cancellation.load(Ordering::Acquire) {
        return Err("workspace execution cancelled".to_string());
    }
    let generated = snapshot_root.join("generated");
    let mut process = Command::new("prlimit");
    process
        .arg(format!("--cpu={}", timeout.as_secs().max(1)))
        .arg(format!("--as={MEMORY_LIMIT_BYTES}"))
        .arg(format!("--fsize={FILE_LIMIT_BYTES}"))
        .arg(format!("--nproc={PROCESS_LIMIT}"))
        .arg("--")
        .arg("bwrap")
        .arg("--unshare-all")
        .arg("--die-with-parent")
        .arg("--new-session")
        .arg("--clearenv");
    add_read_only_system_mounts(&mut process);
    process
        .arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev")
        .arg("--tmpfs")
        .arg("/tmp")
        .arg("--dir")
        .arg("/workspace")
        .arg("--ro-bind")
        .arg(snapshot_root)
        .arg("/workspace")
        .arg("--bind")
        .arg(&generated)
        .arg("/workspace/generated")
        .arg("--setenv")
        .arg("HOME")
        .arg("/tmp")
        .arg("--setenv")
        .arg("PATH")
        .arg("/usr/bin:/bin")
        .arg("--chdir")
        .arg(cwd)
        .arg("/bin/sh")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let started = Instant::now();
    let mut child = process
        .spawn()
        .map_err(|error| format!("failed to start isolated workspace command: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture isolated command stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture isolated command stderr".to_string())?;
    let stdout_reader = std::thread::spawn(move || drain_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || drain_bounded(stderr));
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to poll isolated workspace command: {error}"))?
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
        .map_err(|_| "isolated stdout reader panicked".to_string())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "isolated stderr reader panicked".to_string())??;
    Ok(SandboxOutput {
        stdout,
        stderr,
        exit_code: status.and_then(|status| status.code()),
        timed_out,
        cancelled,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

#[cfg(target_os = "linux")]
fn add_read_only_system_mounts(command: &mut Command) {
    for path in ["/usr", "/bin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
}

#[cfg(target_os = "linux")]
fn drain_bounded<R: std::io::Read>(mut reader: R) -> Result<String, String> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("failed to read isolated command output: {error}"))?;
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
    use super::isolation_available;
    #[cfg(target_os = "linux")]
    use std::{path::Path, time::Duration};

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_host_never_advertises_workspace_execution() {
        assert!(!isolation_available());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn isolated_execution_hides_host_paths_and_symlink_targets() {
        use super::{command_exists, run_linux_sandbox};

        if !command_exists("bwrap") || !command_exists("prlimit") || !isolation_available() {
            assert!(!isolation_available());
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aos-workspace-sandbox-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("generated")).expect("create generated mount");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", root.join("host-passwd"))
            .expect("create escape symlink");
        let output = run_linux_sandbox(
            &root,
            Path::new("/"),
            "test ! -e /etc/passwd && test ! -e /workspace/host-passwd && test \"$(wc -l < /proc/net/route)\" -le 1",
            Duration::from_secs(5),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .expect("sandbox should start");
        let _ = std::fs::remove_dir_all(root);
        assert_eq!(output.exit_code, Some(0), "{}", output.stderr);
        assert!(!output.timed_out);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn isolated_execution_enforces_wall_timeout() {
        use super::{command_exists, run_linux_sandbox};

        if !command_exists("bwrap") || !command_exists("prlimit") || !isolation_available() {
            assert!(!isolation_available());
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aos-workspace-timeout-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("generated")).expect("create generated mount");
        let output = run_linux_sandbox(
            &root,
            Path::new("/"),
            "sleep 5",
            Duration::from_millis(150),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .expect("sandbox should start");
        let _ = std::fs::remove_dir_all(root);
        assert!(output.timed_out);
        assert!(output.duration_ms < 2_000);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn isolated_execution_honors_explicit_cancellation() {
        use super::{command_exists, run_linux_sandbox};

        if !command_exists("bwrap") || !command_exists("prlimit") || !isolation_available() {
            assert!(!isolation_available());
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aos-workspace-cancel-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("generated")).expect("create generated mount");
        let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = cancellation.clone();
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            signal.store(true, std::sync::atomic::Ordering::Release);
        });
        let output = run_linux_sandbox(
            &root,
            Path::new("/"),
            "sleep 5",
            Duration::from_secs(5),
            cancellation,
        )
        .expect("sandbox should start");
        trigger.join().expect("cancellation trigger");
        let _ = std::fs::remove_dir_all(root);
        assert!(output.cancelled);
        assert!(!output.timed_out);
        assert!(output.duration_ms < 2_000);
    }
}
