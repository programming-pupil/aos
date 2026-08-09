//! Custom coding agent profiles for AOS Code Studio.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentProfileRequest {
    name: String,
    role_prompt: String,
    allowed_tools: Option<Value>,
    default_model: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentProfileDto {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) role_prompt: String,
    pub(super) allowed_tools: Option<Value>,
    pub(super) default_model: Option<String>,
    pub(super) enabled: bool,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

pub(super) async fn list_agent_profiles(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Vec<RdAgentProfileDto>>, AppError> {
    let rows = sqlx::query("SELECT id, name, role_prompt, allowed_tools, default_model, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at FROM rd_agent_profiles WHERE tenant_id = ? ORDER BY enabled DESC, updated_at DESC")
        .bind(&claims.tenant_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows.iter().map(row_to_agent_profile).collect()))
}

pub(super) async fn create_agent_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdAgentProfileRequest>,
) -> Result<Json<RdAgentProfileDto>, AppError> {
    let name = require_non_empty(&req.name, "name")?;
    let role_prompt = require_non_empty(&req.role_prompt, "role_prompt")?;
    extract_rd_allowed_tools(req.allowed_tools.as_ref())?;
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO rd_agent_profiles (id, tenant_id, name, role_prompt, allowed_tools, default_model, enabled) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&claims.tenant_id)
        .bind(&name)
        .bind(&role_prompt)
        .bind(&req.allowed_tools)
        .bind(normalize_optional(req.default_model.as_deref()))
        .bind(req.enabled.unwrap_or(true))
        .execute(&state.db)
        .await?;
    get_agent_profile_row(&state.db, &claims.tenant_id, &id)
        .await
        .map(Json)
}

pub(super) async fn update_agent_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RdAgentProfileRequest>,
) -> Result<Json<RdAgentProfileDto>, AppError> {
    let name = require_non_empty(&req.name, "name")?;
    let role_prompt = require_non_empty(&req.role_prompt, "role_prompt")?;
    extract_rd_allowed_tools(req.allowed_tools.as_ref())?;
    let result = sqlx::query("UPDATE rd_agent_profiles SET name = ?, role_prompt = ?, allowed_tools = ?, default_model = ?, enabled = ? WHERE id = ? AND tenant_id = ?")
        .bind(&name)
        .bind(&role_prompt)
        .bind(&req.allowed_tools)
        .bind(normalize_optional(req.default_model.as_deref()))
        .bind(req.enabled.unwrap_or(true))
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("rd agent profile not found".to_string()));
    }
    get_agent_profile_row(&state.db, &claims.tenant_id, &id)
        .await
        .map(Json)
}

pub(super) async fn delete_agent_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    let result = sqlx::query("DELETE FROM rd_agent_profiles WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "deleted": result.rows_affected() > 0 })))
}

pub(super) async fn get_agent_profile_row(
    db: &SqlitePool,
    tenant_id: &str,
    profile_id: &str,
) -> Result<RdAgentProfileDto, AppError> {
    let row = sqlx::query("SELECT id, name, role_prompt, allowed_tools, default_model, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at FROM rd_agent_profiles WHERE id = ? AND tenant_id = ?")
        .bind(profile_id)
        .bind(tenant_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("rd agent profile not found".to_string()))?;
    Ok(row_to_agent_profile(&row))
}

pub(super) async fn load_enabled_agent_profile(
    db: &SqlitePool,
    tenant_id: &str,
    profile_id: &str,
) -> Result<RdAgentProfileDto, AppError> {
    let profile = get_agent_profile_row(db, tenant_id, profile_id).await?;
    if !profile.enabled {
        return Err(AppError::ValidationError(
            "selected RD agent profile is disabled".to_string(),
        ));
    }
    Ok(profile)
}

fn row_to_agent_profile(row: &sqlx::sqlite::SqliteRow) -> RdAgentProfileDto {
    RdAgentProfileDto {
        id: row.get("id"),
        name: row.get("name"),
        role_prompt: row.get("role_prompt"),
        allowed_tools: row.get("allowed_tools"),
        default_model: row.get("default_model"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
