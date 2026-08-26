//! Authenticated file management for a user's isolated workspace.

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::{header, Request, Response as HttpResponse};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path as FsPath, PathBuf};
use std::time::UNIX_EPOCH;

use crate::auth::Claims;
use crate::error::{AppError, Result as AppResult};
use crate::state::AppState;

const VIRTUAL_ROOT: &str = "/projects/session";
const RESERVED_ROOT_NAMES: &[&str] = &[".aos", ".sandbox-home", ".sandbox-tmp"];
const MAX_EDITABLE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    path: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathQuery {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileRequest {
    path: String,
    content: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryRequest {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameRequest {
    path: String,
    new_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteQuery {
    path: String,
    #[serde(default)]
    recursive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadQuery {
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadListQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceFileItem {
    name: String,
    path: String,
    kind: String,
    size_bytes: u64,
    updated_at: Option<String>,
    editable: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceFileList {
    path: String,
    absolute_path: String,
    items: Vec<WorkspaceFileItem>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileCursor {
    key: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadCursor {
    updated_at: String,
    file_id: String,
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/items",
            get(list_files).post(write_file).delete(delete_item),
        )
        .route("/items/content", get(read_file_content))
        .route("/items/download", get(download_file))
        .route("/directories", post(create_directory))
        .route("/rename", post(rename_item))
        .route(
            "/upload",
            post(upload_workspace_file)
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1024 * 1024)),
        )
        .route("/uploads", get(list_indexed_uploads))
        .route("/uploads/{file_id}", delete(delete_indexed_upload))
        .merge(crate::routes::workspace_automation::routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_workspace_menu_permission,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

fn workspace_menu_permission_allowed(role: &str, menu_permissions_json: Option<&str>) -> bool {
    if let Some(raw) = menu_permissions_json {
        return serde_json::from_str::<Vec<String>>(raw)
            .unwrap_or_default()
            .iter()
            .any(|permission| permission == "workspace:read");
    }
    matches!(role, "viewer" | "developer" | "admin" | "superadmin")
}

async fn require_workspace_menu_permission(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let row = sqlx::query(
        "SELECT role, CAST(menu_permissions_json AS TEXT) AS menu_permissions_json
         FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1 LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await;
    match row {
        Ok(Some(row))
            if workspace_menu_permission_allowed(
                &row.get::<String, _>("role"),
                row.get::<Option<String>, _>("menu_permissions_json")
                    .as_deref(),
            ) =>
        {
            next.run(request).await
        }
        Ok(_) => AppError::Forbidden.into_response(),
        Err(error) => AppError::Database(error).into_response(),
    }
}

fn validate_storage_component(value: &str) -> AppResult<()> {
    if value.is_empty()
        || value != value.trim()
        || value == "."
        || value == ".."
        || value.chars().count() > 255
        || value.chars().any(char::is_control)
        || value.contains('/')
        || value.contains('\\')
        || value.contains(':')
    {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

fn ensure_directory_child(parent: &FsPath, name: &str) -> AppResult<PathBuf> {
    let path = parent.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(AppError::Forbidden);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&path)?,
        Err(error) => return Err(error.into()),
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(parent) {
        return Err(AppError::Forbidden);
    }
    Ok(canonical)
}

pub(crate) fn ensure_workspace_root_for_user(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
) -> AppResult<PathBuf> {
    validate_storage_component(tenant_id)?;
    validate_storage_component(user_id)?;
    fs::create_dir_all(&state.data_dir)?;
    let data_root = state.data_dir.canonicalize()?;
    let tenant_root = ensure_directory_child(&data_root, tenant_id)?;
    let user_root = ensure_directory_child(&tenant_root, user_id)?;
    let root = ensure_directory_child(&user_root, "workspace")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    Ok(root)
}

fn ensure_workspace_root(state: &AppState, claims: &Claims) -> AppResult<PathBuf> {
    ensure_workspace_root_for_user(state, &claims.tenant_id, &claims.sub)
}

fn normalize_virtual_path(raw: &str) -> AppResult<(String, Vec<String>)> {
    let raw = raw.trim();
    if raw.chars().count() > 4_096
        || raw.chars().any(char::is_control)
        || raw.contains('\\')
        || raw.contains(':')
    {
        return Err(AppError::ValidationError(
            "invalid workspace path".to_string(),
        ));
    }
    let normalized = if raw.is_empty() || raw == "/" {
        VIRTUAL_ROOT.to_string()
    } else {
        let trimmed = raw.trim_end_matches('/');
        if trimmed == VIRTUAL_ROOT {
            VIRTUAL_ROOT.to_string()
        } else if trimmed.starts_with(&format!("{VIRTUAL_ROOT}/")) {
            trimmed.to_string()
        } else {
            return Err(AppError::ValidationError(
                "workspace path must be inside the personal file root".to_string(),
            ));
        }
    };
    let relative = normalized
        .strip_prefix(VIRTUAL_ROOT)
        .unwrap_or_default()
        .trim_start_matches('/');
    let mut segments = Vec::new();
    for component in FsPath::new(relative).components() {
        let Component::Normal(value) = component else {
            return Err(AppError::ValidationError(
                "workspace path traversal is not allowed".to_string(),
            ));
        };
        let value = value.to_string_lossy().to_string();
        validate_name(&value)?;
        segments.push(value);
    }
    if segments.first().is_some_and(|name| {
        RESERVED_ROOT_NAMES
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
    }) {
        return Err(AppError::NotFound("workspace item not found".to_string()));
    }
    Ok((normalized, segments))
}

fn validate_name(name: &str) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.chars().count() > 255
        || name.chars().any(char::is_control)
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
    {
        return Err(AppError::ValidationError(
            "invalid workspace item name".to_string(),
        ));
    }
    Ok(())
}

fn reject_symlink_components(root: &FsPath, segments: &[String]) -> AppResult<()> {
    let mut current = root.to_path_buf();
    for segment in segments {
        current.push(segment);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AppError::ValidationError(
                    "workspace symbolic links are not supported".to_string(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn existing_path(root: &FsPath, segments: &[String]) -> AppResult<PathBuf> {
    reject_symlink_components(root, segments)?;
    let path = segments
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part));
    let canonical = path
        .canonicalize()
        .map_err(|_| AppError::NotFound("workspace item not found".to_string()))?;
    if !canonical.starts_with(root) {
        return Err(AppError::NotFound("workspace item not found".to_string()));
    }
    Ok(canonical)
}

pub(crate) fn resolve_workspace_directory_for_user(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    virtual_path: &str,
) -> AppResult<(PathBuf, PathBuf, String)> {
    let root = ensure_workspace_root_for_user(state, tenant_id, user_id)?;
    let (normalized, segments) = normalize_virtual_path(virtual_path)?;
    let directory = existing_path(&root, &segments)?;
    if !directory.is_dir() {
        return Err(AppError::ValidationError(
            "workspace command cwd must be a directory".to_string(),
        ));
    }
    Ok((root, directory, normalized))
}

/// Validate an existing workspace path while preserving its canonical virtual
/// representation.  Schedule APIs use this for both scripts and directories.
pub(crate) fn validate_workspace_file_or_directory_for_user(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    virtual_path: &str,
    require_file: bool,
) -> AppResult<String> {
    let root = ensure_workspace_root_for_user(state, tenant_id, user_id)?;
    let (normalized, segments) = normalize_virtual_path(virtual_path)?;
    let path = existing_path(&root, &segments)?;
    let valid = if require_file {
        path.is_file()
    } else {
        path.is_file() || path.is_dir()
    };
    if !valid {
        return Err(AppError::ValidationError(if require_file {
            "workspace path must be a file".to_string()
        } else {
            "workspace path must be a file or directory".to_string()
        }));
    }
    Ok(normalized)
}

fn create_path(root: &FsPath, segments: &[String]) -> AppResult<PathBuf> {
    if segments.is_empty() {
        return Err(AppError::ValidationError(
            "the workspace root cannot be modified".to_string(),
        ));
    }
    reject_symlink_components(root, segments)?;
    let parent_segments = &segments[..segments.len() - 1];
    let parent = existing_path(root, parent_segments)?;
    if !parent.is_dir() {
        return Err(AppError::ValidationError(
            "workspace parent is not a directory".to_string(),
        ));
    }
    Ok(parent.join(&segments[segments.len() - 1]))
}

fn virtual_child(parent: &str, name: &str) -> String {
    format!("{}/{}", parent.trim_end_matches('/'), name)
}

fn item_key(item: &WorkspaceFileItem) -> String {
    format!(
        "{}:{}:{}",
        if item.kind == "directory" { '0' } else { '1' },
        item.name.to_lowercase(),
        item.name,
    )
}

fn item_order(left: &WorkspaceFileItem, right: &WorkspaceFileItem) -> Ordering {
    item_key(left).cmp(&item_key(right))
}

fn encode_file_cursor(key: &str) -> String {
    URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&FileCursor {
            key: key.to_string(),
        })
        .unwrap_or_default(),
    )
}

fn decode_file_cursor(cursor: Option<&str>) -> AppResult<Option<String>> {
    cursor
        .map(|cursor| {
            let bytes = URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|_| AppError::ValidationError("invalid workspace cursor".to_string()))?;
            let cursor: FileCursor = serde_json::from_slice(&bytes)
                .map_err(|_| AppError::ValidationError("invalid workspace cursor".to_string()))?;
            Ok(cursor.key)
        })
        .transpose()
}

fn modified_at(metadata: &fs::Metadata) -> Option<String> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs().to_string())
}

fn is_editable_file(path: &FsPath, size: u64) -> bool {
    if size > MAX_EDITABLE_BYTES {
        return false;
    }
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "txt"
            | "md"
            | "markdown"
            | "sql"
            | "csv"
            | "json"
            | "jsonl"
            | "xml"
            | "html"
            | "css"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "py"
            | "rs"
            | "go"
            | "java"
            | "kt"
            | "sh"
            | "yaml"
            | "yml"
            | "toml"
            | "log"
    )
}

async fn list_files(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListQuery>,
) -> AppResult<Json<WorkspaceFileList>> {
    let root = ensure_workspace_root(&state, &claims)?;
    let (virtual_path, segments) =
        normalize_virtual_path(query.path.as_deref().unwrap_or(VIRTUAL_ROOT))?;
    let directory = existing_path(&root, &segments)?;
    if !directory.is_dir() {
        return Err(AppError::ValidationError(
            "workspace path is not a directory".to_string(),
        ));
    }
    let after = decode_file_cursor(query.cursor.as_deref())?;
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    // Keep only the requested window plus one look-ahead item.  Directory
    // pagination remains deterministic without retaining/sorting an entire
    // multi-thousand-file workspace in memory.
    let window_size = limit.saturating_add(1);
    let mut window = BTreeMap::<String, WorkspaceFileItem>::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if segments.is_empty()
            && RESERVED_ROOT_NAMES
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            continue;
        }
        let kind = if metadata.is_dir() {
            "directory"
        } else {
            "file"
        };
        let item = WorkspaceFileItem {
            path: virtual_child(&virtual_path, &name),
            name,
            kind: kind.to_string(),
            size_bytes: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
            updated_at: modified_at(&metadata),
            editable: metadata.is_file() && is_editable_file(&entry.path(), metadata.len()),
        };
        let key = item_key(&item);
        if after
            .as_ref()
            .is_some_and(|cursor| key.as_str() <= cursor.as_str())
        {
            continue;
        }
        window.insert(key, item);
        if window.len() > window_size {
            if let Some(last) = window.keys().next_back().cloned() {
                window.remove(&last);
            }
        }
    }
    let mut items = window.into_values().collect::<Vec<_>>();
    items.sort_by(item_order);
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = has_more
        .then(|| items.last().map(|item| encode_file_cursor(&item_key(item))))
        .flatten();
    Ok(Json(WorkspaceFileList {
        path: virtual_path,
        absolute_path: directory.to_string_lossy().into_owned(),
        items,
        next_cursor,
        has_more,
    }))
}

async fn read_file_content(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PathQuery>,
) -> AppResult<Json<Value>> {
    let root = ensure_workspace_root(&state, &claims)?;
    let (virtual_path, segments) = normalize_virtual_path(&query.path)?;
    let path = existing_path(&root, &segments)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() || !is_editable_file(&path, metadata.len()) {
        return Err(AppError::ValidationError(
            "workspace file is not editable text or exceeds 2 MiB".to_string(),
        ));
    }
    let content = fs::read_to_string(&path).map_err(|_| {
        AppError::ValidationError("workspace file is not valid UTF-8 text".to_string())
    })?;
    Ok(Json(json!({
        "path": virtual_path,
        "content": content,
        "sizeBytes": metadata.len(),
        "updatedAt": modified_at(&metadata),
    })))
}

async fn write_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<WriteFileRequest>,
) -> AppResult<Json<Value>> {
    if request.content.len() > MAX_EDITABLE_BYTES as usize {
        return Err(AppError::PayloadTooLarge(
            "workspace text file exceeds 2 MiB".to_string(),
        ));
    }
    let root = ensure_workspace_root(&state, &claims)?;
    let (virtual_path, segments) = normalize_virtual_path(&request.path)?;
    let path = create_path(&root, &segments)?;
    if path.exists() && !request.overwrite {
        return Err(AppError::ValidationError(
            "workspace item already exists".to_string(),
        ));
    }
    if path.is_dir() {
        return Err(AppError::ValidationError(
            "workspace path is a directory".to_string(),
        ));
    }
    let temp = path.with_file_name(format!(".aos-write-{}", uuid::Uuid::new_v4().simple()));
    fs::write(&temp, request.content.as_bytes())?;
    fs::rename(&temp, &path)?;
    Ok(Json(json!({
        "path": virtual_path,
        "saved": true,
        "sizeBytes": request.content.len(),
    })))
}

async fn create_directory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<DirectoryRequest>,
) -> AppResult<Json<Value>> {
    let root = ensure_workspace_root(&state, &claims)?;
    let (virtual_path, segments) = normalize_virtual_path(&request.path)?;
    let path = create_path(&root, &segments)?;
    if path.exists() {
        return Err(AppError::ValidationError(
            "workspace item already exists".to_string(),
        ));
    }
    fs::create_dir(&path)?;
    Ok(Json(json!({"path": virtual_path, "created": true})))
}

async fn rename_item(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<RenameRequest>,
) -> AppResult<Json<Value>> {
    validate_name(&request.new_name)?;
    let root = ensure_workspace_root(&state, &claims)?;
    let (virtual_path, segments) = normalize_virtual_path(&request.path)?;
    if segments.is_empty() {
        return Err(AppError::ValidationError(
            "the workspace root cannot be renamed".to_string(),
        ));
    }
    let path = existing_path(&root, &segments)?;
    let destination = path
        .parent()
        .ok_or_else(|| AppError::ValidationError("invalid workspace path".to_string()))?
        .join(request.new_name.trim());
    if destination.exists() {
        return Err(AppError::ValidationError(
            "workspace item already exists".to_string(),
        ));
    }
    fs::rename(&path, &destination)?;
    let parent = virtual_path.rsplit_once('/').map_or(VIRTUAL_ROOT, |v| v.0);
    Ok(Json(json!({
        "path": virtual_child(parent, request.new_name.trim()),
        "renamed": true,
    })))
}

async fn delete_item(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<DeleteQuery>,
) -> AppResult<Json<Value>> {
    let root = ensure_workspace_root(&state, &claims)?;
    let (_, segments) = normalize_virtual_path(&query.path)?;
    if segments.is_empty() {
        return Err(AppError::ValidationError(
            "the workspace root cannot be deleted".to_string(),
        ));
    }
    let path = existing_path(&root, &segments)?;
    if path.is_dir() {
        if query.recursive {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_dir(path)?;
        }
    } else {
        fs::remove_file(path)?;
    }
    Ok(Json(json!({"deleted": true})))
}

async fn upload_workspace_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<UploadQuery>,
    mut multipart: axum::extract::Multipart,
) -> AppResult<Json<Value>> {
    let root = ensure_workspace_root(&state, &claims)?;
    let (virtual_parent, parent_segments) =
        normalize_virtual_path(query.path.as_deref().unwrap_or(VIRTUAL_ROOT))?;
    let parent = existing_path(&root, &parent_segments)?;
    if !parent.is_dir() {
        return Err(AppError::ValidationError(
            "workspace upload target is not a directory".to_string(),
        ));
    }
    let field = multipart
        .next_field()
        .await
        .map_err(|error| AppError::ValidationError(format!("invalid upload: {error}")))?
        .ok_or_else(|| AppError::ValidationError("no file field provided".to_string()))?;
    let original_name = field.file_name().unwrap_or("upload").to_string();
    validate_name(&original_name)?;
    let bytes = field
        .bytes()
        .await
        .map_err(|error| AppError::ValidationError(format!("invalid upload: {error}")))?;
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(AppError::PayloadTooLarge(
            "workspace upload exceeds 50 MiB".to_string(),
        ));
    }
    let destination = unique_upload_destination(&parent, &original_name);
    fs::write(&destination, &bytes)?;
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&original_name);
    Ok(Json(json!({
        "path": virtual_child(&virtual_parent, name),
        "filename": name,
        "sizeBytes": bytes.len(),
        "uploaded": true,
    })))
}

fn unique_upload_destination(parent: &FsPath, original_name: &str) -> PathBuf {
    let initial = parent.join(original_name);
    if !initial.exists() {
        return initial;
    }
    let path = FsPath::new(original_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let name = extension.map_or_else(
            || format!("{stem} ({index})"),
            |extension| format!("{stem} ({index}).{extension}"),
        );
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{}-{}", uuid::Uuid::new_v4(), original_name))
}

async fn download_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PathQuery>,
) -> AppResult<HttpResponse<Body>> {
    let root = ensure_workspace_root(&state, &claims)?;
    let (_, segments) = normalize_virtual_path(&query.path)?;
    let path = existing_path(&root, &segments)?;
    if !path.is_file() {
        return Err(AppError::ValidationError(
            "workspace path is not a file".to_string(),
        ));
    }
    let body = tokio::fs::read(&path).await?;
    HttpResponse::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(body))
        .map_err(|error| AppError::Internal(format!("failed to build download: {error}")))
}

fn encode_upload_cursor(cursor: &UploadCursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).unwrap_or_default())
}

fn decode_upload_cursor(value: Option<&str>) -> AppResult<Option<UploadCursor>> {
    value
        .map(|value| {
            let bytes = URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| AppError::ValidationError("invalid upload cursor".to_string()))?;
            serde_json::from_slice(&bytes)
                .map_err(|_| AppError::ValidationError("invalid upload cursor".to_string()))
        })
        .transpose()
}

async fn list_indexed_uploads(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<UploadListQuery>,
) -> AppResult<Json<Value>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let cursor = decode_upload_cursor(query.cursor.as_deref())?;
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT file_id, filename, media_type, CAST(size_bytes AS INTEGER) AS size_bytes, url, status, error_message, session_id, CAST(updated_at AS TEXT) AS updated_at FROM chat_file_workspace_files WHERE tenant_id = ",
    );
    builder
        .push_bind(&claims.tenant_id)
        .push(" AND user_id = ")
        .push_bind(&claims.sub);
    if let Some(cursor) = &cursor {
        builder
            .push(" AND (updated_at < ")
            .push_bind(&cursor.updated_at)
            .push(" OR (updated_at = ")
            .push_bind(&cursor.updated_at)
            .push(" AND file_id < ")
            .push_bind(&cursor.file_id)
            .push("))");
    }
    builder
        .push(" ORDER BY updated_at DESC, file_id DESC LIMIT ")
        .push_bind(i64::try_from(limit.saturating_add(1)).unwrap_or(101));
    let mut rows = builder.build().fetch_all(&state.db).await?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        rows.last().map(|row| {
            encode_upload_cursor(&UploadCursor {
                updated_at: row.get("updated_at"),
                file_id: row.get("file_id"),
            })
        })
    } else {
        None
    };
    Ok(Json(json!({
        "items": rows.into_iter().map(|row| json!({
            "fileId": row.get::<String, _>("file_id"),
            "filename": row.get::<String, _>("filename"),
            "mediaType": row.get::<String, _>("media_type"),
            "sizeBytes": row.try_get::<u64, _>("size_bytes").unwrap_or(0),
            "url": row.get::<String, _>("url"),
            "status": row.get::<String, _>("status"),
            "errorMessage": row.get::<Option<String>, _>("error_message"),
            "sessionId": row.get::<Option<String>, _>("session_id"),
            "updatedAt": row.get::<String, _>("updated_at"),
        })).collect::<Vec<_>>(),
        "nextCursor": next_cursor,
        "hasMore": has_more,
    })))
}

async fn delete_indexed_upload(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(file_id): Path<String>,
) -> AppResult<Json<Value>> {
    validate_storage_component(&claims.sub)?;
    let row = sqlx::query(
        "SELECT url FROM chat_file_workspace_files WHERE tenant_id = ? AND user_id = ? AND file_id = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&file_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("workspace upload not found".to_string()))?;
    let url = row.get::<String, _>("url");
    let mut transaction = state.db.begin().await?;
    sqlx::query(
        "DELETE FROM chat_file_workspace_chunks WHERE tenant_id = ? AND user_id = ? AND file_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&file_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "DELETE FROM chat_file_workspace_files WHERE tenant_id = ? AND user_id = ? AND file_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&file_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "UPDATE agent_workspace_entries SET enabled = 0, is_current = 0, deleted_at = CURRENT_TIMESTAMP WHERE tenant_id = ? AND owner_user_id = ? AND resource_type = 'upload' AND resource_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&file_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "UPDATE agent_workspaces SET acl_version = acl_version + 1, updated_at = CURRENT_TIMESTAMP WHERE tenant_id = ? AND owner_user_id = ? AND workspace_type = 'personal'",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    if let Some(filename) = upload_storage_filename(&url, &claims.sub) {
        let path = state
            .data_dir
            .join(".aos")
            .join("uploads")
            .join(&claims.sub)
            .join(filename);
        let _ = fs::remove_file(path);
    }
    Ok(Json(json!({"deleted": true, "fileId": file_id})))
}

fn upload_storage_filename<'a>(url: &'a str, user_id: &str) -> Option<&'a str> {
    let prefix = format!("/api/v1/uploads/{user_id}/");
    let filename = url.strip_prefix(&prefix)?;
    (!filename.is_empty()
        && FsPath::new(filename)
            .file_name()
            .and_then(|value| value.to_str())
            == Some(filename))
    .then_some(filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_paths_are_confined_to_personal_files() {
        assert!(normalize_virtual_path(VIRTUAL_ROOT).is_ok());
        assert!(normalize_virtual_path(&format!("{VIRTUAL_ROOT}/reports/a.sql")).is_ok());
        assert!(normalize_virtual_path(&format!("{VIRTUAL_ROOT}/user-files/a.sql")).is_ok());
        for path in [
            "/projects/other",
            "/projects/session/../secret",
            "/projects/session/a\\b",
            "/projects/session/C:secret",
            "/projects/session/.aos/sessions",
            "/projects/session/.SANDBOX-HOME/secret",
        ] {
            assert!(normalize_virtual_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn upload_storage_filename_requires_current_user_prefix() {
        assert_eq!(
            upload_storage_filename("/api/v1/uploads/user-a/file.txt", "user-a"),
            Some("file.txt")
        );
        assert_eq!(
            upload_storage_filename("/api/v1/uploads/user-b/file.txt", "user-a"),
            None
        );
        assert_eq!(
            upload_storage_filename("/api/v1/uploads/user-a/../file.txt", "user-a"),
            None
        );
    }

    #[test]
    fn storage_identity_components_cannot_escape_the_managed_root() {
        for value in ["tenant-a", "5399bca6-be14-43d0-8cdb-a57d0f5b9c55"] {
            assert!(validate_storage_component(value).is_ok(), "{value}");
        }
        for value in ["", ".", "..", "../user-b", "/user-b", "a\\b", "C:temp"] {
            assert!(validate_storage_component(value).is_err(), "{value}");
        }
    }

    #[test]
    fn workspace_menu_permission_honors_inherited_and_custom_access() {
        assert!(workspace_menu_permission_allowed("viewer", None));
        assert!(workspace_menu_permission_allowed(
            "viewer",
            Some(r#"["workspace:read"]"#)
        ));
        assert!(!workspace_menu_permission_allowed(
            "admin",
            Some(r#"["super_assistant:read"]"#)
        ));
        assert!(!workspace_menu_permission_allowed("unknown", None));
    }

    #[test]
    fn workspace_file_list_serializes_local_path_without_changing_virtual_paths() {
        let response = WorkspaceFileList {
            path: format!("{VIRTUAL_ROOT}/reports"),
            absolute_path: "/var/lib/aos/tenant-a/user-a/workspace/reports".to_string(),
            items: vec![WorkspaceFileItem {
                name: "report.md".to_string(),
                path: format!("{VIRTUAL_ROOT}/reports/report.md"),
                kind: "file".to_string(),
                size_bytes: 7,
                updated_at: None,
                editable: true,
            }],
            next_cursor: None,
            has_more: false,
        };

        let value = serde_json::to_value(response).expect("serialize workspace response");
        assert_eq!(
            value["absolutePath"],
            "/var/lib/aos/tenant-a/user-a/workspace/reports"
        );
        assert_eq!(value["path"], format!("{VIRTUAL_ROOT}/reports"));
        assert_eq!(
            value["items"][0]["path"],
            format!("{VIRTUAL_ROOT}/reports/report.md")
        );
        assert!(value.get("absolute_path").is_none());
    }

    #[test]
    fn managed_workspace_directories_are_isolated_by_tenant_and_user() {
        let test_root =
            std::env::temp_dir().join(format!("aos-workspace-path-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&test_root).expect("create isolated test root");
        let canonical_root = test_root.canonicalize().expect("canonical test root");

        let tenant_a =
            ensure_directory_child(&canonical_root, "tenant-a").expect("create first tenant root");
        let tenant_b =
            ensure_directory_child(&canonical_root, "tenant-b").expect("create second tenant root");
        let user_a = ensure_directory_child(&tenant_a, "user-a").expect("create first user root");
        let user_b = ensure_directory_child(&tenant_a, "user-b").expect("create second user root");
        let workspace_a =
            ensure_directory_child(&user_a, "workspace").expect("create first workspace");

        assert!(workspace_a.starts_with(&user_a));
        assert!(workspace_a.starts_with(&tenant_a));
        assert!(!workspace_a.starts_with(&user_b));
        assert!(!workspace_a.starts_with(&tenant_b));

        fs::remove_dir_all(&canonical_root).expect("remove isolated test root");
    }
}
