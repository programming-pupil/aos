//! Domain errors for the agent gateway.

use axum::response::IntoResponse;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GatewayError {
    #[error("concurrency limit exceeded: max {max}, current {current}")]
    ConcurrencyLimit { max: usize, current: usize },

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    #[error("user not found: {0}")]
    UserNotFound(String),

    #[error("workspace path escape attempt: requested {requested}, root is {root}")]
    PathEscape { requested: String, root: String },

    #[error("git operation failed: {0}")]
    GitError(String),

    #[error("runtime build failed: {0}")]
    RuntimeBuild(String),

    #[error("runtime execution failed: {0}")]
    RuntimeExecution(String),

    #[error("config load failed: {0}")]
    ConfigLoad(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("api error: {0}")]
    Api(#[from] api::ApiError),

    #[error("runtime error: {0}")]
    Runtime(#[from] runtime::RuntimeError),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("internal: {0}")]
    Internal(String),

    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),

    #[error("session already exists: {0}")]
    SessionAlreadyExists(String),

    #[error("project already exists: {0}")]
    ProjectAlreadyExists(String),

    #[error("session busy: another turn is already in progress for this session")]
    SessionBusy,

    #[error("turn cancelled")]
    TurnCancelled,

    #[error("invalid token")]
    InvalidToken,

    #[error("unauthorized")]
    Unauthorized,
}

pub type Result<T> = std::result::Result<T, GatewayError>;

impl GatewayError {
    #[must_use]
    pub fn http_status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Self::SessionNotFound(_) | Self::ProjectNotFound(_) | Self::UserNotFound(_) => {
                StatusCode::NOT_FOUND
            }
            Self::PathEscape { .. } => StatusCode::FORBIDDEN,
            Self::Unauthorized | Self::InvalidToken => StatusCode::UNAUTHORIZED,
            Self::ConcurrencyLimit { .. } | Self::QuotaExceeded(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::ProjectAlreadyExists(_)
            | Self::SessionAlreadyExists(_)
            | Self::SessionBusy
            | Self::TurnCancelled => StatusCode::CONFLICT,
            Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            Self::GitError(_)
            | Self::RuntimeBuild(_)
            | Self::RuntimeExecution(_)
            | Self::ConfigLoad(_)
            | Self::Api(_)
            | Self::Runtime(_)
            | Self::Tool(_)
            | Self::Database(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::Internal(_)
            | Self::Validation(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    #[must_use]
    pub fn is_context_window_exceeded(&self) -> bool {
        let text = self.to_string().to_ascii_lowercase();
        [
            "context window",
            "context length",
            "maximum context",
            "max context",
            "too many tokens",
            "token limit",
            "context_length_exceeded",
            "context_window_blocked",
            "input is too long",
            "prompt is too long",
        ]
        .iter()
        .any(|needle| text.contains(needle))
    }
}

impl From<runtime::ToolError> for GatewayError {
    fn from(e: runtime::ToolError) -> Self {
        Self::Tool(e.to_string())
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> axum::response::Response {
        let status = self.http_status();
        let body = serde_json::json!({
            "error": self.to_string(),
            "code": status.as_u16()
        });
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_provider_context_window_errors_without_model_names() {
        assert!(GatewayError::RuntimeExecution(
            "provider returned context_length_exceeded".to_string()
        )
        .is_context_window_exceeded());
        assert!(GatewayError::Runtime(runtime::RuntimeError::new(
            "maximum context length is 128000 tokens"
        ))
        .is_context_window_exceeded());
        assert!(
            !GatewayError::RuntimeExecution("network timeout".to_string())
                .is_context_window_exceeded()
        );
        assert!(GatewayError::RuntimeExecution(
            "all API keys failed: context_window_blocked for a model".to_string()
        )
        .is_context_window_exceeded());
    }
}
