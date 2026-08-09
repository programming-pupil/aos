//! Projects routes — Gitlab project management per user.
//!
//! ## Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | /projects | List user's projects |
//! | POST | /projects | Add a new project |
//! | GET | /projects/{id} | Get project details |
//! | DELETE | /projects/{id} | Delete a project |
//! | POST | /projects/{id}/sync | Clone or sync the project |
//!
//! All endpoints require JWT Bearer authentication.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete as routing_delete, get as routing_get, post as routing_post},
    Json, Router,
};
use serde::Serialize;

use crate::auth::Claims;
use crate::state::AppState;

use agent_gateway::{AddProjectRequest, GitlabProject, GitlabProjectManager};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub url: String,
    pub branch: String,
    pub description: Option<String>,
    pub is_cloned: bool,
    pub clone_path: Option<String>,
    pub last_sync_at: Option<String>,
    pub created_at: String,
}

impl From<GitlabProject> for ProjectDto {
    fn from(p: GitlabProject) -> Self {
        Self {
            id: p.id,
            name: p.name,
            url: p.url,
            branch: p.branch,
            description: p.description,
            is_cloned: p.is_cloned,
            clone_path: p.clone_path,
            last_sync_at: p.last_sync_at.map(|dt| dt.to_rfc3339()),
            created_at: p.created_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<AppState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> impl axum::response::IntoResponse {
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let Some(token) = token else {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    };

    match crate::auth::verify_token(&state, token).await {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(_) => axum::http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

fn get_gitlab_manager(state: &AppState) -> &Arc<GitlabProjectManager> {
    state.gitlab_manager()
}

/// GET /api/v1/projects — list all projects for the authenticated user
async fn list_projects(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> impl IntoResponse {
    match get_gitlab_manager(&state)
        .list_projects(&claims.tenant_id, &claims.sub)
        .await
    {
        Ok(projects) => {
            let dtos: Vec<ProjectDto> = projects.into_iter().map(ProjectDto::from).collect();
            Json(serde_json::json!({ "projects": dtos, "total": dtos.len() })).into_response()
        }
        Err(e) => {
            let status = e.http_status();
            (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// POST /api/v1/projects — add a new project
async fn add_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AddProjectRequest>,
) -> impl IntoResponse {
    match get_gitlab_manager(&state)
        .add_project(&claims.tenant_id, &claims.sub, req)
        .await
    {
        Ok(project) => {
            let dto = ProjectDto::from(project);
            (StatusCode::CREATED, Json(dto)).into_response()
        }
        Err(e) => {
            let status = e.http_status();
            (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// GET /api/v1/projects/{id} — get project details
async fn get_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    match get_gitlab_manager(&state)
        .get_project(&claims.tenant_id, &claims.sub, &project_id)
        .await
    {
        Ok(project) => Json(ProjectDto::from(project)).into_response(),
        Err(e) => {
            let status = e.http_status();
            (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// DELETE /api/v1/projects/{id} — delete a project
async fn delete_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    match get_gitlab_manager(&state)
        .delete_project(&claims.tenant_id, &claims.sub, &project_id)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "deleted": true })).into_response(),
        Err(e) => {
            let status = e.http_status();
            (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// POST /api/v1/projects/{id}/sync — sync (clone or pull) a project
async fn sync_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    match get_gitlab_manager(&state)
        .sync_project(&claims.tenant_id, &claims.sub, &project_id)
        .await
    {
        Ok(path) => Json(serde_json::json!({
            "synced": true,
            "clone_path": path.to_string_lossy(),
        }))
        .into_response(),
        Err(e) => {
            let status = e.http_status();
            (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list_projects))
        .route("/", routing_post(add_project))
        .route("/{id}", routing_get(get_project))
        .route("/{id}", routing_delete(delete_project))
        .route("/{id}/sync", routing_post(sync_project))
        .layer(axum::middleware::from_fn_with_state(state, auth_middleware))
}
