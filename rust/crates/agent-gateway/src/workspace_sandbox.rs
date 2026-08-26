use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

pub(crate) type SandboxOutput = runtime::ConfinedOutput;

pub(crate) fn isolation_available() -> bool {
    runtime::sandbox_backend_capability() == runtime::EnforcementCapability::Full
}

pub(crate) fn execute(
    snapshot_root: &Path,
    cwd: &Path,
    command: &str,
    timeout: Duration,
    cancellation: Arc<AtomicBool>,
) -> Result<SandboxOutput, String> {
    runtime::execute_confined_command(
        snapshot_root,
        cwd,
        command,
        timeout,
        cancellation,
        false,
        &[PathBuf::from("generated")],
    )
}

#[cfg(test)]
mod tests {
    use super::{execute, isolation_available};
    use std::path::Path;
    use std::sync::{atomic::AtomicBool, Arc};
    use std::time::Duration;

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn unsupported_host_never_advertises_workspace_execution() {
        assert!(!isolation_available());
    }

    #[test]
    fn unavailable_backend_fails_closed_before_command_dispatch() {
        if isolation_available() {
            return;
        }
        let error = execute(
            Path::new("/path/that/must/not/be/read"),
            Path::new("/workspace"),
            "touch should-not-exist",
            Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        )
        .expect_err("unavailable sandbox must reject execution");
        assert!(error.contains("command was not executed"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn probed_backend_hides_host_and_limits_writes() {
        if !isolation_available() {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("aos-workspace-sandbox-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("generated")).expect("create generated mount");
        let output = execute(
            &root,
            Path::new("/workspace"),
            if cfg!(target_os = "linux") {
                "test ! -e /etc/passwd && test ! -w /workspace && touch /workspace/generated/ok"
            } else {
                "test ! -r /Users/Shared && test ! -w /tmp && touch generated/ok"
            },
            Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("probed sandbox should execute");
        assert_eq!(output.exit_code, Some(0));
        assert!(root.join("generated/ok").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
