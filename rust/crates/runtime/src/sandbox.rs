use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use crate::sandbox_backend::{ConfinedOutput, EnforcementCapability};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemIsolationMode {
    Off,
    #[default]
    WorkspaceOnly,
    AllowList,
}

impl FilesystemIsolationMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::WorkspaceOnly => "workspace-only",
            Self::AllowList => "allow-list",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SandboxConfig {
    pub enabled: Option<bool>,
    pub namespace_restrictions: Option<bool>,
    pub network_isolation: Option<bool>,
    pub filesystem_mode: Option<FilesystemIsolationMode>,
    pub allowed_mounts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SandboxRequest {
    pub enabled: bool,
    pub namespace_restrictions: bool,
    pub network_isolation: bool,
    pub filesystem_mode: FilesystemIsolationMode,
    pub allowed_mounts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ContainerEnvironment {
    pub in_container: bool,
    pub markers: Vec<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SandboxStatus {
    pub enabled: bool,
    pub requested: SandboxRequest,
    pub supported: bool,
    pub active: bool,
    pub namespace_supported: bool,
    pub namespace_active: bool,
    pub network_supported: bool,
    pub network_active: bool,
    pub filesystem_mode: FilesystemIsolationMode,
    pub filesystem_supported: bool,
    pub filesystem_active: bool,
    pub allowed_mounts: Vec<String>,
    pub in_container: bool,
    pub container_markers: Vec<String>,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxDetectionInputs<'a> {
    pub env_pairs: Vec<(String, String)>,
    pub dockerenv_exists: bool,
    pub containerenv_exists: bool,
    pub proc_1_cgroup: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxSandboxCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[must_use]
pub fn sandbox_backend_capability() -> EnforcementCapability {
    crate::sandbox_backend::capability()
}

pub fn execute_confined_command(
    workspace_root: &Path,
    sandbox_cwd: &Path,
    command: &str,
    timeout: Duration,
    cancellation: Arc<AtomicBool>,
    writable_workspace: bool,
    writable_paths: &[PathBuf],
) -> Result<ConfinedOutput, String> {
    crate::sandbox_backend::execute(
        workspace_root,
        sandbox_cwd,
        command,
        timeout,
        cancellation,
        writable_workspace,
        writable_paths,
    )
}

impl SandboxConfig {
    #[must_use]
    pub fn resolve_request(
        &self,
        enabled_override: Option<bool>,
        namespace_override: Option<bool>,
        network_override: Option<bool>,
        filesystem_mode_override: Option<FilesystemIsolationMode>,
        allowed_mounts_override: Option<Vec<String>>,
    ) -> SandboxRequest {
        SandboxRequest {
            enabled: enabled_override.unwrap_or(self.enabled.unwrap_or(true)),
            namespace_restrictions: namespace_override
                .unwrap_or(self.namespace_restrictions.unwrap_or(true)),
            network_isolation: network_override.unwrap_or(self.network_isolation.unwrap_or(false)),
            filesystem_mode: filesystem_mode_override
                .or(self.filesystem_mode)
                .unwrap_or_default(),
            allowed_mounts: allowed_mounts_override.unwrap_or_else(|| self.allowed_mounts.clone()),
        }
    }
}

#[must_use]
pub fn detect_container_environment() -> ContainerEnvironment {
    let proc_1_cgroup = fs::read_to_string("/proc/1/cgroup").ok();
    detect_container_environment_from(SandboxDetectionInputs {
        env_pairs: env::vars().collect(),
        dockerenv_exists: Path::new("/.dockerenv").exists(),
        containerenv_exists: Path::new("/run/.containerenv").exists(),
        proc_1_cgroup: proc_1_cgroup.as_deref(),
    })
}

#[must_use]
pub fn detect_container_environment_from(
    inputs: SandboxDetectionInputs<'_>,
) -> ContainerEnvironment {
    let mut markers = Vec::new();
    if inputs.dockerenv_exists {
        markers.push("/.dockerenv".to_string());
    }
    if inputs.containerenv_exists {
        markers.push("/run/.containerenv".to_string());
    }
    for (key, value) in inputs.env_pairs {
        let normalized = key.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "container" | "docker" | "podman" | "kubernetes_service_host"
        ) && !value.is_empty()
        {
            markers.push(format!("env:{key}={value}"));
        }
    }
    if let Some(cgroup) = inputs.proc_1_cgroup {
        for needle in ["docker", "containerd", "kubepods", "podman", "libpod"] {
            if cgroup.contains(needle) {
                markers.push(format!("/proc/1/cgroup:{needle}"));
            }
        }
    }
    markers.sort();
    markers.dedup();
    ContainerEnvironment {
        in_container: !markers.is_empty(),
        markers,
    }
}

#[must_use]
pub fn resolve_sandbox_status(config: &SandboxConfig, cwd: &Path) -> SandboxStatus {
    let request = config.resolve_request(None, None, None, None, None);
    resolve_sandbox_status_for_request(&request, cwd)
}

#[must_use]
pub fn resolve_sandbox_status_for_request(request: &SandboxRequest, cwd: &Path) -> SandboxStatus {
    let container = detect_container_environment();
    let backend_full = sandbox_backend_capability() == EnforcementCapability::Full;
    let namespace_supported = backend_full;
    let network_supported = backend_full;
    let filesystem_supported = backend_full;
    let mut fallback_reasons = Vec::new();

    if request.enabled && request.namespace_restrictions && !namespace_supported {
        fallback_reasons
            .push("namespace isolation unavailable (bwrap/prlimit probe failed)".to_string());
    }
    if request.enabled && request.network_isolation && !network_supported {
        fallback_reasons
            .push("network isolation unavailable (bwrap/prlimit probe failed)".to_string());
    }
    if request.enabled && request.filesystem_mode != FilesystemIsolationMode::Off {
        if !filesystem_supported {
            fallback_reasons.push(
                "filesystem isolation unavailable (bwrap/prlimit enforcement probe failed)"
                    .to_string(),
            );
        }
    }
    if request.enabled
        && request.filesystem_mode == FilesystemIsolationMode::AllowList
        && request.allowed_mounts.is_empty()
    {
        fallback_reasons
            .push("filesystem allow-list requested without configured mounts".to_string());
    }

    let supported = (!request.namespace_restrictions || namespace_supported)
        && (!request.network_isolation || network_supported)
        && (request.filesystem_mode == FilesystemIsolationMode::Off || filesystem_supported);
    let active = request.enabled && supported;
    let filesystem_active = active && request.filesystem_mode != FilesystemIsolationMode::Off;

    let allowed_mounts = normalize_mounts(&request.allowed_mounts, cwd);

    SandboxStatus {
        enabled: request.enabled,
        requested: request.clone(),
        supported,
        active,
        namespace_supported,
        namespace_active: request.enabled && request.namespace_restrictions && namespace_supported,
        network_supported,
        network_active: request.enabled && request.network_isolation && network_supported,
        filesystem_mode: request.filesystem_mode,
        filesystem_supported,
        filesystem_active,
        allowed_mounts,
        in_container: container.in_container,
        container_markers: container.markers,
        fallback_reason: (!fallback_reasons.is_empty()).then(|| fallback_reasons.join("; ")),
    }
}

#[must_use]
pub fn build_linux_sandbox_command(
    command: &str,
    cwd: &Path,
    status: &SandboxStatus,
) -> Option<LinuxSandboxCommand> {
    if !status.active {
        return None;
    }
    let writable_workspace = status.filesystem_mode != FilesystemIsolationMode::AllowList;
    let writable_paths = if status.filesystem_mode == FilesystemIsolationMode::AllowList {
        status
            .allowed_mounts
            .iter()
            .filter_map(|mount| {
                Path::new(mount)
                    .strip_prefix(cwd)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    crate::sandbox_backend::build_launcher(
        cwd,
        Path::new("/workspace"),
        command,
        Duration::from_secs(600),
        writable_workspace,
        &writable_paths,
    )
    .map(|launcher| LinuxSandboxCommand {
        program: launcher.program,
        args: launcher.args,
        env: Vec::new(),
    })
}

/// Build the canonical OS-enforced launcher for an arbitrary workspace child.
/// Unlike the legacy convenience wrapper above, this keeps the workspace root
/// and command cwd distinct so Agent Runtime, foreground shell, and managed
/// workers all use the same backend contract.
pub fn build_confined_command_launcher(
    workspace_root: &Path,
    cwd: &Path,
    command: &str,
    timeout: Duration,
    writable_workspace: bool,
    writable_paths: &[PathBuf],
) -> Result<LinuxSandboxCommand, String> {
    if sandbox_backend_capability() != EnforcementCapability::Full {
        return Err("sandbox backend is unavailable; command was not executed".into());
    }
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("sandbox workspace is unavailable: {error}"))?;
    let canonical_cwd = cwd
        .canonicalize()
        .map_err(|error| format!("sandbox cwd is unavailable: {error}"))?;
    let relative_cwd = canonical_cwd
        .strip_prefix(&canonical_root)
        .map_err(|_| "sandbox cwd escapes the workspace".to_string())?;
    let sandbox_cwd = Path::new("/workspace").join(relative_cwd);
    crate::sandbox_backend::build_launcher(
        &canonical_root,
        &sandbox_cwd,
        command,
        timeout,
        writable_workspace,
        writable_paths,
    )
    .map(|launcher| LinuxSandboxCommand {
        program: launcher.program,
        args: launcher.args,
        env: Vec::new(),
    })
    .ok_or_else(|| "sandbox launcher could not be constructed; command was not executed".into())
}

fn normalize_mounts(mounts: &[String], cwd: &Path) -> Vec<String> {
    let cwd = cwd.to_path_buf();
    mounts
        .iter()
        .map(|mount| {
            let path = PathBuf::from(mount);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .map(|path| path.display().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        build_linux_sandbox_command, detect_container_environment_from, FilesystemIsolationMode,
        SandboxConfig, SandboxDetectionInputs,
    };
    use std::path::Path;

    #[test]
    fn detects_container_markers_from_multiple_sources() {
        let detected = detect_container_environment_from(SandboxDetectionInputs {
            env_pairs: vec![("container".to_string(), "docker".to_string())],
            dockerenv_exists: true,
            containerenv_exists: false,
            proc_1_cgroup: Some("12:memory:/docker/abc"),
        });

        assert!(detected.in_container);
        assert!(detected
            .markers
            .iter()
            .any(|marker| marker == "/.dockerenv"));
        assert!(detected
            .markers
            .iter()
            .any(|marker| marker == "env:container=docker"));
        assert!(detected
            .markers
            .iter()
            .any(|marker| marker == "/proc/1/cgroup:docker"));
    }

    #[test]
    fn resolves_request_with_overrides() {
        let config = SandboxConfig {
            enabled: Some(true),
            namespace_restrictions: Some(true),
            network_isolation: Some(false),
            filesystem_mode: Some(FilesystemIsolationMode::WorkspaceOnly),
            allowed_mounts: vec!["logs".to_string()],
        };

        let request = config.resolve_request(
            Some(true),
            Some(false),
            Some(true),
            Some(FilesystemIsolationMode::AllowList),
            Some(vec!["tmp".to_string()]),
        );

        assert!(request.enabled);
        assert!(!request.namespace_restrictions);
        assert!(request.network_isolation);
        assert_eq!(request.filesystem_mode, FilesystemIsolationMode::AllowList);
        assert_eq!(request.allowed_mounts, vec!["tmp"]);
    }

    #[test]
    fn builds_linux_launcher_with_network_flag_when_requested() {
        let config = SandboxConfig::default();
        let status = super::resolve_sandbox_status_for_request(
            &config.resolve_request(
                Some(true),
                Some(true),
                Some(true),
                Some(FilesystemIsolationMode::WorkspaceOnly),
                None,
            ),
            Path::new("/workspace"),
        );

        if let Some(launcher) =
            build_linux_sandbox_command("printf hi", Path::new("/workspace"), &status)
        {
            assert_eq!(launcher.program, "prlimit");
            assert!(launcher.args.iter().any(|arg| arg == "bwrap"));
            assert!(launcher.args.iter().any(|arg| arg == "--unshare-all"));
        }
    }

    #[test]
    fn does_not_claim_environment_variables_are_filesystem_isolation() {
        let request = SandboxConfig::default().resolve_request(
            Some(true),
            Some(false),
            Some(false),
            Some(FilesystemIsolationMode::WorkspaceOnly),
            None,
        );
        let status = super::resolve_sandbox_status_for_request(&request, Path::new("/workspace"));

        let full = super::sandbox_backend_capability() == super::EnforcementCapability::Full;
        assert_eq!(status.filesystem_supported, full);
        assert_eq!(status.filesystem_active, full);
        assert_eq!(status.supported, full);
        assert_eq!(status.active, full);
        if !full {
            assert!(status
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("enforcement probe failed")));
        }
    }
}
