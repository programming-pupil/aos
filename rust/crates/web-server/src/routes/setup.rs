//! Setup API — initial system provisioning (first-boot only).
//!
//! This module handles the one-time setup flow:
//! 1. POST /setup/check  — check whether the system has been initialized.
//! 2. POST /api/v1/setup  — create the first admin user and the default tenant.
//!
//! These endpoints have NO authentication (they are the bootstrap path).

use axum::{
    extract::State,
    routing::{get as routing_get, post as routing_post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::{AppError, Result};
use crate::routes::tenant_bootstrap::seed_tenant_defaults_with_tx;
use crate::state::AppState;

// ── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SetupStatusResponse {
    pub initialized: bool,
    pub tenant_count: usize,
    pub user_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub tenant_name: String,
    pub tenant_slug: String,
    pub admin_email: String,
    pub admin_name: String,
    pub admin_password: String,
}

#[derive(Debug, Serialize)]
pub struct SetupResponse {
    pub tenant_id: String,
    pub admin_user_id: String,
    pub token: String,
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// GET /api/v1/setup/check — check if the system has been initialized.
async fn check(State(state): State<AppState>) -> Result<Json<SetupStatusResponse>> {
    let status = load_setup_status(&state.db).await?;
    if status.initialized {
        state.mark_setup_initialized();
    }
    Ok(Json(status))
}

/// POST /api/v1/setup — initialize the system with the first admin user.
async fn setup(
    State(state): State<AppState>,
    Json(req): Json<SetupRequest>,
) -> Result<Json<SetupResponse>> {
    // Validate input
    if req.tenant_name.trim().is_empty() {
        return Err(AppError::ValidationError("tenant name is required".into()));
    }
    if req.tenant_slug.trim().is_empty()
        || !req
            .tenant_slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
    {
        return Err(AppError::ValidationError(
            "tenant slug must be lowercase alphanumeric (a-z, 0-9, -)".into(),
        ));
    }
    if !req.admin_email.contains('@') {
        return Err(AppError::ValidationError("invalid admin email".into()));
    }
    if req.admin_password.len() < 8 {
        return Err(AppError::ValidationError(
            "admin password must be at least 8 characters".into(),
        ));
    }

    let mut tx = state.db.begin().await?;
    // Make the first statement a write so SQLite acquires the single-writer
    // lock before checking initialization. A concurrent setup waits, then sees
    // the committed tenant and returns Conflict.
    sqlx::query("UPDATE aos_setup_lock SET lock_id = lock_id WHERE lock_id = 1")
        .execute(&mut *tx)
        .await?;
    let existing: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM tenants LIMIT 1")
        .fetch_optional(&mut *tx)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict("system already initialized".into()));
    }

    // Create the default system tenant
    let tenant_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO tenants (id, name, slug, plan, is_system) VALUES (?, ?, ?, 'free', 1)",
    )
    .bind(&tenant_id)
    .bind(&req.tenant_name)
    .bind(&req.tenant_slug)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if matches!(e, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
            AppError::Conflict("tenant slug already exists".into())
        } else {
            AppError::Database(e)
        }
    })?;

    // Create the first admin user
    let admin_id = uuid::Uuid::new_v4().to_string();
    let password_hash = bcrypt::hash(&req.admin_password, bcrypt::DEFAULT_COST)?;

    sqlx::query(
        "INSERT INTO users (id, email, name, password_hash, role, tenant_id, is_active, permission_mode, menu_permissions_json) \
         VALUES (?, ?, ?, ?, 'admin', ?, 1, 'danger_full_access', NULL)",
    )
    .bind(&admin_id)
    .bind(&req.admin_email)
    .bind(&req.admin_name)
    .bind(&password_hash)
    .bind(&tenant_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if matches!(e, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
            AppError::Conflict("admin email already exists".into())
        } else {
            AppError::Database(e)
        }
    })?;

    seed_tenant_defaults_with_tx(&mut tx, &tenant_id, Some(&admin_id)).await?;

    tx.commit().await?;
    state.mark_setup_initialized();

    // Issue a JWT token for the newly created admin
    let token =
        crate::auth::create_token(&state, &admin_id, &req.admin_email, "admin", &tenant_id).await?;

    Ok(Json(SetupResponse {
        tenant_id,
        admin_user_id: admin_id,
        token,
    }))
}

// ── Router ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/check", routing_get(check))
        .route("/", routing_post(setup))
}

pub(crate) async fn load_setup_status(db: &SqlitePool) -> Result<SetupStatusResponse> {
    let tenant_exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM tenants LIMIT 1")
        .fetch_optional(db)
        .await?;
    let user_exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM users LIMIT 1")
        .fetch_optional(db)
        .await?;

    Ok(SetupStatusResponse {
        initialized: tenant_exists.is_some() && user_exists.is_some(),
        tenant_count: usize::from(tenant_exists.is_some()),
        user_count: usize::from(user_exists.is_some()),
    })
}

pub(crate) async fn is_system_initialized(db: &SqlitePool) -> Result<bool> {
    Ok(load_setup_status(db).await?.initialized)
}
