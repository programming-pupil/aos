use crate::state::AppState;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::error::{AppError, Result};

#[derive(Debug, Deserialize)]
pub struct AcceptInviteRequest {
    pub password: String,
    pub invite_token: String,
}

async fn accept_invite(
    State(state): State<AppState>,
    Json(req): Json<AcceptInviteRequest>,
) -> Result<Json<serde_json::Value>> {
    if req.password.len() < 8 {
        return Err(AppError::ValidationError(
            "password must be at least 8 characters".into(),
        ));
    }

    let password_hash = bcrypt::hash(&req.password, bcrypt::DEFAULT_COST)?;
    let updated = sqlx::query(
        "UPDATE users
         SET password_hash = ?, invite_token = NULL, password_changed_at = CURRENT_TIMESTAMP
         WHERE invite_token = ? AND is_active = 1",
    )
    .bind(&password_hash)
    .bind(&req.invite_token)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::ValidationError(
            "invalid or expired invite token".into(),
        ));
    }

    Ok(Json(
        serde_json::json!({ "success": true, "message": "password set successfully" }),
    ))
}

pub fn routes(state: AppState) -> Router<AppState> {
    let protected = Router::<AppState>::new()
        .route("/me", get(crate::auth::me))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth_middleware::require_auth,
        ));
    Router::<AppState>::new()
        .route("/login", post(crate::auth::login))
        .route("/register", post(crate::auth::register))
        // No auth middleware — users accepting an invite don't have a token yet
        .route("/accept-invite", post(accept_invite))
        .merge(protected)
        .with_state(state)
}
