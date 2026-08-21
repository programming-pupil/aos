//! Unified error type for the web server.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("authentication failed")]
    Unauthorized,

    #[error("access denied")]
    Forbidden,

    #[error("resource not found: {0}")]
    NotFound(String),

    #[error("validation error: {0}")]
    ValidationError(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("rate limit exceeded: {0}")]
    TooManyRequests(String),

    #[error("payload too large: {0}")]
    PayloadTooLarge(String),

    #[error("internal server error: {0}")]
    Internal(String),

    #[error("database error")]
    Database(#[from] sqlx::Error),

    #[error("json error")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("jwt error")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("bcrypt error")]
    Bcrypt(#[from] bcrypt::BcryptError),

    #[error("not implemented: {0}")]
    Unimplemented(String),

    #[error("cache expired or not found: {0}")]
    ExplainCacheExpired(String),
}

impl From<crate::semantic_kernel_store::SemanticStoreError> for AppError {
    fn from(error: crate::semantic_kernel_store::SemanticStoreError) -> Self {
        use crate::semantic_kernel_store::SemanticStoreError;
        match error {
            SemanticStoreError::InvalidEvent(message) => Self::ValidationError(message),
            SemanticStoreError::StaleWriter { thread_id } => {
                Self::Conflict(format!("stale writer lease for thread {thread_id}"))
            }
            SemanticStoreError::Sequence {
                thread_id,
                expected,
                actual,
            } => Self::Conflict(format!(
                "ledger sequence conflict for thread {thread_id}: expected {expected}, got {actual}"
            )),
            SemanticStoreError::Database(error) => Self::Database(error),
            corruption @ SemanticStoreError::Corruption { .. } => {
                Self::Internal(corruption.to_string())
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, self.to_string()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::ValidationError(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::TooManyRequests(msg) => (StatusCode::TOO_MANY_REQUESTS, msg.clone()),
            AppError::PayloadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg.clone()),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            AppError::Database(e) => {
                if let Some(mapped) = map_database_error(e) {
                    mapped
                } else {
                    (StatusCode::INTERNAL_SERVER_ERROR, "database error".into())
                }
            }
            AppError::Json(e) => (StatusCode::BAD_REQUEST, format!("json error: {e}")),
            AppError::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("io error: {e}")),
            AppError::Jwt(e) => (StatusCode::UNAUTHORIZED, format!("jwt error: {e}")),
            AppError::Bcrypt(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("bcrypt error: {e}"),
            ),
            AppError::Unimplemented(msg) => (StatusCode::NOT_IMPLEMENTED, msg.clone()),
            AppError::ExplainCacheExpired(msg) => (StatusCode::GONE, msg.clone()),
        };

        let message =
            runtime::protect_sensitive_text(&message, runtime::configured_data_protection_mode())
                .value;
        log_app_error(status, &message, &self);

        let body = Json(json!({
            "error": message,
            "status": status.as_u16(),
        }));

        (status, body).into_response()
    }
}

fn log_app_error(status: StatusCode, message: &str, error: &AppError) {
    let safe_error = runtime::protect_sensitive_text(
        &format!("{error:?}"),
        runtime::configured_data_protection_mode(),
    )
    .value;
    if status.is_server_error() {
        match error {
            AppError::Database(db_error) => {
                let safe_db_error = runtime::protect_sensitive_text(
                    &db_error.to_string(),
                    runtime::configured_data_protection_mode(),
                )
                .value;
                tracing::error!(
                    status = status.as_u16(),
                    error = %message,
                    db_error = %safe_db_error,
                    app_error = %safe_error,
                    "request failed"
                );
            }
            _ => {
                tracing::error!(
                    status = status.as_u16(),
                    error = %message,
                    app_error = %safe_error,
                    "request failed"
                );
            }
        }
    } else {
        tracing::warn!(
            status = status.as_u16(),
            error = %message,
            app_error = %safe_error,
            "request rejected"
        );
    }
}

fn map_database_error(err: &sqlx::Error) -> Option<(StatusCode, String)> {
    let sqlx::Error::Database(db_err) = err else {
        return None;
    };

    let code = db_err.code().map(|c| c.to_string()).unwrap_or_default();
    let message = db_err.message().to_ascii_lowercase();

    let is_duplicate_key = db_err.is_unique_violation()
        || code == "1062"
        || (code == "23000" && message.contains("duplicate entry"))
        || message.contains("duplicate entry");
    if !is_duplicate_key {
        return None;
    }

    if message.contains("uk_tenant_ds_user") {
        return Some((
            StatusCode::CONFLICT,
            "query policy already exists for this user and data source".to_string(),
        ));
    }
    if message.contains("uk_relation") {
        return Some((
            StatusCode::CONFLICT,
            "cross-datasource relation already exists".to_string(),
        ));
    }

    Some((StatusCode::CONFLICT, "resource already exists".to_string()))
}
