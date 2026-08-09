//! Users API — CRUD for user management within a tenant.
//!
//! ## Security
//! - Admins can manage all users in their tenant.
//! - Users can only view their own profile (PATCH me).
//! - The `invite_token` is used for first-time password setup via invite link.

use axum::{
    extract::{Extension, Path, Query, State},
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
    },
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::auth::Claims;
use crate::email;
use crate::error::{AppError, Result};
use crate::routes::PaginationParams;
use crate::state::AppState;

// ── DTOs ─────────────────────────────────────────────────────────────────────

const VALID_MENU_PERMISSIONS: &[&str] = &[
    "dashboard:read",
    "super_assistant:read",
    "workspace:read",
    "chat:read",
    "adversarial:read",
    "watchdog:read",
    "tasks:read",
    "rd_studio:read",
    "rd_specs:read",
    "rd_quality:read",
    "rd_agents:read",
    "operations_assistant:read",
    "operations_tasks:read",
    "operations_materials:read",
    "operations_governance:read",
    "operations_governance:write",
    "projects:read",
    "pipeline:read",
    "nl2sql_explore:read",
    "nl2sql_management:read",
    "nl2sql_analytics:read",
    "datasources:read",
    "mcp:read",
    "skills:read",
    "search_providers:read",
    "hooks:read",
    "bot_agents:read",
    "apikeys:read",
    "users:read",
    "config:read",
];

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub tenant_id: String,
    pub is_active: bool,
    pub permission_mode: String,
    pub menu_permissions: Vec<String>,
    pub menu_permissions_inherited: bool,
    pub created_at: String,
    pub created_by: Option<String>,
    pub last_login_at: Option<String>,
    pub password_changed_at: Option<String>,
}

impl
    From<(
        String,
        String,
        String,
        String,
        String,
        bool,
        chrono::DateTime<chrono::Utc>,
        Option<String>,
        String,
        Option<String>,
        bool,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> for UserInfo
{
    fn from(
        (
            id,
            email,
            name,
            role,
            tenant_id,
            is_active,
            created_at,
            created_by,
            permission_mode,
            menu_permissions_json,
            menu_permissions_inherited,
            last_login_at,
            password_changed_at,
        ): (
            String,
            String,
            String,
            String,
            String,
            bool,
            chrono::DateTime<chrono::Utc>,
            Option<String>,
            String,
            Option<String>,
            bool,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    ) -> Self {
        Self {
            id,
            email,
            name,
            role,
            tenant_id,
            is_active,
            permission_mode,
            menu_permissions: parse_permissions_json(menu_permissions_json),
            menu_permissions_inherited,
            created_at: created_at.to_rfc3339(),
            created_by,
            last_login_at: last_login_at.map(|dt| dt.to_rfc3339()),
            password_changed_at: password_changed_at.map(|dt| dt.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UserListResponse {
    pub users: Vec<UserInfo>,
    pub total: usize,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub email: String,
    pub name: String,
    pub role: Option<String>,
    pub menu_permissions: Option<Vec<String>>,
    #[allow(dead_code)]
    pub invite: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<String>,
    pub is_active: Option<bool>,
    pub permission_mode: Option<String>,
    pub menu_permissions_inherited: Option<bool>,
    pub menu_permissions: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub password: String,
    pub invite_token: String,
}

#[derive(Debug, Deserialize)]
pub struct SendResetEmailRequest {
    pub user_id: String,
}

#[derive(Debug, Serialize)]
pub struct InviteUserResponse {
    pub user_id: String,
    pub invite_token: String,
    pub invite_url: String,
    pub email_configured: bool,
    pub email_sent: bool,
    pub email_error: Option<String>,
}

pub(crate) fn parse_permissions_json(raw: Option<String>) -> Vec<String> {
    raw.as_deref()
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .map(|values| {
            values
                .into_iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn normalize_menu_permissions(menu_permissions: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for permission in menu_permissions {
        let permission = permission.trim();
        if permission.is_empty() {
            continue;
        }
        let expanded_permissions: &[&str] = match permission {
            // The legacy AI Chat / Ops Assistant / Data Attribution entries now
            // route through the unified Super Assistant menu. Preserve access for
            // existing custom-permission users when old menu permissions are
            // saved back from older clients or imported tenant data.
            "chat:read" => &["super_assistant:read", "chat:read"],
            "operations_assistant:read" => &["super_assistant:read", "operations_assistant:read"],
            "watchdog:read" => &["tasks:read", "watchdog:read"],
            // Backward compatibility for tenants that stored the old all-in-one
            // R&D menu permission before Code Dev / Repos / Tasks were split.
            "rd:read" => &[
                "rd_studio:read",
                "rd_specs:read",
                "projects:read",
                "pipeline:read",
                "rd_quality:read",
            ],
            "agent:read" => &["rd_studio:read", "rd_specs:read", "rd_quality:read"],
            "operations:read" => &[
                "super_assistant:read",
                "operations_assistant:read",
                "operations_tasks:read",
                "operations_materials:read",
                "operations_governance:read",
            ],
            "nl2sql:read" => &[
                "nl2sql_explore:read",
                "nl2sql_management:read",
                "nl2sql_analytics:read",
            ],
            "rd:admin" => &["rd_agents:read"],
            _ => std::slice::from_ref(&permission),
        };
        for permission in expanded_permissions {
            if !VALID_MENU_PERMISSIONS.contains(permission) {
                return Err(AppError::ValidationError(format!(
                    "invalid menu permission: {permission}"
                )));
            }
            if !normalized.iter().any(|value| value == permission) {
                normalized.push((*permission).to_string());
            }
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_assistant_menu_permission_is_valid() {
        let normalized = normalize_menu_permissions(&["super_assistant:read".to_string()])
            .expect("super assistant permission should be accepted");

        assert_eq!(normalized, vec!["super_assistant:read"]);
    }

    #[test]
    fn workspace_menu_permission_is_independently_valid() {
        let normalized = normalize_menu_permissions(&["workspace:read".to_string()])
            .expect("workspace permission should be accepted");

        assert_eq!(normalized, vec!["workspace:read"]);
    }

    #[test]
    fn legacy_watchdog_permission_grants_task_command_center() {
        let normalized = normalize_menu_permissions(&["watchdog:read".to_string()])
            .expect("legacy watchdog permission should normalize");

        assert_eq!(
            normalized,
            vec!["tasks:read".to_string(), "watchdog:read".to_string()]
        );
    }

    #[test]
    fn legacy_chat_and_ops_permissions_grant_unified_super_assistant() {
        let normalized = normalize_menu_permissions(&[
            "chat:read".to_string(),
            "operations_assistant:read".to_string(),
        ])
        .expect("legacy menu permissions should normalize");

        assert_eq!(
            normalized,
            vec![
                "super_assistant:read".to_string(),
                "chat:read".to_string(),
                "operations_assistant:read".to_string(),
            ]
        );
    }
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// GET /api/v1/users — admin: list all users in the tenant.
async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<UserListResponse>> {
    let is_admin = claims.role == "admin";
    let is_superadmin = claims.role == "superadmin";

    if !is_admin && !is_superadmin {
        return Err(AppError::Forbidden);
    }

    let offset = pagination.offset();
    let limit = pagination.limit();

    type UserRow = (
        String,
        String,
        String,
        String,
        String,
        bool,
        chrono::DateTime<chrono::Utc>,
        Option<String>,
        String,
        Option<String>,
        bool,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    );

    let rows: Vec<UserRow> = if is_superadmin {
        sqlx::query_as(
            "SELECT id, email, name, role, COALESCE(tenant_id, '') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
             FROM users ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, email, name, role, COALESCE(tenant_id, '') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
             FROM users WHERE tenant_id = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(&claims.tenant_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };

    let total: (i64,) = if is_superadmin {
        sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(&state.db)
            .await?
    } else {
        sqlx::query_as("SELECT COUNT(*) FROM users WHERE tenant_id = ?")
            .bind(&claims.tenant_id)
            .fetch_one(&state.db)
            .await?
    };

    let users: Vec<UserInfo> = rows.into_iter().map(UserInfo::from).collect();
    let total = usize::try_from(total.0).unwrap_or(0);

    Ok(Json(UserListResponse { users, total }))
}

/// POST /api/v1/users — admin: invite a new user to the tenant.
async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateUserRequest>,
) -> Result<Json<InviteUserResponse>> {
    if claims.role != "admin" && claims.role != "superadmin" {
        return Err(AppError::Forbidden);
    }

    let normalized_email = req.email.trim().to_ascii_lowercase();
    if normalized_email.is_empty() || !normalized_email.contains('@') {
        return Err(AppError::ValidationError("invalid email address".into()));
    }

    let role = req.role.as_deref().unwrap_or("developer");
    let valid_roles = ["developer", "viewer", "admin"];
    if !valid_roles.contains(&role) {
        return Err(AppError::ValidationError(format!(
            "role must be one of: {}",
            valid_roles.join(", ")
        )));
    }

    // Check if email already exists
    let existing: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM users WHERE LOWER(email) = LOWER(?)")
            .bind(&normalized_email)
            .fetch_optional(&state.db)
            .await?;
    if existing.is_some() {
        return Err(AppError::Conflict("email already registered".into()));
    }

    // Check tenant user limit
    let user_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE tenant_id = ?")
        .bind(&claims.tenant_id)
        .fetch_one(&state.db)
        .await?;
    let max_users: Option<(i64,)> = sqlx::query_as("SELECT max_users FROM tenants WHERE id = ?")
        .bind(&claims.tenant_id)
        .fetch_optional(&state.db)
        .await?;
    if let Some((max,)) = max_users {
        if user_count.0 >= max {
            return Err(AppError::ValidationError(
                "tenant user limit reached".into(),
            ));
        }
    }

    let user_id = uuid::Uuid::new_v4().to_string();
    let invite_token = uuid::Uuid::new_v4().to_string();
    let menu_permissions_json = match req.menu_permissions.as_ref() {
        Some(menu_permissions) => Some(
            serde_json::to_string(&normalize_menu_permissions(menu_permissions)?).map_err(|e| {
                AppError::Internal(format!("failed to encode menu permissions: {e}"))
            })?,
        ),
        None => None,
    };

    // If invite=true: create user with a random password hash (must reset)
    // If invite=false: create user with a default "changeme" password (discouraged)
    let password_hash = bcrypt::hash(&uuid::Uuid::new_v4().to_string()[..8], bcrypt::DEFAULT_COST)?;

    sqlx::query(
        "INSERT INTO users (id, email, name, password_hash, role, tenant_id, created_by, invite_token, is_active, menu_permissions_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?)",
    )
    .bind(&user_id)
    .bind(&normalized_email)
    .bind(&req.name)
    .bind(&password_hash)
    .bind(role)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&invite_token)
    .bind(menu_permissions_json)
    .execute(&state.db)
    .await
    .map_err(|e| {
        if matches!(e, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
            AppError::Conflict("email already exists".into())
        } else {
            AppError::Database(e)
        }
    })?;

    let base_url = &state.base_url;
    let invite_url = format!("{base_url}/invite?token={invite_token}");

    // Get tenant name for the email
    let tenant_name: String =
        sqlx::query_scalar("SELECT COALESCE(name, 'AOS Workspace') FROM tenants WHERE id = ?")
            .bind(&claims.tenant_id)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_else(|| "AOS Workspace".to_string());

    let email_delivery =
        email::send_invite_email(&normalized_email, &invite_url, &tenant_name).await;

    Ok(Json(InviteUserResponse {
        user_id,
        invite_token,
        invite_url,
        email_configured: email_delivery.configured,
        email_sent: email_delivery.sent,
        email_error: email_delivery.error,
    }))
}

/// GET /api/v1/users/me — current user profile.
async fn me(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<UserInfo>> {
    let row = sqlx::query_as::<_, (String, String, String, String, String, bool, chrono::DateTime<chrono::Utc>, Option<String>, String, Option<String>, bool, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, email, name, role, COALESCE(tenant_id, '') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
         FROM users WHERE id = ?",
    )
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) => Ok(Json(UserInfo::from(r))),
        None => Err(AppError::NotFound("user not found".into())),
    }
}

/// GET /api/v1/users/:id — admin: get user by id.
async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(user_id): Path<String>,
) -> Result<Json<UserInfo>> {
    let is_superadmin = claims.role == "superadmin";
    let is_admin = claims.role == "admin" || is_superadmin;
    let is_self = user_id == claims.sub;

    let row = sqlx::query_as::<_, (String, String, String, String, String, bool, chrono::DateTime<chrono::Utc>, Option<String>, String, Option<String>, bool, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, email, name, role, COALESCE(tenant_id, '') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
         FROM users WHERE id = ?",
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) if !is_superadmin && r.4 != claims.tenant_id => Err(AppError::Forbidden),
        Some(_) if !is_admin && !is_self => Err(AppError::Forbidden),
        Some(r) => Ok(Json(UserInfo::from(r))),
        None => Err(AppError::NotFound("user not found".into())),
    }
}

/// PATCH /api/v1/users/:id — admin: update user; or user updates themselves.
async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(user_id): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<UserInfo>> {
    let is_superadmin = claims.role == "superadmin";
    let is_admin = claims.role == "admin" || is_superadmin;
    let is_self = user_id == claims.sub;

    if !is_admin && !is_self {
        return Err(AppError::Forbidden);
    }

    if is_admin && !is_superadmin {
        let target_tenant: Option<String> =
            sqlx::query_scalar("SELECT tenant_id FROM users WHERE id = ?")
                .bind(&user_id)
                .fetch_optional(&state.db)
                .await?;
        match target_tenant {
            Some(tenant_id) if tenant_id == claims.tenant_id => {}
            Some(_) => return Err(AppError::Forbidden),
            None => return Err(AppError::NotFound("user not found".into())),
        }
    }

    // Non-admins can only update their own name
    if !is_admin && is_self {
        if req.role.is_some()
            || req.is_active.is_some()
            || req.permission_mode.is_some()
            || req.menu_permissions.is_some()
            || req.menu_permissions_inherited.is_some()
        {
            return Err(AppError::Forbidden);
        }
        if let Some(name) = req.name {
            sqlx::query("UPDATE users SET name = ? WHERE id = ?")
                .bind(&name)
                .bind(&user_id)
                .execute(&state.db)
                .await?;
        }
        let row = sqlx::query_as::<_, (String, String, String, String, String, bool, chrono::DateTime<chrono::Utc>, Option<String>, String, Option<String>, bool, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
            "SELECT id, email, name, role, COALESCE(tenant_id, '') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
             FROM users WHERE id = ?",
        )
        .bind(&user_id)
        .fetch_one(&state.db)
        .await?;
        return Ok(Json(UserInfo::from(row)));
    }

    let mut updates: Vec<&str> = Vec::new();
    let mut bindings: Vec<String> = Vec::new();

    if let Some(ref name) = req.name {
        updates.push("name = ?");
        bindings.push(name.clone());
    }
    if let Some(ref role) = req.role {
        let valid_roles = ["developer", "viewer", "admin"];
        if !valid_roles.contains(&role.as_str()) {
            return Err(AppError::ValidationError("invalid role".into()));
        }
        updates.push("role = ?");
        bindings.push(role.clone());
    }
    if let Some(is_active) = req.is_active {
        updates.push("is_active = ?");
        bindings.push(if is_active { "1" } else { "0" }.to_string());
    }
    if let Some(ref pm) = req.permission_mode {
        updates.push("permission_mode = ?");
        bindings.push(pm.clone());
    }
    if let Some(inherited) = req.menu_permissions_inherited {
        if inherited {
            updates.push("menu_permissions_json = NULL");
        } else if let Some(ref menu_permissions) = req.menu_permissions {
            let normalized = normalize_menu_permissions(menu_permissions)?;
            updates.push("menu_permissions_json = ?");
            bindings.push(serde_json::to_string(&normalized).map_err(|e| {
                AppError::Internal(format!("failed to encode menu permissions: {e}"))
            })?);
        } else {
            return Err(AppError::ValidationError(
                "menu permissions are required when custom mode is disabled".into(),
            ));
        }
    } else if let Some(ref menu_permissions) = req.menu_permissions {
        let normalized = normalize_menu_permissions(menu_permissions)?;
        updates.push("menu_permissions_json = ?");
        bindings.push(
            serde_json::to_string(&normalized).map_err(|e| {
                AppError::Internal(format!("failed to encode menu permissions: {e}"))
            })?,
        );
    }

    if !updates.is_empty() {
        let query = format!("UPDATE users SET {} WHERE id = ?", updates.join(", "));
        let mut q = sqlx::query(&query);
        for b in &bindings {
            q = q.bind(b);
        }
        q = q.bind(&user_id);
        q.execute(&state.db).await?;
    }

    let row = sqlx::query_as::<_, (String, String, String, String, String, bool, chrono::DateTime<chrono::Utc>, Option<String>, String, Option<String>, bool, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, email, name, role, COALESCE(tenant_id, '') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
         FROM users WHERE id = ?",
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(UserInfo::from(row)))
}

/// DELETE /api/v1/users/:id — admin: deactivate user (soft delete).
async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(user_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let is_superadmin = claims.role == "superadmin";
    if claims.role != "admin" && !is_superadmin {
        return Err(AppError::Forbidden);
    }
    if user_id == claims.sub {
        return Err(AppError::ValidationError(
            "cannot deactivate your own account".into(),
        ));
    }

    if !is_superadmin {
        let target_tenant: Option<String> =
            sqlx::query_scalar("SELECT tenant_id FROM users WHERE id = ?")
                .bind(&user_id)
                .fetch_optional(&state.db)
                .await?;
        match target_tenant {
            Some(tenant_id) if tenant_id == claims.tenant_id => {}
            Some(_) => return Err(AppError::Forbidden),
            None => return Err(AppError::NotFound("user not found".into())),
        }
    }

    sqlx::query("UPDATE users SET is_active = 0 WHERE id = ?")
        .bind(&user_id)
        .execute(&state.db)
        .await?;

    Ok(Json(
        serde_json::json!({ "deactivated": true, "id": user_id }),
    ))
}

/// POST /api/v1/users/invite/accept — accept invite: set password.
async fn accept_invite(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> Result<Json<serde_json::Value>> {
    if req.password.len() < 8 {
        return Err(AppError::ValidationError(
            "password must be at least 8 characters".into(),
        ));
    }

    // Find user by invite token
    let user_id: Option<(String,)> =
        sqlx::query_as("SELECT id FROM users WHERE invite_token = ? AND is_active = 1")
            .bind(&req.invite_token)
            .fetch_optional(&state.db)
            .await?;

    let Some((user_id,)) = user_id else {
        return Err(AppError::ValidationError(
            "invalid or expired invite token".into(),
        ));
    };

    // Hash the new password and update the user, clearing the invite token
    let password_hash = bcrypt::hash(&req.password, bcrypt::DEFAULT_COST)?;
    sqlx::query(
        "UPDATE users
         SET password_hash = ?, invite_token = NULL, password_changed_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(&password_hash)
    .bind(&user_id)
    .execute(&state.db)
    .await?;

    Ok(Json(
        serde_json::json!({ "success": true, "message": "password set successfully" }),
    ))
}

/// POST /api/v1/users/send-reset-email — admin: send a password-reset link to a user.
async fn send_reset_email(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SendResetEmailRequest>,
) -> Result<Json<serde_json::Value>> {
    if claims.role != "admin" && claims.role != "superadmin" {
        return Err(AppError::Forbidden);
    }

    let user: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT email, name, tenant_id FROM users WHERE id = ? AND is_active = 1")
            .bind(&req.user_id)
            .fetch_optional(&state.db)
            .await?;

    let Some((email, name, tenant_id)) = user else {
        return Err(AppError::NotFound("user not found or inactive".into()));
    };

    if tenant_id.as_ref() != Some(&claims.tenant_id) && claims.role != "superadmin" {
        return Err(AppError::Forbidden);
    }

    let reset_token = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE users SET invite_token = ? WHERE id = ?")
        .bind(&reset_token)
        .bind(&req.user_id)
        .execute(&state.db)
        .await?;

    let reset_url = format!("{}/invite?token={}", state.base_url, reset_token);
    let email_delivery = email::send_invite_email(&email, &reset_url, &name).await;

    Ok(Json(serde_json::json!({
        "success": true,
        "reset_url": reset_url,
        "email_configured": email_delivery.configured,
        "email_sent": email_delivery.sent,
        "email_error": email_delivery.error,
    })))
}

// ── Router ──────────────────────────────────────────────────────────────────

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/", routing_post(create))
        .route("/me", routing_get(me))
        .route("/send-reset-email", routing_post(send_reset_email))
        .route("/{id}", routing_get(get))
        .route("/{id}", routing_patch(update))
        .route("/{id}", routing_delete(delete))
        .route("/invite/accept", routing_post(accept_invite))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}
