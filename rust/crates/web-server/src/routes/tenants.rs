//! Tenants API — CRUD for tenant management.
//!
//! ## Security
//!
//! - Only admins may list/create/update/delete tenants.
//! - All other routes are tenant-scoped and use `tenant_id` from JWT claims.

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
    },
    Json, Router,
};
use chrono::Datelike;
use serde::{Deserialize, Serialize};

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::tenant_bootstrap::seed_tenant_defaults_with_tx;
use crate::routes::PaginationParams;
use crate::state::AppState;

// ── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TenantInfo {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub plan: String,
    pub max_users: Option<i32>,
    pub max_tokens_monthly: Option<i64>,
    pub is_system: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_count: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct TenantListResponse {
    pub tenants: Vec<TenantInfo>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct TenantUsageResponse {
    pub tenant_id: String,
    pub usage_this_month: i64,
    pub max_tokens_monthly: Option<i64>,
    pub user_count: i32,
    pub max_users: Option<i32>,
    pub usage_percent: f64,
    pub over_limit: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateTenantRequest {
    pub name: String,
    pub slug: String,
    #[serde(default = "default_plan")]
    pub plan: String,
    #[serde(default)]
    pub max_users: Option<i32>,
    #[serde(default)]
    pub max_tokens_monthly: Option<i64>,
}

fn default_plan() -> String {
    "free".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateTenantRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub plan: Option<String>,
    pub max_users: Option<i32>,
    pub max_tokens_monthly: Option<i64>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn row_to_tenant(row: TenantRow) -> TenantInfo {
    TenantInfo {
        id: row.id,
        name: row.name,
        slug: row.slug,
        plan: row.plan,
        max_users: row.max_users,
        max_tokens_monthly: row.max_tokens_monthly,
        is_system: row.is_system,
        user_count: None,
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct TenantRow {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub plan: String,
    pub max_users: Option<i32>,
    pub max_tokens_monthly: Option<i64>,
    pub is_system: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<TenantRow> for TenantInfo {
    fn from(row: TenantRow) -> Self {
        row_to_tenant(row)
    }
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// GET /api/v1/tenants — admin: list all tenants (paginated).
async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<TenantListResponse>> {
    if claims.role != "admin" {
        return Err(AppError::Forbidden);
    }

    let offset = pagination.offset();
    let limit = pagination.limit();

    let rows = sqlx::query_as::<_, TenantRow>(
        "SELECT id, name, slug, plan, max_users, max_tokens_monthly, is_system, created_at, updated_at FROM tenants ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tenants")
        .fetch_one(&state.db)
        .await?;

    let tenants: Vec<TenantInfo> = rows.into_iter().map(TenantInfo::from).collect();
    let total = usize::try_from(total.0).unwrap_or(0);

    Ok(Json(TenantListResponse { tenants, total }))
}

/// GET /api/v1/tenants/{id} — get a single tenant by id.
async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<TenantInfo>> {
    // Users can only view their own tenant; admins can view any.
    if claims.role != "admin" && claims.tenant_id != id {
        return Err(AppError::Forbidden);
    }

    let row = sqlx::query_as::<_, TenantRow>(
        "SELECT id, name, slug, plan, max_users, max_tokens_monthly, is_system, created_at, updated_at FROM tenants WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) => Ok(Json(TenantInfo::from(r))),
        None => Err(AppError::NotFound(format!("tenant '{id}' not found"))),
    }
}

/// POST /api/v1/tenants — admin: create a new tenant.
async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<Json<TenantInfo>> {
    if claims.role != "admin" {
        return Err(AppError::Forbidden);
    }

    if req.name.trim().is_empty() {
        return Err(AppError::ValidationError(
            "tenant name cannot be empty".into(),
        ));
    }
    if req.slug.trim().is_empty() {
        return Err(AppError::ValidationError(
            "tenant slug cannot be empty".into(),
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = state.db.begin().await?;

    sqlx::query("INSERT INTO tenants (id, name, slug, plan, max_users, max_tokens_monthly) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&req.name)
        .bind(&req.slug)
        .bind(&req.plan)
        .bind(req.max_users)
        .bind(req.max_tokens_monthly)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
                AppError::Conflict("tenant slug already exists".into())
            } else {
                AppError::Database(e)
            }
        })?;

    seed_tenant_defaults_with_tx(&mut tx, &id, Some(&claims.sub)).await?;

    let row = sqlx::query_as::<_, TenantRow>(
        "SELECT id, name, slug, plan, max_users, max_tokens_monthly, is_system, created_at, updated_at FROM tenants WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(TenantInfo::from(row)))
}

/// PATCH /api/v1/tenants/{id} — admin: update tenant details.
async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<UpdateTenantRequest>,
) -> Result<Json<TenantInfo>> {
    if claims.role != "admin" {
        return Err(AppError::Forbidden);
    }

    if let Some(ref slug) = req.slug {
        let exists: (i64,) = sqlx::query_as(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM tenants WHERE slug = ? AND id != ?",
        )
        .bind(slug)
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
        if exists.0 > 0 {
            return Err(AppError::ValidationError(format!(
                "slug '{slug}' is already taken"
            )));
        }
    }

    let mut updates: Vec<String> = Vec::new();
    if req.name.is_some() {
        updates.push("name = ?".to_string());
    }
    if req.slug.is_some() {
        updates.push("slug = ?".to_string());
    }
    if req.plan.is_some() {
        updates.push("plan = ?".to_string());
    }
    if req.max_users.is_some() {
        updates.push("max_users = ?".to_string());
    }
    if req.max_tokens_monthly.is_some() {
        updates.push("max_tokens_monthly = ?".to_string());
    }

    if !updates.is_empty() {
        let query = format!("UPDATE tenants SET {} WHERE id = ?", updates.join(", "));

        let mut q = sqlx::query(&query);
        if let Some(ref name) = req.name {
            q = q.bind(name);
        }
        if let Some(ref slug) = req.slug {
            q = q.bind(slug);
        }
        if let Some(ref plan) = req.plan {
            q = q.bind(plan);
        }
        if let Some(max_users) = req.max_users {
            q = q.bind(max_users);
        }
        if let Some(max_tokens) = req.max_tokens_monthly {
            q = q.bind(max_tokens);
        }
        q = q.bind(&id);

        q.execute(&state.db).await?;
    }

    let row = sqlx::query_as::<_, TenantRow>(
        "SELECT id, name, slug, plan, max_users, max_tokens_monthly, is_system, created_at, updated_at FROM tenants WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) => Ok(Json(TenantInfo::from(r))),
        None => Err(AppError::NotFound(format!("tenant '{id}' not found"))),
    }
}

/// DELETE /api/v1/tenants/{id} — admin: delete a tenant.
async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>> {
    if claims.role != "admin" {
        return Err(AppError::Forbidden);
    }

    // Prevent deleting the default tenant
    if id == "00000000-0000-0000-0000-000000000001" {
        return Err(AppError::ValidationError(
            "cannot delete the default tenant".into(),
        ));
    }

    let result = sqlx::query("DELETE FROM tenants WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("tenant '{id}' not found")));
    }

    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

/// GET /api/v1/tenants/{id}/usage — get quota usage for a tenant.
async fn usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<TenantUsageResponse>> {
    if claims.role != "admin" && claims.tenant_id != id {
        return Err(AppError::Forbidden);
    }

    let tenant_row = sqlx::query_as::<_, (Option<i64>, Option<i32>)>(
        "SELECT max_tokens_monthly, max_users FROM tenants WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("tenant '{id}' not found")))?;

    let (max_tokens_monthly, max_users) = tenant_row;

    let start_of_month = chrono::Utc::now()
        .date_naive()
        .with_day(1)
        .unwrap_or(chrono::Utc::now().date_naive())
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| chrono::Utc::now().naive_utc())
        .and_utc();

    let usage_row: (i64,) = sqlx::query_as(
        "SELECT COALESCE(CAST(SUM(input_tokens + output_tokens) AS INTEGER), 0) \
         FROM token_usage WHERE tenant_id = ? AND created_at >= ?",
    )
    .bind(&id)
    .bind(start_of_month)
    .fetch_one(&state.db)
    .await?;

    let usage_this_month = usage_row.0;

    let user_count_row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE tenant_id = ?")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    let user_count = i32::try_from(user_count_row.0).unwrap_or(i32::MAX);

    #[allow(clippy::cast_precision_loss)]
    let usage_percent = if let Some(max) = max_tokens_monthly {
        if max > 0 {
            usage_this_month as f64 / max as f64 * 100.0
        } else {
            0.0
        }
    } else {
        0.0
    };

    Ok(Json(TenantUsageResponse {
        tenant_id: id,
        usage_this_month,
        max_tokens_monthly,
        user_count,
        max_users,
        usage_percent,
        over_limit: max_tokens_monthly.is_some_and(|max| usage_this_month >= max),
    }))
}

async fn admin_auth(
    State(state): State<AppState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    match crate::auth::verify_token(&state, token).await {
        Ok(claims) => {
            if claims.role != "admin" {
                return StatusCode::FORBIDDEN.into_response();
            }
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

// ── Router ──────────────────────────────────────────────────────────────────

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/", routing_post(create))
        .route("/{id}", routing_get(get))
        .route("/{id}", routing_patch(update))
        .route("/{id}", routing_delete(delete))
        .route("/{id}/usage", routing_get(usage))
        .layer(axum::middleware::from_fn_with_state(state, admin_auth))
}
