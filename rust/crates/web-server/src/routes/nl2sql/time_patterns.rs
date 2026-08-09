use super::{
    CreateTimePatternRequest, CreateTimePatternResponse, DeleteTimePatternResponse,
    ListTimePatternsResponse, TimePatternRow, UpdateTimePatternRequest, UpdateTimePatternResponse,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use regex::Regex;

const ALLOWED_RESOLVED_TYPES: &[&str] = &[
    "today",
    "yesterday",
    "this_week",
    "this_month",
    "last_month",
    "this_quarter",
    "last_quarter",
    "this_year",
    "ytd",
    "mom",
    "yoy",
    "wow",
    "woww",
    "qoq",
    "custom",
];

const ALLOWED_GRANULARITIES: &[&str] = &["day", "week", "month", "quarter", "year"];

fn validate_pattern_regex(raw: &str) -> Result<String> {
    let pattern = raw.trim();
    if pattern.is_empty() {
        return Err(AppError::ValidationError(
            "pattern_regex cannot be empty".into(),
        ));
    }
    let regex = Regex::new(pattern)
        .map_err(|e| AppError::ValidationError(format!("invalid regex pattern: {e}")))?;
    if regex.is_match("") {
        return Err(AppError::ValidationError(
            "time pattern regex must not match an empty string".to_string(),
        ));
    }
    Ok(pattern.to_string())
}

fn validate_pattern_test_text(pattern: &str, test_text: Option<&str>) -> Result<()> {
    let Some(test_text) = test_text.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    let regex = Regex::new(pattern)
        .map_err(|error| AppError::ValidationError(format!("invalid regex pattern: {error}")))?;
    if !regex.is_match(test_text) {
        return Err(AppError::ValidationError(
            "time pattern regex does not match the supplied test text".to_string(),
        ));
    }
    Ok(())
}

fn normalize_resolved_type(raw: &str) -> Result<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if ALLOWED_RESOLVED_TYPES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(AppError::ValidationError(format!(
            "invalid resolved_type '{raw}', expected one of: {}",
            ALLOWED_RESOLVED_TYPES.join(", ")
        )))
    }
}

fn normalize_granularity(raw: &str) -> Result<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok("day".to_string());
    }
    if ALLOWED_GRANULARITIES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(AppError::ValidationError(format!(
            "invalid granularity '{raw}', expected one of: {}",
            ALLOWED_GRANULARITIES.join(", ")
        )))
    }
}

fn parse_db_id_to_i64(raw: &str) -> i64 {
    raw.parse::<u64>()
        .ok()
        .and_then(|v| i64::try_from(v).ok())
        .or_else(|| raw.parse::<i64>().ok())
        .unwrap_or(i64::MAX)
}

async fn clear_qu_cache_for_tenant(db: &sqlx::SqlitePool, tenant_id: &str) -> Result<()> {
    sqlx::query::<sqlx::Sqlite>("DELETE FROM nl2sql_query_understanding_cache WHERE tenant_id = ?")
        .bind(tenant_id)
        .execute(db)
        .await?;
    Ok(())
}

// GET /nl2sql/time-patterns
pub(crate) async fn list_time_patterns(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ListTimePatternsResponse>> {
    let tenant_id = &claims.tenant_id;
    // Only admins can view time patterns globally; regular users only see 'default'.
    let is_admin = matches!(claims.role.as_str(), "admin" | "superadmin");
    let rows: Vec<(String, String, String, String, String, i32, i32, bool)> = if is_admin {
        sqlx::query_as::<sqlx::Sqlite, _>(
            r#"
            SELECT CAST(id AS TEXT) AS id, pattern_regex, pattern_display, resolved_type, granularity,
                   offset_days, priority, enabled
            FROM nl2sql_time_patterns
            WHERE tenant_id IN ('default', ?)
            ORDER BY tenant_id DESC, priority DESC
            "#,
        )
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?
    } else {
        // Non-admin users can only see system-wide default patterns.
        sqlx::query_as::<sqlx::Sqlite, _>(
            r#"
            SELECT CAST(id AS TEXT) AS id, pattern_regex, pattern_display, resolved_type, granularity,
                   offset_days, priority, enabled
            FROM nl2sql_time_patterns
            WHERE tenant_id = 'default'
            ORDER BY priority DESC
            "#,
        )
        .fetch_all(&state.db)
        .await?
    };

    let patterns: Vec<TimePatternRow> = rows
        .into_iter()
        .map(
            |(
                id,
                pattern_regex,
                pattern_display,
                resolved_type,
                granularity,
                offset_days,
                priority,
                enabled,
            )| {
                TimePatternRow {
                    id: parse_db_id_to_i64(&id),
                    pattern_regex,
                    pattern_display,
                    resolved_type,
                    granularity,
                    offset_days,
                    priority,
                    enabled,
                }
            },
        )
        .collect();

    Ok(Json(ListTimePatternsResponse { patterns }))
}

// POST /nl2sql/time-patterns
pub(crate) async fn create_time_pattern(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateTimePatternRequest>,
) -> Result<Json<CreateTimePatternResponse>> {
    let is_admin = matches!(claims.role.as_str(), "admin" | "superadmin");
    if !is_admin {
        return Err(AppError::Forbidden);
    }
    let tenant_id = &claims.tenant_id;

    let pattern_regex = validate_pattern_regex(&req.pattern_regex)?;
    validate_pattern_test_text(&pattern_regex, req.test_text.as_deref())?;
    let resolved_type = normalize_resolved_type(&req.resolved_type)?;
    let granularity = normalize_granularity(&req.granularity)?;

    let id: u64 = sqlx::query::<sqlx::Sqlite>(
        r#"
        INSERT INTO nl2sql_time_patterns
          (tenant_id, pattern_regex, pattern_display, resolved_type, granularity, offset_days, priority, enabled, created_by)
        VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?)
        "#,
    )
    .bind(tenant_id)
    .bind(&pattern_regex)
    .bind(&req.pattern_display)
    .bind(&resolved_type)
    .bind(&granularity)
    .bind(req.offset_days)
    .bind(req.priority)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?
    .last_insert_rowid()
    .try_into()
    .map_err(|_| AppError::Internal("invalid SQLite rowid".into()))?;
    clear_qu_cache_for_tenant(&state.db, tenant_id).await?;

    Ok(Json(CreateTimePatternResponse { id }))
}

// PATCH /nl2sql/time-patterns/:pattern_id
pub(crate) async fn update_time_pattern(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pattern_id): Path<String>,
    Json(req): Json<UpdateTimePatternRequest>,
) -> Result<Json<UpdateTimePatternResponse>> {
    let is_admin = matches!(claims.role.as_str(), "admin" | "superadmin");
    if !is_admin {
        return Err(AppError::Forbidden);
    }
    let tenant_id = &claims.tenant_id;
    let id = pattern_id
        .parse::<u64>()
        .map_err(|_| AppError::ValidationError("invalid pattern id".into()))?;

    let pattern_regex: Option<String> = req
        .pattern_regex
        .as_deref()
        .map(validate_pattern_regex)
        .transpose()?;
    if let Some(pattern_regex) = pattern_regex.as_deref() {
        validate_pattern_test_text(pattern_regex, req.test_text.as_deref())?;
    } else if req
        .test_text
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
    {
        let current_pattern = sqlx::query_scalar::<_, String>(
            "SELECT pattern_regex FROM nl2sql_time_patterns WHERE id = ? AND tenant_id = ?",
        )
        .bind(crate::sqlite_i64(id))
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("time pattern not found".into()))?;
        validate_pattern_test_text(&current_pattern, req.test_text.as_deref())?;
    }
    let resolved_type: Option<String> = req
        .resolved_type
        .as_deref()
        .map(normalize_resolved_type)
        .transpose()?;
    let granularity: Option<String> = req
        .granularity
        .as_deref()
        .map(normalize_granularity)
        .transpose()?;

    let affected = sqlx::query::<sqlx::Sqlite>(
        r#"
        UPDATE nl2sql_time_patterns SET
          pattern_regex = COALESCE(?, pattern_regex),
          pattern_display = COALESCE(?, pattern_display),
          resolved_type = COALESCE(?, resolved_type),
          granularity = COALESCE(?, granularity),
          offset_days = COALESCE(?, offset_days),
          priority = COALESCE(?, priority),
          enabled = COALESCE(?, enabled)
        WHERE id = ? AND tenant_id = ?
        "#,
    )
    .bind(pattern_regex.as_deref())
    .bind(&req.pattern_display)
    .bind(resolved_type.as_deref())
    .bind(granularity.as_deref())
    .bind(req.offset_days)
    .bind(req.priority)
    .bind(req.enabled)
    .bind(crate::sqlite_i64(id))
    .bind(tenant_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::NotFound("time pattern not found".into()));
    }
    clear_qu_cache_for_tenant(&state.db, tenant_id).await?;

    Ok(Json(UpdateTimePatternResponse { success: true }))
}

// DELETE /nl2sql/time-patterns/:pattern_id
pub(crate) async fn delete_time_pattern(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pattern_id): Path<String>,
) -> Result<Json<DeleteTimePatternResponse>> {
    let is_admin = matches!(claims.role.as_str(), "admin" | "superadmin");
    if !is_admin {
        return Err(AppError::Forbidden);
    }
    let tenant_id = &claims.tenant_id;
    let id = pattern_id
        .parse::<u64>()
        .map_err(|_| AppError::ValidationError("invalid pattern id".into()))?;

    let affected = sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_time_patterns WHERE id = ? AND tenant_id = ?",
    )
    .bind(crate::sqlite_i64(id))
    .bind(tenant_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::NotFound("time pattern not found".into()));
    }
    clear_qu_cache_for_tenant(&state.db, tenant_id).await?;

    Ok(Json(DeleteTimePatternResponse { success: true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_validation_rejects_empty_and_invalid_patterns() {
        assert!(validate_pattern_regex("  ").is_err());
        assert!(validate_pattern_regex("(unclosed").is_err());
    }

    #[test]
    fn regex_validation_accepts_patterns_used_by_time_resolution() {
        let pattern = r"^(?:最近|近)(\d+)天$";
        assert_eq!(
            validate_pattern_regex(pattern).expect("valid time regex"),
            pattern
        );
        assert!(Regex::new(pattern)
            .expect("compiled regex")
            .is_match("最近7天"));
    }

    #[test]
    fn regex_validation_rejects_empty_matches_and_non_matching_examples() {
        assert!(validate_pattern_regex(".*").is_err());
        assert!(validate_pattern_test_text(r"(?:最近|近)(\d+)天", Some("最近7天")).is_ok());
        assert!(validate_pattern_test_text(r"(?:最近|近)(\d+)天", Some("上个月")).is_err());
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Validation Rules Handlers
// ══════════════════════════════════════════════════════════════════════════════

// GET /nl2sql/validation-rules/:datasource_id
