use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use sqlx::Row;

/// Validates that the user has access to the given datasource.
/// Returns the datasource's (tenant_id, db_type, user_id) on success.
pub async fn require_datasource_access(
    state: &AppState,
    claims: &Claims,
    datasource_id: &str,
) -> Result<(String, String, Option<String>)> {
    let row = sqlx::query("SELECT tenant_id, user_id, db_type FROM data_sources WHERE id = ?")
        .bind(datasource_id)
        .fetch_optional(&state.db)
        .await?;
    let (ds_tenant, ds_user, db_type): (String, Option<String>, String) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id"), r.get("db_type")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }
    Ok((ds_tenant, db_type, ds_user))
}

/// Returns true if the current user has admin privileges within their tenant.
pub fn is_admin(claims: &Claims) -> bool {
    claims.role == "admin" || claims.role == "superadmin"
}

/// Guards an endpoint behind admin authority. Returns `AppError::Forbidden` for non-admins.
///
/// Use on any handler that mutates tenant-wide shared semantic objects (domains, metrics,
/// synonyms, foreign keys, join paths, query policies, cross-DS relations/clusters,
/// schema-change approvals, cache invalidation). Without this guard, any tenant member
/// can rewrite the entire knowledge base or grant themselves elevated query access.
pub fn require_admin(claims: &Claims) -> Result<()> {
    if is_admin(claims) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}
