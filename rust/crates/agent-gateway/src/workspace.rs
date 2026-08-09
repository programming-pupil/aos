use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{atomic::AtomicBool, Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use glob::Pattern;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use walkdir::{DirEntry, WalkDir};

const ROOTS: [&str; 6] = [
    "uploads",
    "projects",
    "sql-knowledge",
    "history",
    "generated",
    "shared",
];
const MAX_READ_CHARS: usize = 50_000;
const MAX_PROJECT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const SOURCE_PAGE_SIZE: usize = 500;
const MAX_SNAPSHOT_FILES_PER_ROOT: usize = 5_000;
const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_GENERATED_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct WorkspaceAccessContext {
    pub db: sqlx::SqlitePool,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub project_root: PathBuf,
    pub data_root: Option<PathBuf>,
}

type WorkspaceExecutionRegistry = BTreeMap<String, Vec<Weak<AtomicBool>>>;

fn workspace_execution_registry() -> &'static Mutex<WorkspaceExecutionRegistry> {
    static REGISTRY: OnceLock<Mutex<WorkspaceExecutionRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn workspace_execution_key(tenant_id: &str, user_id: &str, session_id: &str) -> String {
    format!("{tenant_id}\0{user_id}\0{session_id}")
}

struct WorkspaceExecutionRegistration {
    key: String,
    cancellation: Arc<AtomicBool>,
}

impl WorkspaceExecutionRegistration {
    fn register(context: &WorkspaceAccessContext) -> Self {
        let key =
            workspace_execution_key(&context.tenant_id, &context.user_id, &context.session_id);
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut registry = workspace_execution_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registrations = registry.entry(key.clone()).or_default();
        registrations.retain(|entry| entry.strong_count() > 0);
        registrations.push(Arc::downgrade(&cancellation));
        drop(registry);
        Self { key, cancellation }
    }
}

impl Drop for WorkspaceExecutionRegistration {
    fn drop(&mut self) {
        let mut registry = workspace_execution_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let remove_key = if let Some(registrations) = registry.get_mut(&self.key) {
            registrations.retain(|entry| {
                entry
                    .upgrade()
                    .is_some_and(|active| !Arc::ptr_eq(&active, &self.cancellation))
            });
            registrations.is_empty()
        } else {
            false
        };
        if remove_key {
            registry.remove(&self.key);
        }
    }
}

pub fn cancel_active_workspace_executions(
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> usize {
    use std::sync::atomic::Ordering;

    let key = workspace_execution_key(tenant_id, user_id, session_id);
    let mut registry = workspace_execution_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(registrations) = registry.get_mut(&key) else {
        return 0;
    };
    let mut cancelled = 0;
    registrations.retain(|entry| {
        if let Some(cancellation) = entry.upgrade() {
            cancellation.store(true, Ordering::Release);
            cancelled += 1;
            true
        } else {
            false
        }
    });
    cancelled
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VirtualPath {
    root: Option<String>,
    segments: Vec<String>,
}

impl VirtualPath {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.chars().count() > 4_096 {
            return Err("workspace path exceeds 4096 characters".to_string());
        }
        if raw.is_empty() || raw == "/" {
            return Ok(Self {
                root: None,
                segments: Vec::new(),
            });
        }
        if raw.chars().any(char::is_control) || raw.contains('\\') {
            return Err("invalid workspace path".to_string());
        }
        let lower = raw.to_ascii_lowercase();
        if ["%2e", "%2f", "%5c", "%00"]
            .iter()
            .any(|encoded| lower.contains(encoded))
        {
            return Err("encoded workspace path separators are not allowed".to_string());
        }
        if !raw.starts_with('/') {
            return Err("workspace paths must start with `/`".to_string());
        }
        let mut parts = Vec::new();
        for part in raw.split('/').skip(1) {
            match part {
                "" | "." => {}
                ".." => return Err("workspace path traversal is not allowed".to_string()),
                value if value.contains(':') => {
                    return Err("workspace paths cannot contain physical path prefixes".to_string())
                }
                value => parts.push(value.to_string()),
            }
        }
        let root = parts.first().cloned();
        if let Some(root) = root.as_deref() {
            if !ROOTS.contains(&root) {
                return Err(format!("unknown workspace root `/{root}`"));
            }
        }
        Ok(Self {
            root,
            segments: parts.into_iter().skip(1).collect(),
        })
    }

    fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    fn segments(&self) -> &[String] {
        &self.segments
    }

    fn display(&self) -> String {
        self.root.as_ref().map_or_else(
            || "/".to_string(),
            |root| {
                if self.segments.is_empty() {
                    format!("/{root}")
                } else {
                    format!("/{root}/{}", self.segments.join("/"))
                }
            },
        )
    }
}

#[derive(Debug, Clone)]
struct WorkspaceItem {
    path: String,
    resource_type: String,
    resource_id: String,
    version: String,
    content_hash: Option<String>,
    size_bytes: u64,
    mime_type: Option<String>,
    updated_at: Option<String>,
    metadata: Value,
}

#[derive(Debug, Clone)]
struct WorkspaceHandle {
    id: String,
    acl_version: String,
}

struct BoundedHitWindow {
    after: Option<String>,
    capacity: usize,
    path_filter: Option<VirtualPath>,
    glob_filter: Option<Pattern>,
    hits: Vec<Value>,
}

struct BoundedItemWindow {
    after: Option<String>,
    capacity: usize,
    items: Vec<WorkspaceItem>,
}

impl BoundedItemWindow {
    fn new(after: Option<String>, capacity: usize) -> Self {
        Self {
            after,
            capacity: capacity.max(1),
            items: Vec::with_capacity(capacity.max(1).saturating_add(1)),
        }
    }

    fn push(&mut self, item: WorkspaceItem) {
        let key = item_sort_key(&item);
        if self.after.as_ref().is_some_and(|after| key <= *after) {
            return;
        }
        self.items.push(item);
        if self.items.len() > self.capacity {
            self.items.sort_by_key(item_sort_key);
            self.items.truncate(self.capacity);
        }
    }

    fn into_items(mut self) -> Vec<WorkspaceItem> {
        self.items.sort_by_key(item_sort_key);
        self.items
    }
}

impl BoundedHitWindow {
    fn new(after: Option<String>, capacity: usize) -> Self {
        Self {
            after,
            capacity: capacity.max(1),
            path_filter: None,
            glob_filter: None,
            hits: Vec::with_capacity(capacity.max(1).saturating_add(1)),
        }
    }

    fn with_filter(
        after: Option<String>,
        capacity: usize,
        path_filter: VirtualPath,
        glob_filter: Option<Pattern>,
    ) -> Self {
        let mut window = Self::new(after, capacity);
        window.path_filter = Some(path_filter);
        window.glob_filter = glob_filter;
        window
    }

    fn push(&mut self, hit: Value) -> bool {
        let Some(path) = hit.get("path").and_then(Value::as_str) else {
            return false;
        };
        if self
            .path_filter
            .as_ref()
            .is_some_and(|filter| !path_matches_prefix(path, filter))
        {
            return false;
        }
        if let Some(pattern) = &self.glob_filter {
            let relative = path.trim_start_matches('/');
            let filename = Path::new(relative)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(relative);
            if !pattern.matches(relative) && !pattern.matches(filename) {
                return false;
            }
        }
        let key = hit_sort_key(&hit);
        if self.after.as_ref().is_some_and(|after| key <= *after) {
            return false;
        }
        if self
            .hits
            .iter()
            .any(|existing| hit_sort_key(existing) == key)
        {
            return false;
        }
        self.hits.push(hit);
        if self.hits.len() > self.capacity {
            self.hits.sort_by_key(hit_sort_key);
            self.hits.truncate(self.capacity);
        }
        true
    }

    fn into_hits(mut self) -> Vec<Value> {
        self.hits.sort_by_key(hit_sort_key);
        self.hits
    }
}

pub(crate) async fn execute_workspace_operation(
    context: &WorkspaceAccessContext,
    operation: &str,
    input: &Value,
) -> Result<String, String> {
    let started = Instant::now();
    let workspace = ensure_workspace(context).await?;
    let path = input.get("path").and_then(Value::as_str).unwrap_or("/");
    let result = match operation {
        "workspace_tree" => workspace_tree(context, &workspace, input).await,
        "workspace_find" => workspace_find(context, &workspace, input).await,
        "workspace_rg" => workspace_rg(context, &workspace, input).await,
        "workspace_read" => workspace_read(context, &workspace.id, input, false).await,
        "workspace_open" => workspace_read(context, &workspace.id, input, true).await,
        "workspace_stat" => workspace_stat(context, &workspace.id, input).await,
        "workspace_execute" => workspace_execute(context, &workspace, input).await,
        _ => Err(format!("unsupported workspace operation: {operation}")),
    };
    let (outcome, denial_code) = match &result {
        Ok(_) => ("success", None),
        Err(error) if error.contains("not found") || error.contains("access denied") => {
            ("denied", Some("not_found_or_denied"))
        }
        Err(_) => ("failed", None),
    };
    audit_usage(
        context,
        &workspace.id,
        operation,
        path,
        outcome,
        denial_code,
        started.elapsed().as_millis(),
    )
    .await;
    result
}

struct WorkspaceSnapshot {
    root: PathBuf,
    generated_baseline: BTreeMap<String, String>,
}

impl Drop for WorkspaceSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn workspace_execute(
    context: &WorkspaceAccessContext,
    workspace: &WorkspaceHandle,
    input: &Value,
) -> Result<String, String> {
    if !crate::workspace_sandbox::isolation_available() {
        return Err("workspace execution isolation is unavailable".to_string());
    }
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "workspace_execute requires non-empty `command`".to_string())?;
    if command.chars().count() > 32_000 {
        return Err("workspace_execute command exceeds 32000 characters".to_string());
    }
    let execution = WorkspaceExecutionRegistration::register(context);
    let cwd = VirtualPath::parse(
        input
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("/projects/session"),
    )?;
    let timeout_secs = input
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .clamp(1, 600);
    let snapshot = materialize_workspace_snapshot(context).await?;
    let relative_cwd = cwd.display().trim_start_matches('/').to_string();
    let host_cwd = snapshot.root.join(&relative_cwd);
    if !host_cwd.is_dir() {
        return Err("workspace_execute cwd not found or access denied".to_string());
    }
    let sandbox_cwd = PathBuf::from("/workspace").join(relative_cwd);
    let output = crate::workspace_sandbox::execute(
        &snapshot.root,
        &sandbox_cwd,
        command,
        Duration::from_secs(timeout_secs),
        execution.cancellation.clone(),
    )?;
    let generated = collect_generated_files(
        &snapshot.root.join("generated"),
        &snapshot.generated_baseline,
    )?;
    let artifact_id = uuid::Uuid::new_v4().to_string();
    let payload = json!({
        "workspaceId": workspace.id,
        "aclVersion": workspace.acl_version,
        "command": command,
        "cwd": cwd.display(),
        "timeoutSeconds": timeout_secs,
        "timedOut": output.timed_out,
        "cancelled": output.cancelled,
        "exitCode": output.exit_code,
        "durationMs": output.duration_ms,
        "stdout": &output.stdout,
        "stderr": &output.stderr,
        "generatedFiles": &generated,
    });
    sqlx::query(
        "INSERT INTO chat_turn_artifacts
            (id, tenant_id, user_id, session_id, artifact_type, payload_json)
         VALUES (?, ?, ?, ?, 'workspace_execution', ?)",
    )
    .bind(&artifact_id)
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .bind(&context.session_id)
    .bind(&payload)
    .execute(&context.db)
    .await
    .map_err(|error| format!("failed to persist workspace execution artifact: {error}"))?;
    serde_json::to_string_pretty(&json!({
        "status": if output.cancelled { "cancelled" } else if output.timed_out { "timed_out" } else if output.exit_code == Some(0) { "succeeded" } else { "failed" },
        "exitCode": output.exit_code,
        "timedOut": output.timed_out,
        "cancelled": output.cancelled,
        "durationMs": output.duration_ms,
        "stdout": truncate_chars(&output.stdout, 50_000),
        "stderr": truncate_chars(&output.stderr, 50_000),
        "artifactRef": format!("/generated/{artifact_id}-workspace_execution.json"),
        "generatedFiles": generated.iter().map(|file| json!({
            "path": generated_child_virtual_path(&artifact_id, file),
            "sizeBytes": file.get("sizeBytes"),
            "sha256": file.get("sha256")
        })).collect::<Vec<_>>()
    }))
    .map_err(|error| format!("failed to serialize workspace execution result: {error}"))
}

async fn materialize_workspace_snapshot(
    context: &WorkspaceAccessContext,
) -> Result<WorkspaceSnapshot, String> {
    let root = std::env::temp_dir().join(format!(
        "aos-workspace-{}-{}",
        safe_path_segment(&context.user_id),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root)
        .map_err(|error| format!("failed to create isolated workspace snapshot: {error}"))?;
    set_directory_permissions(&root, 0o700)?;
    let mut snapshot = WorkspaceSnapshot {
        root,
        generated_baseline: BTreeMap::new(),
    };
    for root_name in ROOTS {
        let directory = snapshot.root.join(root_name);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("failed to initialize snapshot mount: {error}"))?;
        set_directory_permissions(&directory, 0o700)?;
    }
    std::fs::create_dir_all(snapshot.root.join("projects/session"))
        .map_err(|error| format!("failed to initialize project snapshot: {error}"))?;
    let mut total_bytes = 0_u64;
    for root_name in ROOTS {
        let items = list_items(
            context,
            root_name,
            MAX_SNAPSHOT_FILES_PER_ROOT.saturating_add(1),
        )
        .await?;
        if items.len() > MAX_SNAPSHOT_FILES_PER_ROOT {
            return Err(format!(
                "authorized /{root_name} mount exceeds the {MAX_SNAPSHOT_FILES_PER_ROOT}-file execution limit"
            ));
        }
        for item in items {
            let path = VirtualPath::parse(&item.path)?;
            let content = read_item_content(context, &path).await?;
            let content_bytes = u64::try_from(content.len()).unwrap_or(u64::MAX);
            if total_bytes.saturating_add(content_bytes) > MAX_SNAPSHOT_BYTES {
                return Err(
                    "authorized workspace snapshot exceeds the 256 MiB execution limit".to_string(),
                );
            }
            total_bytes = total_bytes.saturating_add(content_bytes);
            let destination = snapshot.root.join(item.path.trim_start_matches('/'));
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create snapshot directory: {error}"))?;
                set_directory_permissions(parent, 0o700)?;
            }
            std::fs::write(&destination, content)
                .map_err(|error| format!("failed to materialize workspace resource: {error}"))?;
            set_file_permissions(&destination, 0o400)?;
            if root_name == "generated" {
                let relative = destination
                    .strip_prefix(snapshot.root.join("generated"))
                    .map_err(|_| "generated snapshot path escaped workspace".to_string())?;
                let bytes = std::fs::read(&destination).map_err(|error| {
                    format!("failed to hash generated snapshot resource: {error}")
                })?;
                let digest = Sha256::digest(bytes);
                snapshot
                    .generated_baseline
                    .insert(slash_path(relative), hex_prefix(&digest, 64));
            }
        }
    }
    Ok(snapshot)
}

async fn read_item_content(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<String, String> {
    read_item_content_with_metadata(context, path)
        .await
        .map(|value| value.0)
}

async fn read_item_content_with_metadata(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<(String, WorkspaceItem), String> {
    match path.root() {
        Some("uploads") => read_upload(context, path).await,
        Some("history") => read_history(context, path).await,
        Some("sql-knowledge") => read_sql_knowledge(context, path).await,
        Some("generated") => read_generated(context, path).await,
        Some("projects") => read_project(context, path),
        Some("shared") => read_shared(context, path).await,
        _ => Err("workspace resource is not materializable".to_string()),
    }
}

fn collect_generated_files(
    root: &Path,
    baseline: &BTreeMap<String, String>,
) -> Result<Vec<Value>, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("generated workspace is unavailable: {error}"))?;
    let mut total_bytes = 0_u64;
    let mut files = Vec::new();
    for entry in WalkDir::new(&canonical_root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let canonical = entry
            .path()
            .canonicalize()
            .map_err(|error| format!("failed to inspect generated file: {error}"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err("generated file escaped the isolated workspace".to_string());
        }
        let bytes = std::fs::read(&canonical)
            .map_err(|error| format!("failed to read generated file: {error}"))?;
        let relative = canonical
            .strip_prefix(&canonical_root)
            .map_err(|_| "generated file escaped the isolated workspace".to_string())?;
        let digest = Sha256::digest(&bytes);
        let relative_path = slash_path(relative);
        let digest_hex = hex_prefix(&digest, 64);
        if baseline.get(&relative_path) == Some(&digest_hex) {
            continue;
        }
        total_bytes = total_bytes.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if total_bytes > MAX_GENERATED_ARTIFACT_BYTES {
            return Err("generated artifacts exceed the 16 MiB persistence limit".to_string());
        }
        files.push(json!({
            "path": format!("/generated/{relative_path}"),
            "sizeBytes": bytes.len(),
            "sha256": digest_hex,
            "contentBase64": STANDARD.encode(bytes),
        }));
    }
    Ok(files)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("failed to secure workspace directory: {error}"))
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path, _mode: u32) -> Result<(), String> {
    Err("workspace execution snapshots require Unix permissions".to_string())
}

fn set_file_permissions(path: &Path, mode: u32) -> Result<(), String> {
    set_directory_permissions(path, mode)
}

async fn ensure_workspace(context: &WorkspaceAccessContext) -> Result<WorkspaceHandle, String> {
    let digest = Sha256::digest(format!("{}:{}", context.tenant_id, context.user_id).as_bytes());
    let workspace_id = format!("ws-{}", hex_prefix(&digest, 16));
    sqlx::query(
        "INSERT INTO agent_workspaces
            (id, tenant_id, owner_user_id, workspace_type, visibility, enabled, acl_version)
         VALUES (?, ?, ?, 'personal', 'private', 1, 1)
         ON CONFLICT DO UPDATE SET enabled = 1",
    )
    .bind(&workspace_id)
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .execute(&context.db)
    .await
    .map_err(|error| format!("failed to initialize workspace: {error}"))?;

    for root in ROOTS {
        let mount_id = format!("mount-{}-{root}", hex_prefix(&digest, 12));
        sqlx::query(
            "INSERT INTO agent_workspace_mounts
                (id, tenant_id, workspace_id, virtual_root, resource_type, selector_json, enabled)
             VALUES (?, ?, ?, ?, ?, JSON_OBJECT('managed', true), 1)
             ON CONFLICT DO UPDATE SET resource_type = excluded.resource_type, enabled = 1",
        )
        .bind(mount_id)
        .bind(&context.tenant_id)
        .bind(&workspace_id)
        .bind(format!("/{root}"))
        .bind(root)
        .execute(&context.db)
        .await
        .map_err(|error| format!("failed to initialize workspace mount: {error}"))?;
    }
    let row = sqlx::query(
        "SELECT w.acl_version
         FROM agent_workspaces w
         WHERE w.id = ? AND w.tenant_id = ? AND w.owner_user_id = ? AND w.enabled = 1",
    )
    .bind(&workspace_id)
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .fetch_one(&context.db)
    .await
    .map_err(|error| format!("failed to resolve workspace ACL version: {error}"))?;
    let acl_version = row.try_get::<u64, _>("acl_version").unwrap_or(1);
    let owned_shared_rows = sqlx::query(
        "SELECT id, version, enabled, is_current,
                CAST(COALESCE(deleted_at, '1970-01-01') AS TEXT) AS deleted_at,
                CAST(updated_at AS TEXT) AS updated_at
         FROM agent_workspace_entries
         WHERE tenant_id = ? AND owner_user_id = ? AND visibility = 'tenant_shared'
         ORDER BY id ASC",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .fetch_all(&context.db)
    .await
    .map_err(|error| format!("failed to fingerprint owned shared workspace entries: {error}"))?;
    let grant_rows = sqlx::query(
        "SELECT id, workspace_id, COALESCE(entry_id, '') AS entry_id_fingerprint,
                resource_id, permission, enabled,
                CAST(COALESCE(revoked_at, '1970-01-01') AS TEXT) AS revoked_at,
                CAST(updated_at AS TEXT) AS updated_at
         FROM agent_workspace_grants
         WHERE tenant_id = ? AND grantee_user_id = ?
         ORDER BY id ASC",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .fetch_all(&context.db)
    .await
    .map_err(|error| format!("failed to fingerprint workspace grants: {error}"))?;
    let mut grant_hasher = Sha256::new();
    for entry in owned_shared_rows {
        for field in ["id", "version", "deleted_at", "updated_at"] {
            grant_hasher.update(entry.get::<String, _>(field));
            grant_hasher.update([0]);
        }
        grant_hasher.update([u8::from(entry.get::<i8, _>("enabled") != 0)]);
        grant_hasher.update([u8::from(entry.get::<i8, _>("is_current") != 0)]);
    }
    grant_hasher.update([0xff]);
    for grant in grant_rows {
        for field in [
            "id",
            "workspace_id",
            "entry_id_fingerprint",
            "resource_id",
            "permission",
            "revoked_at",
            "updated_at",
        ] {
            grant_hasher.update(grant.get::<String, _>(field));
            grant_hasher.update([0]);
        }
        grant_hasher.update([u8::from(grant.get::<i8, _>("enabled") != 0)]);
    }
    let grant_fingerprint = hex_prefix(&grant_hasher.finalize(), 24);
    Ok(WorkspaceHandle {
        id: workspace_id,
        acl_version: format!("{acl_version}:{grant_fingerprint}"),
    })
}

async fn workspace_tree(
    context: &WorkspaceAccessContext,
    workspace: &WorkspaceHandle,
    input: &Value,
) -> Result<String, String> {
    let path = VirtualPath::parse(input.get("path").and_then(Value::as_str).unwrap_or("/"))?;
    let limit = bounded_limit(input, 80, 200);
    if path.root().is_none() {
        let scope = cursor_scope(context, workspace, "workspace_tree", input);
        let after = decode_cursor(input, &scope)?;
        let mut roots = selected_roots(input, None)
            .into_iter()
            .filter(|root| {
                after
                    .as_ref()
                    .is_none_or(|after| root.as_str() > after.as_str())
            })
            .collect::<Vec<_>>();
        roots.sort();
        let has_more = roots.len() > limit;
        roots.truncate(limit);
        let next_cursor = if has_more {
            roots
                .last()
                .map(|key| encode_cursor(&scope, key))
                .transpose()?
        } else {
            None
        };
        return serde_json::to_string_pretty(&json!({
            "path": "/",
            "entries": roots.iter().map(|root| json!({
                "path": format!("/{root}"),
                "name": root,
                "kind": "directory"
            })).collect::<Vec<_>>(),
            "nextCursor": next_cursor
        }))
        .map_err(|error| error.to_string());
    }
    let depth = input
        .get("depth")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1)
        .clamp(1, 8);
    let scope = cursor_scope(context, workspace, "workspace_tree", input);
    let after = decode_cursor(input, &scope)?;
    let mut window = BoundedHitWindow::new(after, limit.saturating_add(1));
    let root = path
        .root()
        .ok_or_else(|| "workspace_tree requires a rooted virtual path".to_string())?;
    scan_items(context, root, &mut |item| {
        if path_matches_prefix(&item.path, &path) {
            for entry in tree_entries(std::slice::from_ref(&item), &path, depth) {
                window.push(entry);
            }
        }
        true
    })
    .await?;
    let entries = window.into_hits();
    let (entries, next_cursor) = paginate_hits(entries, input, limit, &scope)?;
    let projected = entries.iter().filter_map(hit_to_item).collect::<Vec<_>>();
    project_items(context, &workspace.id, &projected).await?;
    serde_json::to_string_pretty(&json!({
        "path": path.display(),
        "depth": depth,
        "entries": entries,
        "count": entries.len(),
        "nextCursor": next_cursor
    }))
    .map_err(|error| error.to_string())
}

async fn workspace_find(
    context: &WorkspaceAccessContext,
    workspace: &WorkspaceHandle,
    input: &Value,
) -> Result<String, String> {
    let path = VirtualPath::parse(input.get("path").and_then(Value::as_str).unwrap_or("/"))?;
    let glob = input
        .get("glob")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "workspace_find requires `glob`".to_string())?;
    if glob.chars().count() > 512 {
        return Err("workspace glob exceeds 512 characters".to_string());
    }
    let pattern = Pattern::new(glob).map_err(|error| format!("invalid glob: {error}"))?;
    let limit = bounded_limit(input, 80, 200);
    let roots = selected_roots(input, path.root());
    let scope = cursor_scope(context, workspace, "workspace_find", input);
    let after = decode_cursor(input, &scope)?;
    let mut matches = BoundedItemWindow::new(after, limit.saturating_add(1));
    for root in roots {
        scan_items(context, &root, &mut |item| {
            if path_matches_prefix(&item.path, &path) {
                let relative = item.path.trim_start_matches('/');
                let filename = Path::new(relative)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or(relative);
                if pattern.matches(relative) || pattern.matches(filename) {
                    matches.push(item);
                }
            }
            true
        })
        .await?;
    }
    let matches = matches.into_items();
    let (matches, next_cursor) = paginate_items(matches, input, limit, &scope)?;
    project_items(context, &workspace.id, &matches).await?;
    serde_json::to_string_pretty(&json!({
        "path": path.display(),
        "glob": glob,
        "items": matches.iter().map(item_json).collect::<Vec<_>>(),
        "count": matches.len(),
        "nextCursor": next_cursor
    }))
    .map_err(|error| error.to_string())
}

async fn workspace_rg(
    context: &WorkspaceAccessContext,
    workspace: &WorkspaceHandle,
    input: &Value,
) -> Result<String, String> {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "workspace_rg requires non-empty `query`".to_string())?;
    if query.chars().count() > 1_000 {
        return Err("workspace search query exceeds 1000 characters".to_string());
    }
    let path = VirtualPath::parse(input.get("path").and_then(Value::as_str).unwrap_or("/"))?;
    let regex = input
        .get("regex")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| Regex::new(query).map_err(|error| format!("invalid regex: {error}")))
        .transpose()?;
    let limit = bounded_limit(input, 20, 100);
    let context_lines = input
        .get("contextLines")
        .or_else(|| input.get("context_lines"))
        .and_then(Value::as_u64)
        .unwrap_or(2)
        .min(10) as usize;
    let glob = input
        .get("glob")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.chars().count() > 512 {
                Err("workspace glob exceeds 512 characters".to_string())
            } else {
                Ok(value)
            }
        })
        .transpose()?
        .map(Pattern::new)
        .transpose()
        .map_err(|error| format!("invalid glob: {error}"))?;
    let roots = selected_roots(input, path.root());
    let scope = cursor_scope(context, workspace, "workspace_rg", input);
    let after = decode_cursor(input, &scope)?;
    let mut hits =
        BoundedHitWindow::with_filter(after, limit.saturating_add(1), path.clone(), glob.clone());
    for root in roots {
        match root.as_str() {
            "uploads" => search_uploads(context, query, regex.as_ref(), &mut hits).await?,
            "history" => search_history(context, query, regex.as_ref(), &mut hits).await?,
            "sql-knowledge" => {
                search_sql_knowledge(context, query, regex.as_ref(), &mut hits).await?
            }
            "generated" => search_generated(context, query, regex.as_ref(), &mut hits).await?,
            "projects" => search_project(
                context,
                query,
                regex.as_ref(),
                &path,
                glob.as_ref(),
                context_lines,
                &mut hits,
            )?,
            "shared" => search_shared(context, query, regex.as_ref(), &mut hits).await?,
            _ => {}
        }
    }
    let hits = hits.into_hits();
    let (hits, next_cursor) = paginate_hits(hits, input, limit, &scope)?;
    let projected = hits.iter().filter_map(hit_to_item).collect::<Vec<_>>();
    project_items(context, &workspace.id, &projected).await?;
    serde_json::to_string_pretty(&json!({
        "query": query,
        "path": path.display(),
        "regex": regex.is_some(),
        "items": hits,
        "count": hits.len(),
        "nextCursor": next_cursor,
        "guidance": "Open promising matches with workspace_open before relying on long SQL, code, or documents."
    }))
    .map_err(|error| error.to_string())
}

async fn workspace_read(
    context: &WorkspaceAccessContext,
    workspace_id: &str,
    input: &Value,
    wide: bool,
) -> Result<String, String> {
    let path = VirtualPath::parse(
        input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "workspace_read requires `path`".to_string())?,
    )?;
    let root = path
        .root()
        .ok_or_else(|| "workspace_read requires a file path".to_string())?;
    let max_chars = input
        .get("maxChars")
        .or_else(|| input.get("max_chars"))
        .and_then(Value::as_u64)
        .unwrap_or(if wide { 50_000 } else { 20_000 })
        .clamp(1, MAX_READ_CHARS as u64) as usize;
    let mut line_start = input
        .get("lineStart")
        .or_else(|| input.get("line_start"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let mut line_end = input
        .get("lineEnd")
        .or_else(|| input.get("line_end"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    if wide && line_start.is_none() && line_end.is_none() {
        if let Some(around) = input
            .get("aroundLine")
            .or_else(|| input.get("around_line"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        {
            let context_lines = input
                .get("contextLines")
                .or_else(|| input.get("context_lines"))
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(80)
                .min(500);
            line_start = Some(around.saturating_sub(context_lines).max(1));
            line_end = Some(around.saturating_add(context_lines));
        }
    }
    let (content, item) = match root {
        "uploads" => read_upload(context, &path).await?,
        "history" => read_history(context, &path).await?,
        "sql-knowledge" => read_sql_knowledge(context, &path).await?,
        "generated" => read_generated(context, &path).await?,
        "projects" => read_project(context, &path)?,
        "shared" => read_shared(context, &path).await?,
        _ => return Err("workspace path is not readable".to_string()),
    };
    project_items(context, workspace_id, std::slice::from_ref(&item)).await?;
    let (start, end, selected, truncated) = slice_lines(&content, line_start, line_end, max_chars);
    serde_json::to_string_pretty(&json!({
        "path": item.path,
        "resourceType": item.resource_type,
        "resourceId": item.resource_id,
        "version": item.version,
        "lines": [start, end],
        "truncated": truncated,
        "content": selected
    }))
    .map_err(|error| error.to_string())
}

async fn workspace_stat(
    context: &WorkspaceAccessContext,
    workspace_id: &str,
    input: &Value,
) -> Result<String, String> {
    let path = VirtualPath::parse(
        input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "workspace_stat requires `path`".to_string())?,
    )?;
    if path.root().is_none() {
        return Err("workspace_stat requires a file path".to_string());
    }
    let (_, item) = read_item_content_with_metadata(context, &path).await?;
    project_items(context, workspace_id, std::slice::from_ref(&item)).await?;
    let mut value = item_json(&item);
    value["executionAvailable"] = Value::Bool(crate::workspace_sandbox::isolation_available());
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

async fn list_items(
    context: &WorkspaceAccessContext,
    root: &str,
    limit: usize,
) -> Result<Vec<WorkspaceItem>, String> {
    let mut items = Vec::new();
    scan_items(context, root, &mut |item| {
        items.push(item);
        items.len() < limit
    })
    .await?;
    items.truncate(limit);
    Ok(items)
}

async fn scan_items(
    context: &WorkspaceAccessContext,
    root: &str,
    visitor: &mut dyn FnMut(WorkspaceItem) -> bool,
) -> Result<(), String> {
    match root {
        "uploads" => scan_uploads(context, visitor).await,
        "history" => scan_history(context, visitor).await,
        "sql-knowledge" => scan_sql_knowledge(context, visitor).await,
        "generated" => scan_generated(context, visitor).await,
        "projects" => scan_project(context, visitor),
        "shared" => scan_shared(context, visitor).await,
        _ => Ok(()),
    }
}

async fn scan_uploads(
    context: &WorkspaceAccessContext,
    visitor: &mut dyn FnMut(WorkspaceItem) -> bool,
) -> Result<(), String> {
    let mut after = String::new();
    loop {
        let page_size = SOURCE_PAGE_SIZE;
        let rows = sqlx::query(
            "SELECT file_id, filename, media_type, CAST(size_bytes AS INTEGER) AS size_bytes,
                    status, CAST(updated_at AS TEXT) AS updated_at
             FROM chat_file_workspace_files
             WHERE tenant_id = ? AND user_id = ? AND (session_id = ? OR session_id IS NULL)
               AND status IN ('uploaded','indexed','parsing') AND file_id > ?
             ORDER BY file_id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(&after)
        .bind(i64::try_from(page_size).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to list uploads: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        after = rows
            .last()
            .map(|row| row.get::<String, _>("file_id"))
            .unwrap_or_default();
        for row in rows {
            let id = row.get::<String, _>("file_id");
            let name = safe_virtual_leaf(&row.get::<String, _>("filename"));
            let updated = row.get::<String, _>("updated_at");
            if !visitor(WorkspaceItem {
                path: format!("/uploads/{id}/{name}"),
                resource_type: "upload".to_string(),
                resource_id: id,
                version: updated.clone(),
                content_hash: None,
                size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
                mime_type: Some(row.get::<String, _>("media_type")),
                updated_at: Some(updated),
                metadata: json!({"status": row.get::<String, _>("status")}),
            }) {
                return Ok(());
            }
        }
        if row_count < page_size {
            break;
        }
    }
    Ok(())
}

async fn scan_history(
    context: &WorkspaceAccessContext,
    visitor: &mut dyn FnMut(WorkspaceItem) -> bool,
) -> Result<(), String> {
    let mut after = String::new();
    loop {
        let page_size = SOURCE_PAGE_SIZE;
        let rows = sqlx::query(
            "SELECT id, window_id, role, content_kind, char_count, content_hash,
                    CAST(created_at AS TEXT) AS created_at
             FROM agent_context_archives
             WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND id > ?
             ORDER BY id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(&after)
        .bind(i64::try_from(page_size).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to list history: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        after = rows
            .last()
            .map(|row| row.get::<String, _>("id"))
            .unwrap_or_default();
        for row in rows {
            let id = row.get::<String, _>("id");
            if !visitor(WorkspaceItem {
                path: format!("/history/{}/{id}.md", context.session_id),
                resource_type: "history".to_string(),
                resource_id: id,
                version: row.get::<String, _>("content_hash"),
                content_hash: Some(row.get::<String, _>("content_hash")),
                size_bytes: row.try_get::<u64, _>("char_count").unwrap_or(0),
                mime_type: Some("text/markdown".to_string()),
                updated_at: Some(row.get::<String, _>("created_at")),
                metadata: json!({
                    "windowId": row.get::<String, _>("window_id"),
                    "role": row.get::<String, _>("role"),
                    "contentKind": row.get::<String, _>("content_kind")
                }),
            }) {
                return Ok(());
            }
        }
        if row_count < page_size {
            break;
        }
    }
    Ok(())
}

async fn scan_sql_knowledge(
    context: &WorkspaceAccessContext,
    visitor: &mut dyn FnMut(WorkspaceItem) -> bool,
) -> Result<(), String> {
    let admin = is_admin(context).await?;
    let mut after = String::new();
    loop {
        let page_size = SOURCE_PAGE_SIZE;
        let rows = sqlx::query(
            "SELECT f.id, f.pack_id, f.filename, f.media_type,
                    CAST(f.size_bytes AS INTEGER) AS size_bytes, f.content_hash,
                    CAST(f.updated_at AS TEXT) AS updated_at, p.scope, p.verified
             FROM nl2sql_reference_files f
             JOIN nl2sql_reference_packs p ON p.tenant_id = f.tenant_id AND p.id = f.pack_id
             LEFT JOIN data_sources ds ON ds.tenant_id = f.tenant_id AND ds.id = f.datasource_id
             WHERE f.tenant_id = ? AND p.enabled = 1 AND f.status = 'indexed'
               AND (p.user_id = ? OR p.scope = 'tenant' OR ds.user_id = ? OR ?)
               AND f.id > ?
             ORDER BY f.id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.user_id)
        .bind(admin)
        .bind(&after)
        .bind(i64::try_from(page_size).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to list SQL knowledge: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        after = rows
            .last()
            .map(|row| row.get::<String, _>("id"))
            .unwrap_or_default();
        for row in rows {
            let id = row.get::<String, _>("id");
            let pack_id = row.get::<String, _>("pack_id");
            let name = safe_virtual_leaf(&row.get::<String, _>("filename"));
            if !visitor(WorkspaceItem {
                path: format!("/sql-knowledge/{pack_id}/{id}/{name}"),
                resource_type: "sql_knowledge".to_string(),
                resource_id: id,
                version: row.get::<String, _>("content_hash"),
                content_hash: Some(row.get::<String, _>("content_hash")),
                size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
                mime_type: row.get::<Option<String>, _>("media_type"),
                updated_at: Some(row.get::<String, _>("updated_at")),
                metadata: json!({
                    "packId": pack_id,
                    "scope": row.get::<String, _>("scope"),
                    "verified": row.get::<i8, _>("verified") != 0
                }),
            }) {
                return Ok(());
            }
        }
        if row_count < page_size {
            break;
        }
    }
    Ok(())
}

async fn scan_generated(
    context: &WorkspaceAccessContext,
    visitor: &mut dyn FnMut(WorkspaceItem) -> bool,
) -> Result<(), String> {
    let mut after = String::new();
    loop {
        let page_size = SOURCE_PAGE_SIZE;
        let rows = sqlx::query(
            "SELECT id, artifact_type, CAST(payload_json AS TEXT) AS content,
                    LENGTH(CAST(payload_json AS TEXT)) AS size_bytes,
                    CAST(created_at AS TEXT) AS created_at
             FROM chat_turn_artifacts
             WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND id > ?
             ORDER BY id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(&after)
        .bind(i64::try_from(page_size).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to list generated artifacts: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        after = rows
            .last()
            .map(|row| row.get::<String, _>("id"))
            .unwrap_or_default();
        for row in rows {
            let id = row.get::<String, _>("id");
            let artifact_type = row.get::<String, _>("artifact_type");
            let created = row.get::<String, _>("created_at");
            let content = row.get::<String, _>("content");
            if !visitor(WorkspaceItem {
                path: format!("/generated/{id}-{}.json", safe_virtual_leaf(&artifact_type)),
                resource_type: "generated".to_string(),
                resource_id: id.clone(),
                version: created.clone(),
                content_hash: Some(content_digest(&content)),
                size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
                mime_type: Some("application/json".to_string()),
                updated_at: Some(created.clone()),
                metadata: json!({"artifactType": artifact_type}),
            }) {
                return Ok(());
            }
            if artifact_type == "workspace_execution" {
                for file in generated_text_files(&content) {
                    if !visitor(WorkspaceItem {
                        path: generated_child_virtual_path(&id, &file),
                        resource_type: "generated_file".to_string(),
                        resource_id: id.clone(),
                        version: file
                            .get("sha256")
                            .and_then(Value::as_str)
                            .unwrap_or(&created)
                            .to_string(),
                        content_hash: file
                            .get("sha256")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        size_bytes: file.get("sizeBytes").and_then(Value::as_u64).unwrap_or(0),
                        mime_type: Some("text/plain".to_string()),
                        updated_at: Some(created.clone()),
                        metadata: json!({
                            "artifactType": artifact_type,
                            "generatedPath": file.get("path"),
                        }),
                    }) {
                        return Ok(());
                    }
                }
            }
        }
        if row_count < page_size {
            break;
        }
    }
    Ok(())
}

fn scan_project(
    context: &WorkspaceAccessContext,
    visitor: &mut dyn FnMut(WorkspaceItem) -> bool,
) -> Result<(), String> {
    let canonical_root = context
        .project_root
        .canonicalize()
        .map_err(|error| format!("project workspace unavailable: {error}"))?;
    for entry in WalkDir::new(&canonical_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(project_entry_allowed)
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| format!("failed to stat project file: {error}"))?;
        if metadata.len() > MAX_PROJECT_FILE_BYTES {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&canonical_root)
            .map_err(|_| "project path escaped workspace".to_string())?;
        let resource_id = project_resource_id(relative);
        let relative = slash_path(relative);
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |value| value.as_secs());
        if !visitor(WorkspaceItem {
            path: format!("/projects/session/{relative}"),
            resource_type: "project".to_string(),
            resource_id,
            version: format!("{modified}:{}", metadata.len()),
            content_hash: None,
            size_bytes: metadata.len(),
            mime_type: Some("text/plain".to_string()),
            updated_at: Some(modified.to_string()),
            metadata: json!({}),
        }) {
            return Ok(());
        }
    }
    Ok(())
}

async fn scan_shared(
    context: &WorkspaceAccessContext,
    visitor: &mut dyn FnMut(WorkspaceItem) -> bool,
) -> Result<(), String> {
    let mut after_path = String::new();
    let mut after_resource = String::new();
    loop {
        let page_size = SOURCE_PAGE_SIZE;
        let rows = sqlx::query(
            "SELECT e.resource_id, e.resource_type, e.virtual_path, e.version,
                    e.content_hash, CAST(e.size_bytes AS INTEGER) AS size_bytes,
                    e.mime_type, CAST(e.updated_at AS TEXT) AS updated_at,
                    COALESCE(e.metadata_json, JSON_OBJECT()) AS metadata_json
             FROM agent_workspace_entries e
             WHERE e.tenant_id = ? AND e.visibility = 'tenant_shared'
               AND e.enabled = 1 AND e.is_current = 1 AND e.deleted_at IS NULL
               AND (e.owner_user_id = ? OR EXISTS (
                    SELECT 1 FROM agent_workspace_grants g
                    WHERE g.tenant_id = e.tenant_id AND g.workspace_id = e.workspace_id
                      AND g.entry_id = e.id AND g.grantee_user_id = ?
                      AND g.enabled = 1 AND g.revoked_at IS NULL))
               AND (e.virtual_path > ? OR (e.virtual_path = ? AND e.resource_id > ?))
             ORDER BY e.virtual_path ASC, e.resource_id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.user_id)
        .bind(&after_path)
        .bind(&after_path)
        .bind(&after_resource)
        .bind(i64::try_from(page_size).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to list shared workspace: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        if let Some(last) = rows.last() {
            after_path = last.get::<String, _>("virtual_path");
            after_resource = last.get::<String, _>("resource_id");
        }
        for row in rows {
            let path = normalize_shared_path(&row.get::<String, _>("virtual_path"));
            let Ok(parsed) = VirtualPath::parse(&path) else {
                continue;
            };
            if parsed.root() != Some("shared") {
                continue;
            }
            if !visitor(WorkspaceItem {
                path,
                resource_type: row.get::<String, _>("resource_type"),
                resource_id: row.get::<String, _>("resource_id"),
                version: row.get::<String, _>("version"),
                content_hash: row.get::<Option<String>, _>("content_hash"),
                size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
                mime_type: row.get::<Option<String>, _>("mime_type"),
                updated_at: Some(row.get::<String, _>("updated_at")),
                metadata: row.get::<Value, _>("metadata_json"),
            }) {
                return Ok(());
            }
        }
        if row_count < page_size {
            break;
        }
    }
    Ok(())
}

async fn search_uploads(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    let prefilter = search_prefilter(query, regex);
    let mut after_file = String::new();
    let mut after_chunk = -1_i32;
    loop {
        let rows = sqlx::query(
            "SELECT c.file_id, f.filename, c.chunk_index, c.line_start, c.line_end, c.content,
                    CAST(f.updated_at AS TEXT) AS updated_at
             FROM chat_file_workspace_chunks c
             JOIN chat_file_workspace_files f
               ON f.tenant_id = c.tenant_id AND f.user_id = c.user_id AND f.file_id = c.file_id
             WHERE c.tenant_id = ? AND c.user_id = ? AND f.status = 'indexed'
               AND (f.session_id = ? OR f.session_id IS NULL)
               AND (c.content LIKE ('%' || ? || '%') OR f.filename LIKE ('%' || ? || '%'))
               AND (c.file_id > ? OR (c.file_id = ? AND c.chunk_index > ?))
             ORDER BY c.file_id ASC, c.chunk_index ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(&prefilter)
        .bind(&prefilter)
        .bind(&after_file)
        .bind(&after_file)
        .bind(after_chunk)
        .bind(i64::try_from(SOURCE_PAGE_SIZE).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to search uploads: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        if let Some(last) = rows.last() {
            after_file = last.get::<String, _>("file_id");
            after_chunk = last.get::<i32, _>("chunk_index");
        }
        for row in rows {
            let content = row.get::<String, _>("content");
            let filename = row.get::<String, _>("filename");
            if !text_matches(&content, query, regex) && !text_matches(&filename, query, regex) {
                continue;
            }
            let id = row.get::<String, _>("file_id");
            let name = safe_virtual_leaf(&filename);
            hits.push(json!({
                "path": format!("/uploads/{id}/{name}"),
                "resourceType": "upload",
                "resourceId": id,
                "version": row.get::<String, _>("updated_at"),
                "lineStart": row.get::<Option<i32>, _>("line_start"),
                "lineEnd": row.get::<Option<i32>, _>("line_end"),
                "excerpt": excerpt(&content, query, regex, 1200)
            }));
        }
        if row_count < SOURCE_PAGE_SIZE {
            break;
        }
    }
    Ok(())
}

async fn search_history(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    let prefilter = search_prefilter(query, regex);
    let mut after = String::new();
    loop {
        let rows = sqlx::query(
            "SELECT id, content, content_hash
             FROM agent_context_archives
             WHERE tenant_id = ? AND user_id = ? AND session_id = ?
               AND content LIKE ('%' || ? || '%') AND id > ?
             ORDER BY id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(&prefilter)
        .bind(&after)
        .bind(i64::try_from(SOURCE_PAGE_SIZE).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to search history: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        after = rows
            .last()
            .map(|row| row.get::<String, _>("id"))
            .unwrap_or_default();
        for row in rows {
            let content = row.get::<String, _>("content");
            if !text_matches(&content, query, regex) {
                continue;
            }
            let id = row.get::<String, _>("id");
            let line = matching_line_number(&content, query, regex);
            hits.push(json!({
                "path": format!("/history/{}/{id}.md", context.session_id),
                "resourceType": "history",
                "resourceId": id,
                "version": row.get::<String, _>("content_hash"),
                "lineStart": line,
                "lineEnd": line,
                "excerpt": excerpt(&content, query, regex, 1200)
            }));
        }
        if row_count < SOURCE_PAGE_SIZE {
            break;
        }
    }
    Ok(())
}

async fn search_sql_knowledge(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    let admin = is_admin(context).await?;
    let prefilter = search_prefilter(query, regex);
    let mut after_file = String::new();
    // The first-page predicate is already satisfied by file_id > '', so zero is
    // a safe cursor sentinel for the non-negative chunk index.
    let mut after_chunk = 0_u32;
    loop {
        let rows = sqlx::query(
            "SELECT c.file_id, c.chunk_index, c.start_line, c.end_line, c.content_text,
                    c.content_hash, c.keywords_text, f.pack_id, f.filename, p.verified
             FROM nl2sql_reference_chunks c
             JOIN nl2sql_reference_files f ON f.tenant_id = c.tenant_id AND f.id = c.file_id
             JOIN nl2sql_reference_packs p ON p.tenant_id = c.tenant_id AND p.id = c.pack_id
             LEFT JOIN data_sources ds ON ds.tenant_id = f.tenant_id AND ds.id = f.datasource_id
             WHERE c.tenant_id = ? AND p.enabled = 1 AND f.status = 'indexed'
               AND (p.user_id = ? OR p.scope = 'tenant' OR ds.user_id = ? OR ?)
               AND (c.content_text LIKE ('%' || ? || '%')
                    OR c.keywords_text LIKE ('%' || ? || '%')
                    OR f.filename LIKE ('%' || ? || '%'))
               AND (c.file_id > ? OR (c.file_id = ? AND c.chunk_index > ?))
             ORDER BY c.file_id ASC, c.chunk_index ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.user_id)
        .bind(admin)
        .bind(&prefilter)
        .bind(&prefilter)
        .bind(&prefilter)
        .bind(&after_file)
        .bind(&after_file)
        .bind(after_chunk)
        .bind(i64::try_from(SOURCE_PAGE_SIZE).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to search SQL knowledge: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        if let Some(last) = rows.last() {
            after_file = last.get::<String, _>("file_id");
            after_chunk = last
                .try_get::<u32, _>("chunk_index")
                .map_err(|error| format!("failed to decode SQL knowledge chunk cursor: {error}"))?;
        }
        for row in rows {
            let content = row.get::<String, _>("content_text");
            let keywords = row
                .get::<Option<String>, _>("keywords_text")
                .unwrap_or_default();
            let filename = row.get::<String, _>("filename");
            if !text_matches(&content, query, regex)
                && !text_matches(&keywords, query, regex)
                && !text_matches(&filename, query, regex)
            {
                continue;
            }
            let id = row.get::<String, _>("file_id");
            let pack_id = row.get::<String, _>("pack_id");
            let name = safe_virtual_leaf(&filename);
            hits.push(json!({
                "path": format!("/sql-knowledge/{pack_id}/{id}/{name}"),
                "resourceType": "sql_knowledge",
                "resourceId": id,
                "version": row.get::<String, _>("content_hash"),
                "lineStart": row.get::<u32, _>("start_line"),
                "lineEnd": row.get::<u32, _>("end_line"),
                "verified": row.get::<i8, _>("verified") != 0,
                "excerpt": excerpt(&content, query, regex, 1800)
            }));
        }
        if row_count < SOURCE_PAGE_SIZE {
            break;
        }
    }
    Ok(())
}

async fn search_generated(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    let prefilter = search_prefilter(query, regex);
    let mut after = String::new();
    loop {
        let rows = sqlx::query(
            "SELECT id, artifact_type, CAST(payload_json AS TEXT) AS content,
                    CAST(created_at AS TEXT) AS created_at
             FROM chat_turn_artifacts
             WHERE tenant_id = ? AND user_id = ? AND session_id = ?
               AND (artifact_type = 'workspace_execution'
                    OR CAST(payload_json AS TEXT) LIKE ('%' || ? || '%')) AND id > ?
             ORDER BY id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(&prefilter)
        .bind(&after)
        .bind(i64::try_from(SOURCE_PAGE_SIZE).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to search generated artifacts: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        after = rows
            .last()
            .map(|row| row.get::<String, _>("id"))
            .unwrap_or_default();
        for row in rows {
            let content = row.get::<String, _>("content");
            let id = row.get::<String, _>("id");
            let artifact_type = row.get::<String, _>("artifact_type");
            let created = row.get::<String, _>("created_at");
            if text_matches(&content, query, regex) {
                let line = matching_line_number(&content, query, regex);
                hits.push(json!({
                    "path": format!("/generated/{id}-{}.json", safe_virtual_leaf(&artifact_type)),
                    "resourceType": "generated",
                    "resourceId": id,
                    "version": created,
                    "lineStart": line,
                    "lineEnd": line,
                    "excerpt": excerpt(&content, query, regex, 1200)
                }));
            }
            if artifact_type == "workspace_execution" {
                for file in generated_text_files(&content) {
                    let Some(text) = generated_file_text(&file) else {
                        continue;
                    };
                    if !text_matches(&text, query, regex) {
                        continue;
                    }
                    let line = matching_line_number(&text, query, regex);
                    hits.push(json!({
                        "path": generated_child_virtual_path(&id, &file),
                        "resourceType": "generated_file",
                        "resourceId": id,
                        "version": file.get("sha256").and_then(Value::as_str).unwrap_or(&created),
                        "lineStart": line,
                        "lineEnd": line,
                        "excerpt": excerpt(&text, query, regex, 1200)
                    }));
                }
            }
        }
        if row_count < SOURCE_PAGE_SIZE {
            break;
        }
    }
    Ok(())
}

fn search_project(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    path: &VirtualPath,
    glob: Option<&Pattern>,
    context_lines: usize,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    match search_project_with_rg(context, query, regex, path, glob, context_lines, hits) {
        Err(error) if error.starts_with("rg unavailable:") => {
            search_project_fallback(context, query, regex, path, glob, context_lines, hits)
        }
        result => result,
    }
}

#[allow(clippy::too_many_arguments)]
fn search_project_with_rg(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    path: &VirtualPath,
    glob: Option<&Pattern>,
    context_lines: usize,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    let canonical_root = context
        .project_root
        .canonicalize()
        .map_err(|error| format!("project workspace unavailable: {error}"))?;
    let search_root = requested_project_root(&canonical_root, path)?;
    let mut command = Command::new("rg");
    command
        .arg("--json")
        .arg("--line-number")
        .arg("--sort")
        .arg("path")
        .arg("--no-messages")
        .arg("--hidden")
        .arg("--max-filesize")
        .arg(format!("{MAX_PROJECT_FILE_BYTES}"))
        .arg("--glob")
        .arg("!**/.git/**")
        .arg("--glob")
        .arg("!**/.aos/**")
        .arg("--glob")
        .arg("!**/.aos-*/**")
        .arg("--glob")
        .arg("!**/.sandbox-home/**")
        .arg("--glob")
        .arg("!**/.sandbox-tmp/**");
    if regex.is_none() {
        command.arg("--fixed-strings").arg("--ignore-case");
    }
    if let Some(pattern) = glob {
        command.arg("--glob").arg(pattern.as_str());
    }
    command.arg("--").arg(query).arg(&search_root);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("rg unavailable: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "project rg failed to expose stdout".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "project rg failed to expose stderr".to_string())?;
    let stderr_reader = std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stderr.read_to_string(&mut output);
        output
    });
    let mut accepted_matches = 0_usize;
    let mut stopped_early = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|error| format!("failed to stream project rg output: {error}"))?;
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }
        let Some(data) = event.get("data") else {
            continue;
        };
        let Some(raw_path) = data
            .get("path")
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let candidate = PathBuf::from(raw_path);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            search_root.join(candidate)
        };
        let canonical = match candidate.canonicalize() {
            Ok(candidate) if candidate.starts_with(&canonical_root) && candidate.is_file() => {
                candidate
            }
            _ => continue,
        };
        let relative = canonical
            .strip_prefix(&canonical_root)
            .map_err(|_| "project rg result escaped workspace".to_string())?;
        let content = match std::fs::read_to_string(&canonical) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let lines = content.lines().collect::<Vec<_>>();
        let line_number = data
            .get("line_number")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(1)
            .clamp(1, lines.len().max(1));
        let start = line_number.saturating_sub(context_lines).max(1);
        let end = line_number.saturating_add(context_lines).min(lines.len());
        let metadata = canonical
            .metadata()
            .map_err(|error| format!("failed to stat project rg match: {error}"))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |value| value.as_secs());
        if hits.push(json!({
            "path": format!("/projects/session/{}", slash_path(relative)),
            "resourceType": "project",
            "resourceId": project_resource_id(relative),
            "version": format!("{}:{modified}", metadata.len()),
            "lineStart": start,
            "lineEnd": end,
            "excerpt": if lines.is_empty() { String::new() } else { lines[start - 1..end].join("\n") }
        })) {
            accepted_matches = accepted_matches.saturating_add(1);
            // `rg --sort path` emits the same ordering used by the workspace
            // cursor (path, then line). Once this source has supplied a full
            // page, later project hits cannot displace any of those hits.
            // Stop reading immediately instead of rereading every high-match
            // file until rg reaches EOF.
            if accepted_matches >= hits.capacity {
                stopped_early = true;
                break;
            }
        }
    }
    if stopped_early {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|error| format!("failed to wait for project rg: {error}"))?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !stopped_early && !status.success() && status.code() != Some(1) {
        return Err(format!("project rg failed: {}", stderr.trim()));
    }
    Ok(())
}

fn requested_project_root(canonical_root: &Path, path: &VirtualPath) -> Result<PathBuf, String> {
    let requested = if path.root() == Some("projects") {
        let mut segments = path.segments().iter();
        if segments
            .next()
            .map(String::as_str)
            .is_some_and(|value| value != "session")
        {
            return Err("project path not found or access denied".to_string());
        }
        segments.collect::<PathBuf>()
    } else {
        PathBuf::new()
    };
    let search_root = canonical_root.join(requested);
    match search_root.canonicalize() {
        Ok(root) if root.starts_with(canonical_root) => Ok(root),
        _ => Err("project path not found or access denied".to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
fn search_project_fallback(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    path: &VirtualPath,
    glob: Option<&Pattern>,
    context_lines: usize,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    let canonical_root = context
        .project_root
        .canonicalize()
        .map_err(|error| format!("project workspace unavailable: {error}"))?;
    let search_root = requested_project_root(&canonical_root, path)?;
    for entry in WalkDir::new(&search_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(project_entry_allowed)
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| format!("failed to stat project file: {error}"))?;
        if metadata.len() > MAX_PROJECT_FILE_BYTES {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&canonical_root)
            .map_err(|_| "project path escaped workspace".to_string())?;
        if let Some(pattern) = glob {
            let relative_text = slash_path(relative);
            let filename = entry.file_name().to_string_lossy();
            if !pattern.matches(&relative_text) && !pattern.matches(&filename) {
                continue;
            }
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let lines = content.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            if !text_matches(line, query, regex) {
                continue;
            }
            let start = index.saturating_sub(context_lines);
            let end = (index + context_lines + 1).min(lines.len());
            hits.push(json!({
                "path": format!("/projects/session/{}", slash_path(relative)),
                "resourceType": "project",
                "resourceId": project_resource_id(relative),
                "version": format!("{}:{}", metadata.len(), metadata.modified().ok().and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |v| v.as_secs())),
                "lineStart": start + 1,
                "lineEnd": end,
                "excerpt": lines[start..end].join("\n")
            }));
        }
    }
    Ok(())
}

async fn search_shared(
    context: &WorkspaceAccessContext,
    query: &str,
    regex: Option<&Regex>,
    hits: &mut BoundedHitWindow,
) -> Result<(), String> {
    let mut after_path = String::new();
    let mut after_resource = String::new();
    loop {
        let rows = sqlx::query(
            "SELECT e.resource_id, e.virtual_path
             FROM agent_workspace_entries e
             WHERE e.tenant_id = ? AND e.visibility = 'tenant_shared'
               AND e.enabled = 1 AND e.is_current = 1 AND e.deleted_at IS NULL
               AND (e.owner_user_id = ? OR EXISTS (
                    SELECT 1 FROM agent_workspace_grants g
                    WHERE g.tenant_id = e.tenant_id AND g.workspace_id = e.workspace_id
                      AND g.entry_id = e.id AND g.grantee_user_id = ?
                      AND g.enabled = 1 AND g.revoked_at IS NULL))
               AND (e.virtual_path > ? OR (e.virtual_path = ? AND e.resource_id > ?))
             ORDER BY e.virtual_path ASC, e.resource_id ASC LIMIT ?",
        )
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.user_id)
        .bind(&after_path)
        .bind(&after_path)
        .bind(&after_resource)
        .bind(i64::try_from(SOURCE_PAGE_SIZE).unwrap_or(500))
        .fetch_all(&context.db)
        .await
        .map_err(|error| format!("failed to search shared workspace: {error}"))?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        if let Some(last) = rows.last() {
            after_path = last.get::<String, _>("virtual_path");
            after_resource = last.get::<String, _>("resource_id");
        }
        for row in rows {
            let virtual_path = normalize_shared_path(&row.get::<String, _>("virtual_path"));
            let Ok(path) = VirtualPath::parse(&virtual_path) else {
                continue;
            };
            let (content, authorized_item) = read_shared(context, &path).await?;
            if text_matches(&content, query, regex) {
                let line = matching_line_number(&content, query, regex);
                hits.push(json!({
                    "path": authorized_item.path,
                    "resourceType": authorized_item.resource_type,
                    "resourceId": authorized_item.resource_id,
                    "version": authorized_item.version,
                    "lineStart": line,
                    "lineEnd": line,
                    "excerpt": excerpt(&content, query, regex, 1200)
                }));
            }
        }
        if row_count < SOURCE_PAGE_SIZE {
            break;
        }
    }
    Ok(())
}

async fn read_upload(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<(String, WorkspaceItem), String> {
    let id = path
        .segments()
        .first()
        .ok_or_else(|| "upload not found or access denied".to_string())?;
    let row = sqlx::query(
        "SELECT filename, media_type, CAST(size_bytes AS INTEGER) AS size_bytes,
                CAST(updated_at AS TEXT) AS updated_at
         FROM chat_file_workspace_files
         WHERE tenant_id = ? AND user_id = ? AND file_id = ?
           AND (session_id = ? OR session_id IS NULL) AND status = 'indexed' LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .bind(id)
    .bind(&context.session_id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read upload metadata: {error}"))?
    .ok_or_else(|| "upload not found or access denied".to_string())?;
    let chunks = sqlx::query(
        "SELECT content FROM chat_file_workspace_chunks
         WHERE tenant_id = ? AND user_id = ? AND file_id = ? ORDER BY chunk_index ASC",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .bind(id)
    .fetch_all(&context.db)
    .await
    .map_err(|error| format!("failed to read upload: {error}"))?;
    let content = join_overlapping_chunks(
        chunks
            .into_iter()
            .map(|chunk| chunk.get::<String, _>("content"))
            .collect::<Vec<_>>(),
    );
    let updated = row.get::<String, _>("updated_at");
    let content_hash = content_digest(&content);
    Ok((
        content,
        WorkspaceItem {
            path: format!(
                "/uploads/{id}/{}",
                safe_virtual_leaf(&row.get::<String, _>("filename"))
            ),
            resource_type: "upload".to_string(),
            resource_id: id.to_string(),
            version: updated.clone(),
            content_hash: Some(content_hash),
            size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
            mime_type: Some(row.get::<String, _>("media_type")),
            updated_at: Some(updated),
            metadata: json!({}),
        },
    ))
}

async fn read_history(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<(String, WorkspaceItem), String> {
    if path.segments().first().map(String::as_str) != Some(context.session_id.as_str()) {
        return Err("history not found or access denied".to_string());
    }
    let id = path
        .segments()
        .get(1)
        .and_then(|value| value.strip_suffix(".md"))
        .ok_or_else(|| "history not found or access denied".to_string())?;
    let row = sqlx::query(
        "SELECT content, content_hash, content_kind, char_count,
                CAST(created_at AS TEXT) AS created_at
         FROM agent_context_archives
         WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND id = ? LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .bind(&context.session_id)
    .bind(id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read history: {error}"))?
    .ok_or_else(|| "history not found or access denied".to_string())?;
    Ok((
        row.get::<String, _>("content"),
        WorkspaceItem {
            path: path.display(),
            resource_type: "history".to_string(),
            resource_id: id.to_string(),
            version: row.get::<String, _>("content_hash"),
            content_hash: Some(row.get::<String, _>("content_hash")),
            size_bytes: row.try_get::<u64, _>("char_count").unwrap_or(0),
            mime_type: Some("text/markdown".to_string()),
            updated_at: Some(row.get::<String, _>("created_at")),
            metadata: json!({"contentKind": row.get::<String, _>("content_kind")}),
        },
    ))
}

async fn read_sql_knowledge(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<(String, WorkspaceItem), String> {
    let pack_id = path
        .segments()
        .first()
        .ok_or_else(|| "SQL knowledge file not found or access denied".to_string())?;
    let file_id = path
        .segments()
        .get(1)
        .ok_or_else(|| "SQL knowledge file not found or access denied".to_string())?;
    let admin = is_admin(context).await?;
    let row = sqlx::query(
        "SELECT f.filename, f.media_type, CAST(f.size_bytes AS INTEGER) AS size_bytes,
                f.content_hash, f.storage_path, CAST(f.updated_at AS TEXT) AS updated_at
         FROM nl2sql_reference_files f
         JOIN nl2sql_reference_packs p ON p.tenant_id = f.tenant_id AND p.id = f.pack_id
         LEFT JOIN data_sources ds ON ds.tenant_id = f.tenant_id AND ds.id = f.datasource_id
         WHERE f.tenant_id = ? AND f.id = ? AND f.pack_id = ?
           AND p.enabled = 1 AND f.status = 'indexed'
           AND (p.user_id = ? OR p.scope = 'tenant' OR ds.user_id = ? OR ?) LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(file_id)
    .bind(pack_id)
    .bind(&context.user_id)
    .bind(&context.user_id)
    .bind(admin)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read SQL knowledge metadata: {error}"))?
    .ok_or_else(|| "SQL knowledge file not found or access denied".to_string())?;
    let storage_path = PathBuf::from(row.get::<String, _>("storage_path"));
    validate_sql_storage_path(context, pack_id, &storage_path)?;
    let bytes = tokio::fs::read(&storage_path)
        .await
        .map_err(|error| format!("failed to read SQL knowledge file: {error}"))?;
    let content = String::from_utf8(bytes)
        .map_err(|_| "SQL knowledge file is not valid UTF-8 text".to_string())?;
    Ok((
        content,
        WorkspaceItem {
            path: format!(
                "/sql-knowledge/{pack_id}/{file_id}/{}",
                safe_virtual_leaf(&row.get::<String, _>("filename"))
            ),
            resource_type: "sql_knowledge".to_string(),
            resource_id: file_id.to_string(),
            version: row.get::<String, _>("content_hash"),
            content_hash: Some(row.get::<String, _>("content_hash")),
            size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
            mime_type: row.get::<Option<String>, _>("media_type"),
            updated_at: Some(row.get::<String, _>("updated_at")),
            metadata: json!({"packId": pack_id}),
        },
    ))
}

async fn read_generated(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<(String, WorkspaceItem), String> {
    let segment = path
        .segments()
        .first()
        .ok_or_else(|| "generated artifact not found or access denied".to_string())?;
    let id_candidate = segment.chars().take(36).collect::<String>();
    let id = uuid::Uuid::parse_str(&id_candidate)
        .map_err(|_| "generated artifact not found or access denied".to_string())?
        .to_string();
    let row = sqlx::query(
        "SELECT artifact_type, CAST(payload_json AS TEXT) AS content,
                CAST(created_at AS TEXT) AS created_at
         FROM chat_turn_artifacts
         WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND id = ? LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .bind(&context.session_id)
    .bind(&id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read generated artifact: {error}"))?
    .ok_or_else(|| "generated artifact not found or access denied".to_string())?;
    let content = row.get::<String, _>("content");
    let created = row.get::<String, _>("created_at");
    if path.segments().get(1).map(String::as_str) == Some("files") {
        let requested = path.segments().iter().skip(2).cloned().collect::<Vec<_>>();
        if requested.is_empty() {
            return Err("generated file not found or access denied".to_string());
        }
        let requested = requested.join("/");
        let file = generated_text_files(&content)
            .into_iter()
            .find(|file| generated_file_relative_path(file).as_deref() == Some(&requested))
            .ok_or_else(|| "generated file not found or access denied".to_string())?;
        let text = generated_file_text(&file).ok_or_else(|| {
            "generated file is binary; open the parent execution artifact".to_string()
        })?;
        let version = file
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or(&created)
            .to_string();
        return Ok((
            text.clone(),
            WorkspaceItem {
                path: path.display(),
                resource_type: "generated_file".to_string(),
                resource_id: id,
                version: version.clone(),
                content_hash: Some(version),
                size_bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
                mime_type: Some("text/plain".to_string()),
                updated_at: Some(created),
                metadata: json!({
                    "artifactType": row.get::<String, _>("artifact_type"),
                    "generatedPath": file.get("path"),
                }),
            },
        ));
    }
    let content_hash = content_digest(&content);
    Ok((
        content.clone(),
        WorkspaceItem {
            path: path.display(),
            resource_type: "generated".to_string(),
            resource_id: id,
            version: created.clone(),
            content_hash: Some(content_hash),
            size_bytes: content.len() as u64,
            mime_type: Some("application/json".to_string()),
            updated_at: Some(created),
            metadata: json!({"artifactType": row.get::<String, _>("artifact_type")}),
        },
    ))
}

fn generated_text_files(content: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|payload| {
            payload
                .get("generatedFiles")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|file| {
            generated_file_text(file).is_some() && generated_file_relative_path(file).is_some()
        })
        .collect()
}

fn generated_file_text(file: &Value) -> Option<String> {
    let encoded = file.get("contentBase64")?.as_str()?;
    let bytes = STANDARD.decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}

fn generated_file_relative_path(file: &Value) -> Option<String> {
    let path = file.get("path")?.as_str()?.trim();
    let relative = path.strip_prefix("/generated/").unwrap_or(path);
    safe_relative_project_path(relative)
        .ok()
        .map(|path| slash_path(&path))
}

fn generated_child_virtual_path(artifact_id: &str, file: &Value) -> String {
    generated_file_relative_path(file).map_or_else(
        || format!("/generated/{artifact_id}/files/unknown"),
        |relative| format!("/generated/{artifact_id}/files/{relative}"),
    )
}

fn read_project(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<(String, WorkspaceItem), String> {
    if path.segments().first().map(String::as_str) != Some("session") {
        return Err("project file not found or access denied".to_string());
    }
    let relative = path.segments().iter().skip(1).collect::<PathBuf>();
    if relative.as_os_str().is_empty() {
        return Err("project file not found or access denied".to_string());
    }
    let root = context
        .project_root
        .canonicalize()
        .map_err(|error| format!("project workspace unavailable: {error}"))?;
    let candidate = root.join(&relative);
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("project file not found or access denied".to_string());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|_| "project file not found or access denied".to_string())?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err("project file not found or access denied".to_string());
    }
    let metadata = canonical
        .metadata()
        .map_err(|error| format!("failed to stat project file: {error}"))?;
    if metadata.len() > MAX_PROJECT_FILE_BYTES {
        return Err("project file exceeds workspace read limit".to_string());
    }
    let content = std::fs::read_to_string(&canonical)
        .map_err(|error| format!("failed to read project file: {error}"))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_secs());
    let content_hash = content_digest(&content);
    Ok((
        content,
        WorkspaceItem {
            path: path.display(),
            resource_type: "project".to_string(),
            resource_id: project_resource_id(&relative),
            version: format!("{modified}:{}", metadata.len()),
            content_hash: Some(content_hash),
            size_bytes: metadata.len(),
            mime_type: Some("text/plain".to_string()),
            updated_at: Some(modified.to_string()),
            metadata: json!({}),
        },
    ))
}

async fn read_shared(
    context: &WorkspaceAccessContext,
    path: &VirtualPath,
) -> Result<(String, WorkspaceItem), String> {
    let display_path = path.display();
    let stored_path = display_path.trim_start_matches('/');
    let row = sqlx::query(
        "SELECT e.owner_user_id, e.resource_id, e.resource_type, e.version, e.content_hash,
                CAST(e.size_bytes AS INTEGER) AS size_bytes, e.mime_type,
                CAST(e.updated_at AS TEXT) AS updated_at,
                COALESCE(e.metadata_json, JSON_OBJECT()) AS metadata_json
         FROM agent_workspace_entries e
         WHERE e.tenant_id = ? AND e.visibility = 'tenant_shared'
           AND e.enabled = 1 AND e.is_current = 1 AND e.deleted_at IS NULL
           AND (e.virtual_path = ? OR e.virtual_path = ?)
           AND (e.owner_user_id = ? OR EXISTS (
                SELECT 1 FROM agent_workspace_grants g
                WHERE g.tenant_id = e.tenant_id AND g.workspace_id = e.workspace_id
                  AND g.entry_id = e.id AND g.grantee_user_id = ?
                  AND g.enabled = 1 AND g.revoked_at IS NULL))
         LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(&display_path)
    .bind(stored_path)
    .bind(&context.user_id)
    .bind(&context.user_id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to authorize shared resource: {error}"))?
    .ok_or_else(|| "shared resource not found or access denied".to_string())?;
    let owner_user_id = row.get::<String, _>("owner_user_id");
    let resource_id = row.get::<String, _>("resource_id");
    let resource_type = row.get::<String, _>("resource_type");
    let metadata = row.get::<Value, _>("metadata_json");
    let content = match resource_type.as_str() {
        "upload" => read_shared_upload(context, &owner_user_id, &resource_id).await?,
        "history" => read_shared_history(context, &owner_user_id, &resource_id).await?,
        "sql_knowledge" => read_shared_sql_knowledge(context, &resource_id).await?,
        "generated" => read_shared_generated(context, &owner_user_id, &resource_id).await?,
        "project" => read_shared_project(context, &owner_user_id, &resource_id, &metadata).await?,
        "shared_text" | "text" => metadata
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "shared resource has no readable text projection".to_string())?
            .to_string(),
        _ => {
            return Err(
                "shared resource type has no server-side readable source adapter".to_string(),
            )
        }
    };
    let item = WorkspaceItem {
        path: display_path,
        resource_type,
        resource_id,
        version: row.get::<String, _>("version"),
        content_hash: row
            .get::<Option<String>, _>("content_hash")
            .or_else(|| Some(content_digest(&content))),
        size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
        mime_type: row.get::<Option<String>, _>("mime_type"),
        updated_at: Some(row.get::<String, _>("updated_at")),
        metadata,
    };
    Ok((content, item))
}

async fn read_shared_upload(
    context: &WorkspaceAccessContext,
    owner_user_id: &str,
    resource_id: &str,
) -> Result<String, String> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM chat_file_workspace_files
         WHERE tenant_id = ? AND user_id = ? AND file_id = ? AND status = 'indexed'",
    )
    .bind(&context.tenant_id)
    .bind(owner_user_id)
    .bind(resource_id)
    .fetch_one(&context.db)
    .await
    .map_err(|error| format!("failed to read shared upload metadata: {error}"))?;
    if exists == 0 {
        return Err("shared resource not found or access denied".to_string());
    }
    let rows = sqlx::query(
        "SELECT content FROM chat_file_workspace_chunks
         WHERE tenant_id = ? AND user_id = ? AND file_id = ? ORDER BY chunk_index ASC",
    )
    .bind(&context.tenant_id)
    .bind(owner_user_id)
    .bind(resource_id)
    .fetch_all(&context.db)
    .await
    .map_err(|error| format!("failed to read shared upload: {error}"))?;
    Ok(join_overlapping_chunks(
        rows.into_iter()
            .map(|row| row.get::<String, _>("content"))
            .collect::<Vec<_>>(),
    ))
}

fn join_overlapping_chunks(chunks: Vec<String>) -> String {
    // chat_intelligence::chunk_text carries exactly the previous 180-character
    // tail into the next chunk. Removing an arbitrary shorter common substring
    // corrupts legitimate repeated SQL/text, so only that full parser overlap
    // is eligible for deduplication.
    const PARSER_OVERLAP_CHARS: usize = crate::UNIFIED_WORKSPACE_UPLOAD_OVERLAP_CHARS;
    let mut chunks = chunks.into_iter();
    let Some(mut combined) = chunks.next() else {
        return String::new();
    };
    for chunk in chunks {
        let tail = combined
            .chars()
            .rev()
            .take(PARSER_OVERLAP_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        let head = chunk.chars().take(tail.len()).collect::<Vec<_>>();
        let overlap = if tail.len() == PARSER_OVERLAP_CHARS && head == tail {
            PARSER_OVERLAP_CHARS
        } else {
            0
        };
        combined.extend(chunk.chars().skip(overlap));
    }
    combined
}

async fn read_shared_history(
    context: &WorkspaceAccessContext,
    owner_user_id: &str,
    resource_id: &str,
) -> Result<String, String> {
    sqlx::query_scalar::<_, String>(
        "SELECT content FROM agent_context_archives
         WHERE tenant_id = ? AND user_id = ? AND id = ? LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(owner_user_id)
    .bind(resource_id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read shared history: {error}"))?
    .ok_or_else(|| "shared resource not found or access denied".to_string())
}

async fn read_shared_sql_knowledge(
    context: &WorkspaceAccessContext,
    resource_id: &str,
) -> Result<String, String> {
    let row = sqlx::query(
        "SELECT f.pack_id, f.storage_path
         FROM nl2sql_reference_files f
         JOIN nl2sql_reference_packs p
           ON p.tenant_id = f.tenant_id AND p.id = f.pack_id
         WHERE f.tenant_id = ? AND f.id = ? AND p.enabled = 1 AND f.status = 'indexed'
         LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(resource_id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read shared SQL knowledge metadata: {error}"))?
    .ok_or_else(|| "shared resource not found or access denied".to_string())?;
    let pack_id = row.get::<String, _>("pack_id");
    let storage_path = PathBuf::from(row.get::<String, _>("storage_path"));
    validate_sql_storage_path(context, &pack_id, &storage_path)?;
    let bytes = tokio::fs::read(storage_path)
        .await
        .map_err(|error| format!("failed to read shared SQL knowledge file: {error}"))?;
    String::from_utf8(bytes)
        .map_err(|_| "shared SQL knowledge file is not valid UTF-8 text".to_string())
}

async fn read_shared_generated(
    context: &WorkspaceAccessContext,
    owner_user_id: &str,
    resource_id: &str,
) -> Result<String, String> {
    sqlx::query_scalar::<_, String>(
        "SELECT CAST(payload_json AS TEXT) FROM chat_turn_artifacts
         WHERE tenant_id = ? AND user_id = ? AND id = ? LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(owner_user_id)
    .bind(resource_id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read shared generated artifact: {error}"))?
    .ok_or_else(|| "shared resource not found or access denied".to_string())
}

async fn read_shared_project(
    context: &WorkspaceAccessContext,
    owner_user_id: &str,
    project_id: &str,
    metadata: &Value,
) -> Result<String, String> {
    let source_path = metadata
        .get("sourcePath")
        .and_then(Value::as_str)
        .ok_or_else(|| "shared project resource has no source path".to_string())?;
    let relative = safe_relative_project_path(source_path)?;
    let row = sqlx::query(
        "SELECT clone_path FROM gitlab_projects
         WHERE tenant_id = ? AND user_id = ? AND id = ?
           AND is_cloned = 1 AND clone_path IS NOT NULL LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(owner_user_id)
    .bind(project_id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to read shared project metadata: {error}"))?
    .ok_or_else(|| "shared resource not found or access denied".to_string())?;
    let data_root = context
        .data_root
        .as_ref()
        .ok_or_else(|| "shared project storage is unavailable".to_string())?;
    let managed_root = data_root
        .join(&context.tenant_id)
        .join(owner_user_id)
        .join("workspace")
        .canonicalize()
        .map_err(|_| "shared resource not found or access denied".to_string())?;
    let clone_root = PathBuf::from(row.get::<String, _>("clone_path"))
        .canonicalize()
        .map_err(|_| "shared resource not found or access denied".to_string())?;
    if !clone_root.starts_with(&managed_root) {
        return Err("shared resource not found or access denied".to_string());
    }
    let file = clone_root
        .join(relative)
        .canonicalize()
        .map_err(|_| "shared resource not found or access denied".to_string())?;
    if !file.starts_with(&clone_root) || !file.is_file() {
        return Err("shared resource not found or access denied".to_string());
    }
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to stat shared project file: {error}"))?;
    if metadata.len() > MAX_PROJECT_FILE_BYTES {
        return Err("shared project file exceeds workspace read limit".to_string());
    }
    std::fs::read_to_string(file)
        .map_err(|_| "shared project file is not valid UTF-8 text".to_string())
}

fn safe_relative_project_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.chars().any(char::is_control)
        || raw.contains('\\')
        || raw.starts_with('/')
        || raw.to_ascii_lowercase().contains("%2e")
    {
        return Err("shared project source path is invalid".to_string());
    }
    let path = PathBuf::from(raw);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("shared project source path is invalid".to_string());
    }
    Ok(path)
}

async fn project_items(
    context: &WorkspaceAccessContext,
    workspace_id: &str,
    items: &[WorkspaceItem],
) -> Result<(), String> {
    let mut transaction = context
        .db
        .begin()
        .await
        .map_err(|error| format!("failed to begin workspace projection: {error}"))?;
    for item in items {
        // Shared entries are their own source of truth. Re-projecting them into a
        // personal workspace would create recursive tenant-shared copies.
        if item.path.starts_with("/shared/") {
            continue;
        }
        sqlx::query(
            "UPDATE agent_workspace_entries
             SET is_current = 0
             WHERE tenant_id = ? AND workspace_id = ? AND virtual_path = ?
               AND version <> ? AND is_current = 1",
        )
        .bind(&context.tenant_id)
        .bind(workspace_id)
        .bind(&item.path)
        .bind(&item.version)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("failed to retire workspace projection version: {error}"))?;
        let digest = Sha256::digest(
            format!(
                "{workspace_id}:{}:{}:{}",
                item.resource_type, item.path, item.version
            )
            .as_bytes(),
        );
        let id = format!("entry-{}", hex_prefix(&digest, 20));
        sqlx::query(
            "INSERT INTO agent_workspace_entries
                (id, tenant_id, owner_user_id, workspace_id, visibility, resource_type,
                 resource_id, virtual_path, version, content_hash, size_bytes, mime_type,
                 metadata_json, source_updated_at, enabled, is_current)
             VALUES (?, ?, ?, ?, 'private', ?, ?, ?, ?, ?, ?, ?, ?, NULL, 1, 1)
             ON CONFLICT DO UPDATE SET
                resource_id = excluded.resource_id, version = excluded.version,
                content_hash = COALESCE(excluded.content_hash, content_hash),
                size_bytes = excluded.size_bytes, mime_type = excluded.mime_type,
                metadata_json = excluded.metadata_json, enabled = 1, is_current = 1,
                deleted_at = NULL",
        )
        .bind(id)
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(workspace_id)
        .bind(&item.resource_type)
        .bind(&item.resource_id)
        .bind(&item.path)
        .bind(&item.version)
        .bind(&item.content_hash)
        .bind(i64::try_from(item.size_bytes).unwrap_or(i64::MAX))
        .bind(&item.mime_type)
        .bind(sqlx::types::Json(&item.metadata))
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("failed to persist workspace projection: {error}"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("failed to commit workspace projection: {error}"))?;
    Ok(())
}

async fn audit_usage(
    context: &WorkspaceAccessContext,
    workspace_id: &str,
    operation: &str,
    path: &str,
    outcome: &str,
    denial_code: Option<&str>,
    duration_ms: u128,
) {
    let parent_turn_id = sqlx::query_scalar::<_, String>(
        "SELECT turn_id FROM super_assistant_turns
         WHERE tenant_id = ? AND user_id = ? AND session_id = ?
           AND runtime_turn_id IS NOT NULL
           AND status IN ('running','running_model','waiting_subagent',
                          'resuming_model','verifying')
         ORDER BY last_heartbeat_at DESC, updated_at DESC LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .bind(&context.session_id)
    .fetch_optional(&context.db)
    .await
    .ok()
    .flatten();
    let _ = sqlx::query(
        "INSERT INTO agent_workspace_usage
            (tenant_id, user_id, workspace_id, turn_id, operation, virtual_path,
             outcome, duration_ms, denial_code, metadata_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, JSON_OBJECT('sessionId', ?))",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .bind(workspace_id)
    .bind(parent_turn_id)
    .bind(operation)
    .bind(path)
    .bind(outcome)
    .bind(i64::try_from(duration_ms).unwrap_or(i64::MAX))
    .bind(denial_code)
    .bind(&context.session_id)
    .execute(&context.db)
    .await;
}

async fn is_admin(context: &WorkspaceAccessContext) -> Result<bool, String> {
    let role = sqlx::query_scalar::<_, String>(
        "SELECT role FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1 LIMIT 1",
    )
    .bind(&context.tenant_id)
    .bind(&context.user_id)
    .fetch_optional(&context.db)
    .await
    .map_err(|error| format!("failed to resolve workspace actor: {error}"))?
    .ok_or_else(|| "workspace actor not found".to_string())?;
    Ok(matches!(role.as_str(), "admin" | "superadmin"))
}

fn validate_sql_storage_path(
    context: &WorkspaceAccessContext,
    pack_id: &str,
    path: &Path,
) -> Result<(), String> {
    let data_root = context
        .data_root
        .as_ref()
        .ok_or_else(|| "SQL knowledge storage is unavailable in this workspace".to_string())?;
    let expected = data_root
        .join(".aos")
        .join("nl2sql-reference")
        .join(safe_path_segment(&context.tenant_id))
        .join(safe_path_segment(pack_id));
    let expected = expected
        .canonicalize()
        .map_err(|_| "SQL knowledge storage root is unavailable".to_string())?;
    let canonical = path
        .canonicalize()
        .map_err(|_| "SQL knowledge file not found or access denied".to_string())?;
    if !canonical.starts_with(expected) || !canonical.is_file() {
        return Err("SQL knowledge file not found or access denied".to_string());
    }
    Ok(())
}

fn selected_roots(input: &Value, path_root: Option<&str>) -> Vec<String> {
    if let Some(root) = path_root {
        return vec![root.to_string()];
    }
    let requested = input
        .get("roots")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter_map(normalize_root_alias)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if requested.is_empty() {
        ROOTS.iter().map(|root| (*root).to_string()).collect()
    } else {
        requested.into_iter().collect()
    }
}

fn normalize_root_alias(value: &str) -> Option<String> {
    match value
        .trim()
        .trim_start_matches('/')
        .to_ascii_lowercase()
        .as_str()
    {
        "memory" | "history" => Some("history".to_string()),
        "files" | "uploads" => Some("uploads".to_string()),
        "project" | "projects" => Some("projects".to_string()),
        "sql" | "sql-knowledge" => Some("sql-knowledge".to_string()),
        "generated" => Some("generated".to_string()),
        "shared" => Some("shared".to_string()),
        _ => None,
    }
}

fn path_matches_prefix(candidate: &str, path: &VirtualPath) -> bool {
    path.root().is_none()
        || candidate == path.display()
        || candidate.starts_with(&format!("{}/", path.display().trim_end_matches('/')))
}

fn bounded_limit(input: &Value, default: usize, maximum: usize) -> usize {
    input
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
        .clamp(1, maximum)
}

fn cursor_scope(
    context: &WorkspaceAccessContext,
    workspace: &WorkspaceHandle,
    operation: &str,
    input: &Value,
) -> String {
    let mut request = input.as_object().cloned().unwrap_or_default();
    request.remove("cursor");
    request.remove("limit");
    let digest = Sha256::digest(
        serde_json::to_vec(&json!({
            "tenantId": context.tenant_id,
            "userId": context.user_id,
            "workspaceId": workspace.id,
            "aclVersion": workspace.acl_version,
            "operation": operation,
            "request": request,
        }))
        .unwrap_or_default(),
    );
    hex_prefix(&digest, 32)
}

fn encode_cursor(scope: &str, key: &str) -> Result<String, String> {
    let value = serde_json::to_vec(&json!({"v": 1, "scope": scope, "key": key}))
        .map_err(|error| format!("failed to encode workspace cursor: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

fn decode_cursor(input: &Value, expected_scope: &str) -> Result<Option<String>, String> {
    let Some(raw) = input
        .get("cursor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| "invalid or expired workspace cursor".to_string())?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid or expired workspace cursor".to_string())?;
    let valid = value.get("v").and_then(Value::as_u64) == Some(1)
        && value.get("scope").and_then(Value::as_str) == Some(expected_scope);
    if !valid {
        return Err(
            "workspace cursor no longer matches the current authorization or query".to_string(),
        );
    }
    value
        .get("key")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .map(Some)
        .ok_or_else(|| "invalid or expired workspace cursor".to_string())
}

fn item_sort_key(item: &WorkspaceItem) -> String {
    format!("{}\0{}\0{}", item.path, item.version, item.resource_id)
}

fn tree_entries(items: &[WorkspaceItem], base: &VirtualPath, depth: usize) -> Vec<Value> {
    let base_segments = base.segments().len();
    let mut entries = BTreeMap::<String, Value>::new();
    for item in items {
        let Ok(candidate) = VirtualPath::parse(&item.path) else {
            continue;
        };
        if candidate.root() != base.root()
            || candidate.segments().get(..base_segments) != Some(base.segments())
        {
            continue;
        }
        let remaining = &candidate.segments()[base_segments..];
        if remaining.is_empty() {
            entries.insert(item.path.clone(), item_json(item));
            continue;
        }
        if remaining.len() <= depth {
            entries.insert(item.path.clone(), item_json(item));
            continue;
        }
        let visible_segments = candidate
            .segments()
            .iter()
            .take(base_segments.saturating_add(depth))
            .cloned()
            .collect::<Vec<_>>();
        let directory_path = format!(
            "/{}/{}",
            candidate.root().unwrap_or_default(),
            visible_segments.join("/")
        );
        entries.entry(directory_path.clone()).or_insert_with(|| {
            json!({
                "path": directory_path,
                "name": visible_segments.last(),
                "kind": "directory"
            })
        });
    }
    entries.into_values().collect()
}

fn hit_sort_key(hit: &Value) -> String {
    let path = hit.get("path").and_then(Value::as_str).unwrap_or_default();
    let line_start = hit
        .get("lineStart")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let line_end = hit
        .get("lineEnd")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let resource_id = hit
        .get("resourceId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let excerpt_hash = Sha256::digest(
        hit.get("excerpt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .as_bytes(),
    );
    format!(
        "{path}\0{line_start:020}\0{line_end:020}\0{resource_id}\0{}",
        hex_prefix(&excerpt_hash, 16)
    )
}

fn paginate_items(
    mut items: Vec<WorkspaceItem>,
    input: &Value,
    limit: usize,
    scope: &str,
) -> Result<(Vec<WorkspaceItem>, Option<String>), String> {
    items.sort_by_key(item_sort_key);
    if let Some(after) = decode_cursor(input, scope)? {
        items.retain(|item| item_sort_key(item) > after);
    }
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items
            .last()
            .map(item_sort_key)
            .map(|key| encode_cursor(scope, &key))
            .transpose()?
    } else {
        None
    };
    Ok((items, next_cursor))
}

fn paginate_hits(
    mut hits: Vec<Value>,
    input: &Value,
    limit: usize,
    scope: &str,
) -> Result<(Vec<Value>, Option<String>), String> {
    hits.sort_by_key(hit_sort_key);
    if let Some(after) = decode_cursor(input, scope)? {
        hits.retain(|hit| hit_sort_key(hit) > after);
    }
    let has_more = hits.len() > limit;
    hits.truncate(limit);
    let next_cursor = if has_more {
        hits.last()
            .map(hit_sort_key)
            .map(|key| encode_cursor(scope, &key))
            .transpose()?
    } else {
        None
    };
    Ok((hits, next_cursor))
}

fn item_json(item: &WorkspaceItem) -> Value {
    json!({
        "path": item.path,
        "kind": "file",
        "resourceType": item.resource_type,
        "resourceId": item.resource_id,
        "version": item.version,
        "contentHash": item.content_hash,
        "sizeBytes": item.size_bytes,
        "mimeType": item.mime_type,
        "updatedAt": item.updated_at,
        "metadata": public_metadata(&item.metadata)
    })
}

fn public_metadata(metadata: &Value) -> Value {
    let mut value = metadata.clone();
    if let Some(object) = value.as_object_mut() {
        for key in [
            "content",
            "storagePath",
            "storage_path",
            "physicalPath",
            "physical_path",
            "ownerUserId",
            "owner_user_id",
        ] {
            object.remove(key);
        }
    }
    value
}

fn hit_to_item(hit: &Value) -> Option<WorkspaceItem> {
    Some(WorkspaceItem {
        path: hit.get("path")?.as_str()?.to_string(),
        resource_type: hit.get("resourceType")?.as_str()?.to_string(),
        resource_id: hit.get("resourceId")?.as_str()?.to_string(),
        version: hit
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        content_hash: hit
            .get("contentHash")
            .and_then(Value::as_str)
            .map(str::to_string),
        size_bytes: 0,
        mime_type: None,
        updated_at: None,
        metadata: json!({}),
    })
}

fn text_matches(text: &str, query: &str, regex: Option<&Regex>) -> bool {
    regex.map_or_else(
        || text.to_lowercase().contains(&query.to_lowercase()),
        |regex| regex.is_match(text),
    )
}

fn search_prefilter(query: &str, regex: Option<&Regex>) -> String {
    // An arbitrary regex has no generally safe mandatory literal. Choosing a
    // token from alternatives such as `foo|bar` would silently discard valid
    // rows before the regex engine sees them. The SQL queries are already
    // tenant/user scoped and keyset-paged, so regex mode deliberately uses an
    // empty LIKE prefilter and applies the real expression to every authorized
    // candidate.
    regex.map_or_else(|| query.to_string(), |_| String::new())
}

fn match_byte_position(text: &str, query: &str, regex: Option<&Regex>) -> Option<usize> {
    if let Some(regex) = regex {
        return regex.find(text).map(|found| found.start());
    }
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .ok()
        .and_then(|pattern| pattern.find(text).map(|found| found.start()))
}

fn matching_line_number(text: &str, query: &str, regex: Option<&Regex>) -> usize {
    match_byte_position(text, query, regex)
        .map(|position| {
            text[..position]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1
        })
        .unwrap_or(1)
}

fn excerpt(text: &str, query: &str, regex: Option<&Regex>, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let position = match_byte_position(text, query, regex)
        .map(|byte| text[..byte].chars().count())
        .unwrap_or(0);
    text.chars()
        .skip(position.saturating_sub(max_chars / 3))
        .take(max_chars)
        .collect()
}

fn slice_lines(
    content: &str,
    requested_start: Option<usize>,
    requested_end: Option<usize>,
    max_chars: usize,
) -> (usize, usize, String, bool) {
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return (1, 1, String::new(), false);
    }
    let start = requested_start.unwrap_or(1).clamp(1, lines.len());
    let end = requested_end
        .unwrap_or(lines.len())
        .clamp(start, lines.len());
    let mut selected = String::new();
    let mut actual_end = start;
    let mut truncated = false;
    for (offset, line) in lines[start - 1..end].iter().enumerate() {
        let separator_chars = usize::from(offset > 0);
        let available = max_chars.saturating_sub(selected.chars().count());
        if available <= separator_chars {
            truncated = true;
            break;
        }
        if offset > 0 {
            selected.push('\n');
        }
        let available = max_chars.saturating_sub(selected.chars().count());
        let line_chars = line.chars().count();
        selected.extend(line.chars().take(available));
        actual_end = start + offset;
        if line_chars > available {
            truncated = true;
            break;
        }
    }
    if actual_end < end {
        truncated = true;
    }
    (start, actual_end, selected, truncated)
}

fn project_entry_allowed(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    !matches!(name.as_ref(), ".sandbox-home" | ".sandbox-tmp")
        && name != ".aos"
        && !name.starts_with(".aos-")
}

fn project_resource_id(relative: &Path) -> String {
    let digest = Sha256::digest(slash_path(relative).as_bytes());
    format!("project-{}", hex_prefix(&digest, 32))
}

fn safe_virtual_leaf(value: &str) -> String {
    let leaf = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, ':' | '%') {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    if leaf.is_empty() || matches!(leaf.as_str(), "." | "..") {
        "file".to_string()
    } else {
        leaf
    }
}

fn safe_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn normalize_shared_path(value: &str) -> String {
    let value = value.trim().trim_start_matches('/');
    if value.starts_with("shared/") {
        format!("/{value}")
    } else {
        format!("/shared/{value}")
    }
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(ToOwned::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn hex_prefix(bytes: &[u8], length: usize) -> String {
    bytes
        .iter()
        .flat_map(|byte| format!("{byte:02x}").chars().collect::<Vec<_>>())
        .take(length)
        .collect()
}

fn content_digest(content: &str) -> String {
    hex_prefix(&Sha256::digest(content.as_bytes()), 64)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_cursor, encode_cursor, excerpt, execute_workspace_operation,
        generated_child_virtual_path, generated_file_text, generated_text_files,
        join_overlapping_chunks, matching_line_number, paginate_items, safe_relative_project_path,
        safe_virtual_leaf, search_prefilter, search_project, slice_lines, tree_entries,
        BoundedHitWindow, VirtualPath, WorkspaceAccessContext, WorkspaceItem,
    };
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use proptest::prelude::*;
    use serde_json::{json, Value};

    #[test]
    fn canonical_roots_are_accepted() {
        assert_eq!(
            VirtualPath::parse("/sql-knowledge/p/f/a.sql")
                .expect("valid path")
                .display(),
            "/sql-knowledge/p/f/a.sql"
        );
    }

    #[test]
    fn path_attacks_are_rejected() {
        for path in [
            "../secret",
            "/uploads/../secret",
            "/uploads/%2e%2e/secret",
            "/uploads/a\\b",
            "/etc/passwd",
            "C:/secret",
            "/uploads/a\0b",
        ] {
            assert!(VirtualPath::parse(path).is_err(), "{path}");
        }
    }

    #[test]
    fn cursor_is_bound_to_authorization_and_query_scope() {
        let cursor =
            encode_cursor("tenant-a:user-a:acl-1", "/uploads/a.sql").expect("cursor should encode");
        assert_eq!(
            decode_cursor(&json!({"cursor": &cursor}), "tenant-a:user-a:acl-1")
                .expect("matching cursor should decode")
                .as_deref(),
            Some("/uploads/a.sql")
        );
        assert!(
            decode_cursor(&json!({"cursor": &cursor}), "tenant-a:user-b:acl-1").is_err(),
            "a cursor must not survive a user or ACL scope change"
        );
    }

    #[test]
    fn virtual_leaf_never_creates_a_path_segment_escape() {
        for raw in ["..", ".", "a/b", "a\\b", "C:secret", "%2e%2e"] {
            let leaf = safe_virtual_leaf(raw);
            assert!(VirtualPath::parse(&format!("/uploads/id/{leaf}")).is_ok());
        }
    }

    #[test]
    fn shared_project_source_rejects_every_escape_form() {
        assert_eq!(
            safe_relative_project_path("src/report.sql").expect("valid relative path"),
            std::path::PathBuf::from("src/report.sql")
        );
        for path in ["../x", "/etc/passwd", "a/../../b", "a\\b", "%2e%2e/x"] {
            assert!(safe_relative_project_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn tree_depth_returns_directories_until_the_file_is_visible() {
        let item = WorkspaceItem {
            path: "/uploads/file-1/report.sql".to_string(),
            resource_type: "upload".to_string(),
            resource_id: "file-1".to_string(),
            version: "v1".to_string(),
            content_hash: Some("hash-1".to_string()),
            size_bytes: 10,
            mime_type: Some("text/plain".to_string()),
            updated_at: None,
            metadata: Value::Null,
        };
        let root = VirtualPath::parse("/uploads").expect("valid root");

        assert_eq!(
            tree_entries(std::slice::from_ref(&item), &root, 1)[0]["path"],
            "/uploads/file-1"
        );
        assert_eq!(
            tree_entries(&[item], &root, 2)[0]["path"],
            "/uploads/file-1/report.sql"
        );
    }

    #[test]
    fn unicode_excerpt_and_truncated_line_range_are_exact() {
        let text = "第一行\n第二行包含关键字\n第三行";
        assert_eq!(matching_line_number(text, "关键字", None), 2);
        assert!(excerpt(text, "关键字", None, 8).contains("关键字"));

        let (start, end, selected, truncated) = slice_lines(text, Some(1), Some(3), 6);
        assert_eq!((start, end), (1, 2));
        assert!(selected.starts_with("第一行\n第二"));
        assert!(truncated);
    }

    #[test]
    fn generated_text_files_have_stable_readable_virtual_paths() {
        let file = json!({
            "path": "/generated/reports/roi.sql",
            "sizeBytes": 9,
            "sha256": "abc123",
            "contentBase64": STANDARD.encode("SELECT 1;")
        });
        let payload = json!({"generatedFiles": [file.clone()]}).to_string();

        assert_eq!(generated_text_files(&payload), vec![file.clone()]);
        assert_eq!(generated_file_text(&file).as_deref(), Some("SELECT 1;"));
        assert_eq!(
            generated_child_virtual_path("artifact-1", &file),
            "/generated/artifact-1/files/reports/roi.sql"
        );
    }

    #[test]
    fn generated_binary_and_traversal_entries_are_not_projected_as_text_files() {
        let binary = json!({
            "path": "/generated/image.bin",
            "contentBase64": STANDARD.encode([0xff, 0xfe, 0xfd])
        });
        let traversal = json!({
            "path": "/generated/../secret.txt",
            "contentBase64": STANDARD.encode("secret")
        });
        let payload = json!({"generatedFiles": [binary, traversal]}).to_string();

        let files = generated_text_files(&payload);
        assert!(files.is_empty());
    }

    #[test]
    fn chunk_reconstruction_removes_only_the_parser_overlap() {
        let prefix = "A".repeat(80);
        let parser_overlap = "B".repeat(180);
        assert_eq!(
            join_overlapping_chunks(vec![
                format!("{prefix}{parser_overlap}"),
                format!("{parser_overlap}tail"),
            ]),
            format!("{prefix}{parser_overlap}tail")
        );
        assert_eq!(
            join_overlapping_chunks(vec!["abc".to_string(), "xyz".to_string()]),
            "abcxyz"
        );
        assert_eq!(
            join_overlapping_chunks(vec!["first SAME".to_string(), "SAME second".to_string()]),
            "first SAMESAME second"
        );
    }

    #[test]
    fn regex_search_never_uses_a_lossy_literal_prefilter() {
        let regex = regex::Regex::new("foo|bar").expect("valid regex");
        assert_eq!(search_prefilter("foo|bar", Some(&regex)), "");
        assert_eq!(search_prefilter("exact phrase", None), "exact phrase");
    }

    #[test]
    fn bounded_hit_window_keeps_only_the_next_sorted_page() {
        let after = super::hit_sort_key(&json!({
            "path": "/uploads/004/file.sql",
            "lineStart": 1,
            "lineEnd": 1,
            "resourceId": "004",
            "excerpt": "match"
        }));
        let mut window = BoundedHitWindow::new(Some(after), 4);
        for index in (0..20).rev() {
            window.push(json!({
                "path": format!("/uploads/{index:03}/file.sql"),
                "lineStart": 1,
                "lineEnd": 1,
                "resourceId": format!("{index:03}"),
                "excerpt": "match"
            }));
        }
        let paths = window
            .into_hits()
            .into_iter()
            .filter_map(|hit| hit.get("path").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "/uploads/005/file.sql",
                "/uploads/006/file.sql",
                "/uploads/007/file.sql",
                "/uploads/008/file.sql"
            ]
        );
    }

    #[test]
    fn bounded_hit_window_filters_before_applying_capacity() {
        let mut window = BoundedHitWindow::with_filter(
            None,
            1,
            VirtualPath::parse("/uploads/999").expect("valid filter"),
            Some(glob::Pattern::new("*.sql").expect("valid glob")),
        );
        for index in 0..1_000 {
            window.push(json!({
                "path": format!("/uploads/{index:03}/file.sql"),
                "resourceId": format!("{index:03}"),
                "lineStart": 1,
                "lineEnd": 1,
                "excerpt": "match"
            }));
        }
        let hits = window.into_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["path"], "/uploads/999/file.sql");
    }

    #[tokio::test]
    async fn project_search_uses_rg_compatible_exact_lines() {
        let root =
            std::env::temp_dir().join(format!("aos-workspace-rg-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).expect("create fixture root");
        std::fs::write(
            root.join("src/query.sql"),
            "SELECT 1;\n-- 中文关键指标\nSELECT 2;\n",
        )
        .expect("write fixture");
        std::fs::create_dir_all(root.join(".aos/sessions")).expect("create internal session root");
        std::fs::write(
            root.join(".aos/sessions/internal.jsonl"),
            "中文关键指标 must stay private",
        )
        .expect("write internal fixture");
        std::fs::create_dir_all(root.join(".aos-rd-candidates/internal"))
            .expect("create internal candidate root");
        std::fs::write(
            root.join(".aos-rd-candidates/internal/candidate.txt"),
            "中文关键指标 must stay private",
        )
        .expect("write internal candidate fixture");
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .expect("lazy SQLite pool");
        let context = WorkspaceAccessContext {
            db,
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
            project_root: root.clone(),
            data_root: None,
        };
        let mut hits = BoundedHitWindow::new(None, 10);
        search_project(
            &context,
            "关键指标",
            None,
            &VirtualPath::parse("/projects/session").expect("valid project path"),
            None,
            1,
            &mut hits,
        )
        .expect("project search");
        let _ = std::fs::remove_dir_all(root);
        let hits = hits.into_hits();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["path"], "/projects/session/src/query.sql");
        assert!(hits[0]["resourceId"]
            .as_str()
            .is_some_and(|value| value.starts_with("project-") && value.len() == 40));
        assert_eq!(hits[0]["lineStart"], 1);
        assert_eq!(hits[0]["lineEnd"], 3);
        assert!(hits[0]["excerpt"]
            .as_str()
            .is_some_and(|value| value.contains("中文关键指标")));
    }

    #[tokio::test]
    async fn project_search_stops_after_filling_the_bounded_window() {
        let root = std::env::temp_dir().join(format!(
            "aos-workspace-rg-bounded-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("create fixture root");
        std::fs::write(
            root.join("many-matches.txt"),
            (0..20_000)
                .map(|index| format!("ROI match {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .expect("write high-match fixture");
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .expect("lazy SQLite pool");
        let context = WorkspaceAccessContext {
            db,
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
            project_root: root.clone(),
            data_root: None,
        };
        let mut hits = BoundedHitWindow::new(None, 4);
        search_project(
            &context,
            "ROI",
            None,
            &VirtualPath::parse("/projects/session").expect("valid project path"),
            None,
            0,
            &mut hits,
        )
        .expect("bounded project search");
        let _ = std::fs::remove_dir_all(root);

        let hits = hits.into_hits();
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0]["lineStart"], 1);
        assert_eq!(hits[3]["lineStart"], 4);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn arbitrary_parent_segments_never_parse(prefix in "[a-z]{0,12}", suffix in "[a-z]{0,12}") {
            let path = format!("/uploads/{prefix}/../{suffix}");
            prop_assert!(VirtualPath::parse(&path).is_err());
        }

        // Feature: unified-agent-workspace, Property: tenant/user/ACL scope changes invalidate every existing cursor.
        #[test]
        fn authorization_scope_changes_always_invalidate_cursor(
            original in "[a-z0-9:-]{1,48}",
            changed in "[a-z0-9:-]{1,48}",
        ) {
            prop_assume!(original != changed);
            let cursor = encode_cursor(&original, "/uploads/file.sql").expect("encode cursor");
            let decoded = decode_cursor(&json!({"cursor": cursor}), &changed);
            prop_assert!(decoded.is_err());
        }

        // Feature: unified-agent-workspace, Property: stable cursor pagination is complete and duplicate-free.
        #[test]
        fn cursor_pagination_returns_each_authorized_item_once(
            names in prop::collection::btree_set("[a-z]{1,12}", 1..80)
        ) {
            let items = names
                .iter()
                .map(|name| WorkspaceItem {
                    path: format!("/uploads/{name}/file.sql"),
                    resource_type: "upload".to_string(),
                    resource_id: name.clone(),
                    version: "v1".to_string(),
                    content_hash: None,
                    size_bytes: 1,
                    mime_type: Some("text/plain".to_string()),
                    updated_at: None,
                    metadata: Value::Null,
                })
                .collect::<Vec<_>>();
            let mut cursor = None;
            let mut actual = Vec::new();
            loop {
                let input = cursor.as_ref().map_or_else(
                    || json!({}),
                    |cursor| json!({"cursor": cursor}),
                );
                let (page, next) = paginate_items(items.clone(), &input, 7, "fixed-scope")
                    .expect("generated cursor page should be valid");
                actual.extend(page.into_iter().map(|item| item.path));
                cursor = next;
                if cursor.is_none() {
                    break;
                }
            }
            let mut expected = items.into_iter().map(|item| item.path).collect::<Vec<_>>();
            expected.sort();
            prop_assert_eq!(actual, expected);
        }
    }

    #[tokio::test]
    async fn sqlite_workspace_authorization_is_zero_leakage() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open SQLite workspace test database");
        sqlx::migrate!("../web-server/sqlite-migrations")
            .run(&db)
            .await
            .expect("apply SQLite workspace test migrations");
        let tenant = uuid::Uuid::new_v4().to_string();
        let user_a = uuid::Uuid::new_v4().to_string();
        let user_b = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let file_a = uuid::Uuid::new_v4().to_string();
        let file_b = uuid::Uuid::new_v4().to_string();
        let unique_b = format!("private-b-{}", uuid::Uuid::new_v4());
        let project_root = std::env::temp_dir().join(format!("aos-isolation-{tenant}"));
        std::fs::create_dir_all(&project_root).expect("create project fixture");

        for (user, file, filename, content) in [
            (
                &user_a,
                &file_a,
                "a.sql",
                "SELECT 'visible-to-a';".to_string(),
            ),
            (&user_b, &file_b, "b-secret.sql", unique_b.clone()),
        ] {
            sqlx::query(
                "INSERT INTO chat_file_workspace_files
                    (id, tenant_id, user_id, session_id, file_id, filename, media_type,
                     size_bytes, url, status, chunk_count)
                 VALUES (?, ?, ?, ?, ?, ?, 'text/plain', ?, 'test://fixture', 'indexed', 1)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&tenant)
            .bind(user)
            .bind(&session)
            .bind(file)
            .bind(filename)
            .bind(i64::try_from(content.len()).unwrap_or(i64::MAX))
            .execute(&db)
            .await
            .expect("insert upload fixture");
            sqlx::query(
                "INSERT INTO chat_file_workspace_chunks
                    (id, tenant_id, user_id, file_id, chunk_index, line_start, line_end, content)
                 VALUES (?, ?, ?, ?, 0, 1, 1, ?)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&tenant)
            .bind(user)
            .bind(file)
            .bind(content)
            .execute(&db)
            .await
            .expect("insert upload chunk fixture");
        }

        let context_a = WorkspaceAccessContext {
            db: db.clone(),
            tenant_id: tenant.clone(),
            user_id: user_a.clone(),
            session_id: session.clone(),
            project_root: project_root.clone(),
            data_root: None,
        };
        let hidden_search = execute_workspace_operation(
            &context_a,
            "workspace_rg",
            &json!({"path": "/uploads", "query": unique_b, "limit": 20}),
        )
        .await
        .expect("authorized search should run");
        let hidden_search: Value = serde_json::from_str(&hidden_search).expect("search JSON");
        assert_eq!(hidden_search["count"], 0);
        let guessed_path = format!("/uploads/{file_b}/b-secret.sql");
        assert!(
            execute_workspace_operation(
                &context_a,
                "workspace_read",
                &json!({"path": guessed_path}),
            )
            .await
            .is_err(),
            "a guessed B resource id must remain indistinguishable from not found"
        );

        let shared_workspace = format!("ws-shared-{}", uuid::Uuid::new_v4());
        let shared_entry = format!("entry-{}", uuid::Uuid::new_v4());
        let shared_resource = uuid::Uuid::new_v4().to_string();
        let shared_path = format!("shared/tests/{shared_resource}.txt");
        sqlx::query(
            "INSERT INTO agent_workspaces
                (id, tenant_id, owner_user_id, workspace_type, visibility, enabled, acl_version)
             VALUES (?, ?, ?, 'tenant_shared', 'tenant_shared', 1, 1)",
        )
        .bind(&shared_workspace)
        .bind(&tenant)
        .bind(&user_b)
        .execute(&db)
        .await
        .expect("insert shared workspace");
        sqlx::query(
            "INSERT INTO agent_workspace_entries
                (id, tenant_id, owner_user_id, workspace_id, visibility, resource_type,
                 resource_id, virtual_path, version, content_hash, size_bytes, mime_type,
                 enabled, is_current, metadata_json)
             VALUES (?, ?, ?, ?, 'tenant_shared', 'shared_text', ?, ?, 'v1', 'hash', 6,
                     'text/plain', 1, 1, JSON_OBJECT('content', 'shared'))",
        )
        .bind(&shared_entry)
        .bind(&tenant)
        .bind(&user_b)
        .bind(&shared_workspace)
        .bind(&shared_resource)
        .bind(&shared_path)
        .execute(&db)
        .await
        .expect("insert shared entry");
        sqlx::query(
            "INSERT INTO agent_workspace_grants
                (id, tenant_id, workspace_id, entry_id, resource_id, grantee_user_id,
                 permission, enabled)
             VALUES (?, ?, ?, ?, ?, ?, 'read', 1)",
        )
        .bind(format!("grant-{}", uuid::Uuid::new_v4()))
        .bind(&tenant)
        .bind(&shared_workspace)
        .bind(&shared_entry)
        .bind(&shared_resource)
        .bind(&user_a)
        .execute(&db)
        .await
        .expect("insert shared grant");
        let virtual_shared_path = format!("/{shared_path}");
        assert!(execute_workspace_operation(
            &context_a,
            "workspace_read",
            &json!({"path": &virtual_shared_path}),
        )
        .await
        .expect("granted shared text should be readable")
        .contains("shared"));
        sqlx::query(
            "UPDATE agent_workspace_grants
             SET enabled = 0, revoked_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND entry_id = ? AND grantee_user_id = ?",
        )
        .bind(&tenant)
        .bind(&shared_entry)
        .bind(&user_a)
        .execute(&db)
        .await
        .expect("revoke shared grant");
        assert!(
            execute_workspace_operation(
                &context_a,
                "workspace_read",
                &json!({"path": &virtual_shared_path}),
            )
            .await
            .is_err(),
            "revocation must be effective on the next read"
        );

        for statement in [
            "DELETE FROM agent_workspace_usage WHERE tenant_id = ?",
            "DELETE FROM agent_workspace_grants WHERE tenant_id = ?",
            "DELETE FROM agent_workspace_entries WHERE tenant_id = ?",
            "DELETE FROM agent_workspace_mounts WHERE tenant_id = ?",
            "DELETE FROM agent_workspaces WHERE tenant_id = ?",
            "DELETE FROM chat_file_workspace_chunks WHERE tenant_id = ?",
            "DELETE FROM chat_file_workspace_files WHERE tenant_id = ?",
        ] {
            sqlx::query(statement)
                .bind(&tenant)
                .execute(&db)
                .await
                .expect("clean isolation fixture");
        }
        let _ = std::fs::remove_dir_all(project_root);
    }
}
