//! Authentication — JWT-based auth for the web server.

use axum::{
    extract::{Extension, State},
    http::HeaderMap,
    Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration as StdDuration, Instant};

use crate::error::{AppError, Result};
use crate::routes::tenant_bootstrap::seed_tenant_defaults_with_tx;
use crate::routes::users::{parse_permissions_json, UserInfo as FullUserInfo};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub role: String,
    /// Tenant ID — loaded from the users table at token verification time.
    #[serde(default)]
    pub tenant_id: String,
    pub exp: i64,
    pub iat: i64,
}

impl Claims {
    pub fn new(user_id: &str, email: &str, role: &str, tenant_id: &str) -> Self {
        let now = Utc::now();
        Self {
            sub: user_id.to_string(),
            email: email.to_string(),
            role: role.to_string(),
            tenant_id: tenant_id.to_string(),
            iat: now.timestamp(),
            exp: (now + Duration::hours(24)).timestamp(),
        }
    }

    /// Tenant administrators may inspect tenant-wide operational metrics.
    /// Every other role is restricted to records attributable to `sub`.
    pub fn has_tenant_wide_monitoring_scope(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "superadmin")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreviewClaims {
    sub: String,
    tenant_id: String,
    session_id: String,
    scope: String,
    exp: i64,
    iat: i64,
}

const PREVIEW_TOKEN_SCOPE: &str = "rd_preview_read";
const PREVIEW_TOKEN_TTL_HOURS: i64 = 4;
const LOGIN_FAILURE_LIMIT: u32 = 10;
const LOGIN_FAILURE_WINDOW: StdDuration = StdDuration::from_secs(5 * 60);

#[derive(Debug, Clone)]
struct LoginAttempt {
    failures: u32,
    window_started: Instant,
    blocked_until: Option<Instant>,
}

static LOGIN_ATTEMPTS: OnceLock<Mutex<HashMap<String, LoginAttempt>>> = OnceLock::new();
const DUMMY_PASSWORD_HASH: &str = "$2y$12$aoA6wUT73xFDG6lyw4jsZ.xXksUPC81vNdp991V6gQvTdiwLFFNZu";

fn login_attempt_key(headers: &HeaderMap, email: &str) -> String {
    let client = headers
        .get("x-real-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("direct");
    hex::encode(Sha256::digest(
        format!("{client}\n{}", email.trim().to_ascii_lowercase()).as_bytes(),
    ))
}

fn login_retry_after(key: &str) -> Option<u64> {
    let now = Instant::now();
    let mut attempts = LOGIN_ATTEMPTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let attempt = attempts.get(key)?;
    let blocked_until = attempt.blocked_until?;
    if blocked_until <= now {
        attempts.remove(key);
        return None;
    }
    Some(blocked_until.duration_since(now).as_secs().max(1))
}

fn record_login_failure(key: &str) {
    let now = Instant::now();
    let mut attempts = LOGIN_ATTEMPTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if attempts.len() > 10_000 {
        attempts.retain(|_, attempt| {
            attempt.blocked_until.is_some_and(|until| until > now)
                || now.duration_since(attempt.window_started) < LOGIN_FAILURE_WINDOW
        });
    }
    let attempt = attempts.entry(key.to_string()).or_insert(LoginAttempt {
        failures: 0,
        window_started: now,
        blocked_until: None,
    });
    if now.duration_since(attempt.window_started) >= LOGIN_FAILURE_WINDOW {
        attempt.failures = 0;
        attempt.window_started = now;
        attempt.blocked_until = None;
    }
    attempt.failures = attempt.failures.saturating_add(1);
    if attempt.failures >= LOGIN_FAILURE_LIMIT {
        attempt.blocked_until = Some(now + LOGIN_FAILURE_WINDOW);
    }
}

fn clear_login_failures(key: &str) {
    LOGIN_ATTEMPTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(key);
}

async fn password_matches(password: String, password_hash: String) -> bool {
    tokio::task::spawn_blocking(move || bcrypt::verify(password, &password_hash).unwrap_or(false))
        .await
        .unwrap_or(false)
}

pub async fn create_token(
    state: &AppState,
    user_id: &str,
    email: &str,
    role: &str,
    tenant_id: &str,
) -> Result<String> {
    let secret = state.jwt_secret.read().await;
    let claims = Claims::new(user_id, email, role, tenant_id);
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(AppError::Jwt)
}

pub async fn verify_token(state: &AppState, token: &str) -> Result<Claims> {
    let secret = state.jwt_secret.read().await;
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized)?;

    refresh_authorization_state(state, token_data.claims).await
}

async fn refresh_authorization_state(state: &AppState, mut claims: Claims) -> Result<Claims> {
    // JWTs prove signature and expiry, but authorization state remains live in
    // the database. Re-read the small primary-key row so disabled users, role /
    // tenant changes, and password resets take effect immediately.
    let current = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            bool,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    >(
        "SELECT email, role,
                COALESCE(tenant_id, '00000000-0000-0000-0000-000000000001'),
                is_active, password_changed_at
         FROM users WHERE id = ?",
    )
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    let Some((email, role, tenant_id, is_active, password_changed_at)) = current else {
        return Err(AppError::Unauthorized);
    };
    if !is_active
        || password_changed_at.is_some_and(|changed_at| changed_at.timestamp() > claims.iat)
    {
        return Err(AppError::Unauthorized);
    }
    claims.email = email;
    claims.role = role;
    claims.tenant_id = tenant_id;

    Ok(claims)
}

pub(crate) async fn create_preview_token(
    state: &AppState,
    claims: &Claims,
    session_id: &str,
) -> Result<String> {
    let now = Utc::now();
    let scoped = PreviewClaims {
        sub: claims.sub.clone(),
        tenant_id: claims.tenant_id.clone(),
        session_id: session_id.to_string(),
        scope: PREVIEW_TOKEN_SCOPE.to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::hours(PREVIEW_TOKEN_TTL_HOURS)).timestamp(),
    };
    let secret = state.jwt_secret.read().await;
    encode(
        &Header::default(),
        &scoped,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(AppError::Jwt)
}

pub(crate) async fn verify_preview_token(
    state: &AppState,
    token: &str,
    expected_session_id: &str,
) -> Result<Claims> {
    let secret = state.jwt_secret.read().await;
    let token_data = decode::<PreviewClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized)?;
    let scoped = token_data.claims;
    if scoped.scope != PREVIEW_TOKEN_SCOPE || scoped.session_id != expected_session_id {
        return Err(AppError::Unauthorized);
    }
    let claims = Claims {
        sub: scoped.sub,
        email: String::new(),
        role: String::new(),
        tenant_id: scoped.tenant_id,
        exp: scoped.exp,
        iat: scoped.iat,
    };
    refresh_authorization_state(state, claims).await
}

fn public_registration_enabled() -> bool {
    std::env::var("AOS_ALLOW_PUBLIC_REGISTRATION")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

#[cfg(test)]
mod login_limit_tests {
    use super::*;

    #[test]
    fn repeated_login_failures_are_temporarily_blocked_and_success_clears_state() {
        let key = format!("test-{}", uuid::Uuid::new_v4());
        assert!(login_retry_after(&key).is_none());
        for _ in 0..LOGIN_FAILURE_LIMIT {
            record_login_failure(&key);
        }
        assert!(login_retry_after(&key).is_some());
        clear_login_failures(&key);
        assert!(login_retry_after(&key).is_none());
    }
}

// ---- DTOs ----

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: FullUserInfo,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub invite_token: Option<String>,
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>> {
    let email_input = req.email.trim().to_ascii_lowercase();
    let attempt_key = login_attempt_key(&headers, &email_input);
    if let Some(retry_after) = login_retry_after(&attempt_key) {
        return Err(AppError::TooManyRequests(format!(
            "too many failed login attempts; retry after {retry_after} seconds"
        )));
    }
    let user = sqlx::query_as::<_, (String, String, String, String, String, String, bool, chrono::DateTime<chrono::Utc>, Option<String>, String, Option<String>, bool, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, email, name, password_hash, role, COALESCE(tenant_id, '00000000-0000-0000-0000-000000000001') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
         FROM users WHERE email = ?",
    )
    .bind(&email_input)
    .fetch_optional(&state.db)
    .await?;
    let Some(user) = user else {
        let _ = password_matches(req.password.clone(), DUMMY_PASSWORD_HASH.to_string()).await;
        record_login_failure(&attempt_key);
        return Err(AppError::Unauthorized);
    };

    let (
        id,
        email,
        name,
        password_hash,
        role,
        tenant_id,
        is_active,
        created_at,
        created_by,
        permission_mode,
        menu_permissions_json,
        menu_permissions_inherited,
        _last_login_at,
        password_changed_at,
    ) = user;

    if !is_active {
        let _ = password_matches(req.password.clone(), DUMMY_PASSWORD_HASH.to_string()).await;
        record_login_failure(&attempt_key);
        return Err(AppError::Unauthorized);
    }

    if password_matches(req.password.clone(), password_hash).await {
        clear_login_failures(&attempt_key);
    } else {
        record_login_failure(&attempt_key);
        return Err(AppError::Unauthorized);
    }

    let now = chrono::Utc::now();
    sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
        .bind(now)
        .bind(&id)
        .execute(&state.db)
        .await?;

    let token = create_token(&state, &id, &email, &role, &tenant_id).await?;

    Ok(Json(LoginResponse {
        token,
        user: FullUserInfo {
            id: id.clone(),
            email: email.clone(),
            name,
            role,
            tenant_id,
            is_active,
            permission_mode,
            menu_permissions: parse_permissions_json(menu_permissions_json),
            menu_permissions_inherited,
            created_at: created_at.to_rfc3339(),
            created_by,
            last_login_at: Some(now.to_rfc3339()),
            password_changed_at: password_changed_at.map(|dt| dt.to_rfc3339()),
        },
    }))
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<LoginResponse>> {
    if !public_registration_enabled() {
        return Err(AppError::Forbidden);
    }
    let email = req.email.trim().to_ascii_lowercase();
    let name = req.name.trim();
    if !email.contains('@') {
        return Err(AppError::ValidationError("invalid email".into()));
    }
    if name.is_empty() {
        return Err(AppError::ValidationError("name is required".into()));
    }
    if req.password.len() < 8 {
        return Err(AppError::ValidationError(
            "password must be at least 8 characters".into(),
        ));
    }
    let password_hash = bcrypt::hash(&req.password, bcrypt::DEFAULT_COST)?;

    let id = uuid::Uuid::new_v4().to_string();
    let role = "developer";

    // Create a unique tenant for this user.
    let tenant_id = uuid::Uuid::new_v4().to_string();
    let tenant_slug = email.split('@').next().unwrap_or("user").to_lowercase();

    // Wrap tenant + user creation in a single transaction so that a failure mid-way (e.g. duplicate
    // email after tenant insert) does not leave an orphan tenant row. Without this, the second
    // INSERT failing would leave a "free" tenant with no member — a subtle and hard-to-debug leak.
    let mut tx = state.db.begin().await.map_err(AppError::Database)?;

    sqlx::query("INSERT INTO tenants (id, name, slug, plan) VALUES (?, ?, ?, 'free')")
        .bind(&tenant_id)
        .bind(format!("{name}'s Workspace"))
        .bind(&tenant_slug)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
                AppError::Conflict("tenant already exists".into())
            } else {
                AppError::Database(e)
            }
        })?;

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, role, tenant_id) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&email)
    .bind(&password_hash)
    .bind(name)
    .bind(role)
    .bind(&tenant_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if matches!(e, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
            AppError::Conflict("email already exists".into())
        } else {
            AppError::Database(e)
        }
    })?;

    seed_tenant_defaults_with_tx(&mut tx, &tenant_id, Some(&id)).await?;

    tx.commit().await.map_err(AppError::Database)?;

    let token = create_token(&state, &id, &email, role, &tenant_id).await?;

    Ok(Json(LoginResponse {
        token,
        user: FullUserInfo {
            id,
            email,
            name: name.to_string(),
            role: role.to_string(),
            tenant_id: tenant_id.clone(),
            is_active: true,
            permission_mode: "workspace_write".to_string(),
            menu_permissions: Vec::new(),
            menu_permissions_inherited: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            created_by: None,
            last_login_at: None,
            password_changed_at: None,
        },
    }))
}

pub async fn me(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<FullUserInfo>> {
    let row = sqlx::query_as::<_, (String, String, String, String, String, bool, chrono::DateTime<chrono::Utc>, Option<String>, String, Option<String>, bool, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, email, name, role, COALESCE(tenant_id, '') as tenant_id, is_active, created_at, created_by, permission_mode, CAST(menu_permissions_json AS TEXT) as menu_permissions_json, (menu_permissions_json IS NULL) as menu_permissions_inherited, last_login_at, password_changed_at \
         FROM users WHERE id = ?",
    )
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(row) => Ok(Json(FullUserInfo::from(row))),
        None => Err(AppError::NotFound("user not found".into())),
    }
}
