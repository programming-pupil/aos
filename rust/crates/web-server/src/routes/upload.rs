//! File upload endpoint — stores files under `$data_dir/.aos/uploads/{user_id}/{uuid}.{ext}`
//! and returns metadata for embedding in chat messages.

use axum::{
    extract::{multipart::MultipartError, DefaultBodyLimit, Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::auth::Claims;
use crate::error::AppError;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Uploaded file metadata (returned to the frontend)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResponse {
    pub file_id: String,
    pub filename: String,
    pub media_type: String,
    pub size: u64,
    pub url: String,
}

pub(crate) async fn store_upload_bytes(
    state: &AppState,
    user_id: &str,
    filename: &str,
    media_type: &str,
    data: &[u8],
) -> Result<UploadResponse, AppError> {
    if user_id.is_empty()
        || user_id.contains('/')
        || user_id.contains('\\')
        || user_id.contains("..")
        || user_id.contains('\0')
    {
        return Err(AppError::ValidationError(
            "invalid upload user id".to_string(),
        ));
    }
    if !allowed_media_type(media_type) {
        return Err(AppError::ValidationError(format!(
            "unsupported media type: {media_type}. Allowed: images and common document types."
        )));
    }
    let max_file_bytes = upload_max_file_bytes();
    let max_image_bytes = upload_max_image_bytes(max_file_bytes);
    if media_type.starts_with("image/") && data.len() > max_image_bytes {
        return Err(AppError::PayloadTooLarge(format!(
            "image too large: {} bytes (max {} bytes)",
            data.len(),
            max_image_bytes
        )));
    }
    if data.len() > max_file_bytes {
        return Err(AppError::PayloadTooLarge(format!(
            "file too large: {} bytes (max {} bytes)",
            data.len(),
            max_file_bytes
        )));
    }

    let file_id = uuid::Uuid::new_v4().to_string();
    let ext = extension_for_media_type(media_type);
    let saved_filename = format!("{file_id}.{ext}");
    let dir = uploads_dir(&state.data_dir, user_id);
    tokio::fs::create_dir_all(&dir).await.map_err(|error| {
        tracing::error!(
            user_id,
            dir = %dir.display(),
            error = %error,
            "upload failed: create upload directory"
        );
        AppError::Internal(format!(
            "failed to create upload directory {}: {error}",
            dir.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| {
                AppError::Internal(format!(
                    "failed to secure upload directory {}: {error}",
                    dir.display()
                ))
            })?;
    }
    let path = dir.join(&saved_filename);
    tokio::fs::write(&path, data).await.map_err(|error| {
        tracing::error!(
            user_id,
            path = %path.display(),
            media_type,
            size = data.len(),
            error = %error,
            "upload failed: persist file"
        );
        AppError::Internal(format!(
            "failed to write upload file {}: {error}",
            path.display()
        ))
    })?;
    let url = format!("/api/v1/uploads/{user_id}/{saved_filename}");
    Ok(UploadResponse {
        file_id,
        filename: filename.to_string(),
        media_type: media_type.to_string(),
        size: data.len() as u64,
        url,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn uploads_dir(data_dir: &Path, user_id: &str) -> PathBuf {
    data_dir.join(".aos").join("uploads").join(user_id)
}

fn allowed_media_type(media_type: &str) -> bool {
    let allowed = [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/heic",
        "image/heif",
        "application/pdf",
        "text/plain",
        "text/markdown",
        "text/csv",
        "application/sql",
        "text/rtf",
        "text/html",
        "text/css",
        "text/javascript",
        "application/json",
        "application/xml",
        "application/zip",
        "application/rtf",
        "application/msword",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "application/vnd.oasis.opendocument.text",
    ];
    allowed.contains(&media_type)
}

pub(crate) fn media_type_for_filename(filename: &str) -> Option<&'static str> {
    let ext = filename
        .rsplit_once('.')
        .map_or("", |(_, ext)| ext)
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "heic" => Some("image/heic"),
        "heif" => Some("image/heif"),
        "pdf" => Some("application/pdf"),
        "txt" | "log" => Some("text/plain"),
        "sql" => Some("application/sql"),
        "md" | "markdown" => Some("text/markdown"),
        "csv" => Some("text/csv"),
        "rtf" => Some("application/rtf"),
        "html" | "htm" => Some("text/html"),
        "css" => Some("text/css"),
        "js" | "mjs" | "ts" | "tsx" | "jsx" => Some("text/javascript"),
        "json" => Some("application/json"),
        "xml" => Some("application/xml"),
        "zip" => Some("application/zip"),
        "doc" => Some("application/msword"),
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "odt" => Some("application/vnd.oasis.opendocument.text"),
        _ => None,
    }
}

fn extension_for_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "application/sql" => "sql",
        "text/rtf" | "application/rtf" => "rtf",
        "text/html" => "html",
        "text/css" => "css",
        "text/javascript" => "js",
        "application/json" => "json",
        "application/xml" => "xml",
        "application/zip" => "zip",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.oasis.opendocument.text" => "odt",
        _ => "bin",
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn upload_max_file_bytes() -> usize {
    env_usize("UPLOAD_MAX_FILE_BYTES", 50 * 1024 * 1024)
}

fn upload_max_image_bytes(max_file_bytes: usize) -> usize {
    env_usize("UPLOAD_MAX_IMAGE_BYTES", 1024 * 1024).min(max_file_bytes)
}

fn multipart_to_app_error(
    stage: &str,
    user_id: &str,
    filename: Option<&str>,
    content_type: Option<&str>,
    err: MultipartError,
) -> AppError {
    let status = err.status();
    let detail = err.body_text();
    let max_file_bytes = upload_max_file_bytes();
    let max_image_bytes = upload_max_image_bytes(max_file_bytes);

    match status {
        StatusCode::PAYLOAD_TOO_LARGE => {
            tracing::warn!(
                stage,
                user_id = %user_id,
                filename = ?filename,
                content_type = ?content_type,
                max_image_bytes,
                max_file_bytes,
                error = %err,
                detail = %detail,
                "upload rejected: payload too large"
            );
            AppError::PayloadTooLarge(format!(
                "upload payload too large. image limit={} bytes, file limit={} bytes",
                max_image_bytes, max_file_bytes
            ))
        }
        _ => {
            tracing::warn!(
                stage,
                user_id = %user_id,
                filename = ?filename,
                content_type = ?content_type,
                status = %status,
                error = %err,
                detail = %detail,
                "upload rejected: invalid multipart payload"
            );
            AppError::ValidationError(format!("invalid multipart/form-data: {detail}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Route handler
// ---------------------------------------------------------------------------

async fn upload_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut request: axum::extract::Multipart,
) -> Result<axum::response::Response, AppError> {
    let user_id = &claims.sub;

    let field = match request.next_field().await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return Err(AppError::ValidationError("no file field provided".into()));
        }
        Err(e) => {
            return Err(multipart_to_app_error("next_field", user_id, None, None, e));
        }
    };

    let filename = field
        .file_name()
        .map_or_else(|| "upload".to_string(), ToString::to_string);

    let content_type = field.content_type().map(ToString::to_string);

    let data = match field.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            return Err(multipart_to_app_error(
                "read_bytes",
                user_id,
                Some(&filename),
                content_type.as_deref(),
                e,
            ));
        }
    };

    let media_type = content_type
        .filter(|value| {
            let value = value.trim();
            !value.is_empty() && value != "application/octet-stream"
        })
        .or_else(|| media_type_for_filename(&filename).map(ToString::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let uploaded = store_upload_bytes(&state, user_id, &filename, &media_type, &data).await?;
    Ok(Json(uploaded).into_response())
}

// ---------------------------------------------------------------------------
// Serve uploaded files (static)
// ---------------------------------------------------------------------------

async fn serve_file(
    State(state): State<AppState>,
    axum::extract::Path((user_id, filename)): axum::extract::Path<(String, String)>,
    Extension(claims): Extension<Claims>,
) -> Result<axum::response::Response, AppError> {
    // Users can only access their own uploads
    if claims.sub != user_id {
        return Err(AppError::ValidationError("unauthorized".into()));
    }

    let ext = filename.rsplit_once('.').map_or("", |(_, e)| e);
    let media_type = match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "webm" => "audio/webm",
        "opus" => "audio/opus",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "sql" => "application/sql",
        "rtf" => "application/rtf",
        "doc" => "application/msword",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    };

    let path = uploads_dir(&state.data_dir, &user_id).join(&filename);
    if !path.exists() {
        return Err(AppError::ValidationError("file not found".into()));
    }

    let body = tokio::fs::read(&path).await.map_err(|e| {
        tracing::error!(
            user_id = %user_id,
            path = %path.display(),
            error = %e,
            "serve upload failed: read file"
        );
        AppError::Internal(format!("failed to read file {}: {e}", path.display()))
    })?;
    axum::response::Response::builder()
        .header("Content-Type", media_type)
        .header("Content-Length", body.len())
        .header("Cache-Control", "private, max-age=3600")
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Security-Policy", "sandbox; default-src 'none'")
        .header(
            "Content-Disposition",
            if media_type.starts_with("image/") {
                "inline"
            } else {
                "attachment"
            },
        )
        .body(axum::body::Body::from(body))
        .map_err(|_| AppError::Internal("failed to build response".into()))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn routes(state: AppState) -> Router<AppState> {
    let max_file_bytes = upload_max_file_bytes();
    Router::new()
        .route(
            "/upload",
            post(upload_file).layer(DefaultBodyLimit::max(max_file_bytes)),
        )
        .route("/{user_id}/{filename}", get(serve_file))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}
