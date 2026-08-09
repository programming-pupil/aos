//! Small shared helpers for RD route modules.

use super::*;

pub(super) fn nonnegative_i64_to_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(0)
}

pub(super) fn metric_count(metric_totals: &HashMap<String, f64>, metric_name: &str) -> u64 {
    metric_totals
        .get(metric_name)
        .copied()
        .unwrap_or_default()
        .max(0.0)
        .round() as u64
}

pub(super) fn ratio(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

pub(super) fn require_non_empty(value: &str, field: &str) -> Result<String, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::ValidationError(format!("{field} is required")));
    }
    Ok(trimmed.to_string())
}

pub(super) fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

const RD_TASK_CANCELLED_ERROR: &str = "rd_task_cancelled";

pub(super) fn rd_task_cancelled_error() -> AppError {
    AppError::ValidationError(RD_TASK_CANCELLED_ERROR.to_string())
}

pub(super) fn rd_error_is_cancelled(error: &AppError) -> bool {
    matches!(error, AppError::ValidationError(message) if message == RD_TASK_CANCELLED_ERROR)
}

pub(super) fn rd_task_error_detail_json(error: &AppError) -> Value {
    if rd_error_is_cancelled(error) {
        return json!({
            "errorKind": "cancelled",
            "errorMessage": "RD task was cancelled",
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        });
    }
    match error {
        AppError::Database(db_error) => json!({
            "errorKind": "database",
            "errorMessage": "database error",
            "databaseError": truncate_text(&db_error.to_string(), 4_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Internal(message) => json!({
            "errorKind": "internal",
            "errorMessage": truncate_text(message, 4_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::ValidationError(message) => json!({
            "errorKind": "validation",
            "errorMessage": truncate_text(message, 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::NotFound(message) => json!({
            "errorKind": "not_found",
            "errorMessage": truncate_text(message, 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Conflict(message) => json!({
            "errorKind": "conflict",
            "errorMessage": truncate_text(message, 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::TooManyRequests(message) => json!({
            "errorKind": "rate_limit",
            "errorMessage": truncate_text(message, 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::PayloadTooLarge(message) => json!({
            "errorKind": "payload_too_large",
            "errorMessage": truncate_text(message, 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Unimplemented(message) => json!({
            "errorKind": "unimplemented",
            "errorMessage": truncate_text(message, 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::ExplainCacheExpired(message) => json!({
            "errorKind": "cache_expired",
            "errorMessage": truncate_text(message, 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Unauthorized => json!({
            "errorKind": "unauthorized",
            "errorMessage": "authentication failed",
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Forbidden => json!({
            "errorKind": "forbidden",
            "errorMessage": "access denied",
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Json(json_error) => json!({
            "errorKind": "json",
            "errorMessage": truncate_text(&json_error.to_string(), 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Io(io_error) => json!({
            "errorKind": "io",
            "errorMessage": truncate_text(&io_error.to_string(), 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Jwt(jwt_error) => json!({
            "errorKind": "jwt",
            "errorMessage": truncate_text(&jwt_error.to_string(), 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
        AppError::Bcrypt(bcrypt_error) => json!({
            "errorKind": "bcrypt",
            "errorMessage": truncate_text(&bcrypt_error.to_string(), 2_000),
            "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
        }),
    }
}
