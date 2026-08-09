//! Query Policy CRUD — per-(tenant, user, datasource) table/column allow/deny lists +
//! optional row-level filter expression.
//!
//! Moved out of the historic `mod.rs` god-file. Both the implementations and the public
//! shim wrappers (`qp_list` / `qp_create` / `qp_update` / `qp_delete`) live here so the
//! whole feature is auditable in one place.
//!
//! Query policies are tenant-wide security objects: they grant or deny SQL access at the
//! table and column level. Every write requires admin authority (see [`super::auth::require_admin`])
//! because a self-created policy is a privilege-escalation vector.

use super::auth::require_admin;
use super::PaginationParams;
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use serde::{Deserialize, Serialize};

// ── DTOs ────────────────────────────────────────────────────────────────────

/// Request body for creating a query policy.
#[derive(Debug, Deserialize)]
pub struct CreateQueryPolicyRequest {
    pub datasource_id: String,
    pub user_id: String,
    pub allowed_tables: Vec<String>,
    pub denied_tables: Vec<String>,
    pub allowed_columns: Vec<String>,
    pub denied_columns: Vec<String>,
    pub row_filter_expr: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

/// Request body for updating a query policy.
#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateQueryPolicyRequest {
    pub user_id: Option<String>,
    pub allowed_tables: Option<Vec<String>>,
    pub denied_tables: Option<Vec<String>>,
    pub allowed_columns: Option<Vec<String>>,
    pub denied_columns: Option<Vec<String>>,
    pub row_filter_expr: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

/// Single policy record returned by list / get.
#[derive(Debug, Serialize)]
pub struct QueryPolicyRecord {
    pub id: i64,
    pub tenant_id: String,
    pub datasource_id: String,
    pub user_id: String,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub allowed_tables: Vec<String>,
    pub denied_tables: Vec<String>,
    pub allowed_columns: Vec<String>,
    pub denied_columns: Vec<String>,
    pub row_filter_expr: Option<String>,
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Response for listing query policies.
#[derive(Debug, Serialize)]
pub struct QueryPolicyListResponse {
    pub items: Vec<QueryPolicyRecord>,
    pub total: usize,
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn json_vec(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn to_json_vec(v: &[String]) -> serde_json::Value {
    serde_json::json!(v)
}

async fn resolve_policy_user_id(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    identifier: &str,
) -> Result<String> {
    let identifier = identifier.trim();
    if identifier.is_empty() {
        return Err(AppError::ValidationError(
            "policy user id or email is required".to_string(),
        ));
    }
    sqlx::query_scalar::<_, String>(
        "SELECT id FROM users
         WHERE tenant_id = ? AND is_active = 1
           AND (id = ? OR LOWER(email) = LOWER(?))
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(identifier)
    .bind(identifier)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| {
        AppError::ValidationError(format!(
            "no active tenant user found for id or email: {identifier}"
        ))
    })
}

#[allow(clippy::type_complexity)]
fn row_to_policy(
    row: (
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
        Option<String>,
        Option<String>,
        bool,
        String,
        String,
    ),
) -> QueryPolicyRecord {
    QueryPolicyRecord {
        id: row.0,
        tenant_id: row.1,
        datasource_id: row.2,
        user_id: row.3,
        user_name: row.4,
        user_email: row.5,
        allowed_tables: json_vec(&row.6),
        denied_tables: json_vec(&row.7),
        allowed_columns: json_vec(&row.8),
        denied_columns: json_vec(&row.9),
        row_filter_expr: row.10,
        description: row.11,
        enabled: row.12,
        created_at: row.13,
        updated_at: row.14,
    }
}

// ── Implementations ────────────────────────────────────────────────────────

/// GET /api/v1/nl2sql/query-policies — list all policies for the tenant.
pub(crate) async fn list_query_policies(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<QueryPolicyListResponse>> {
    let offset = params.offset();
    let limit = params.limit();

    let tenant_id = &claims.tenant_id;

    let total: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM nl2sql_query_policies WHERE tenant_id = ?")
            .bind(tenant_id)
            .fetch_one(&state.db)
            .await?;

    let rows: Vec<QueryPolicyRecord> = sqlx::query_as(
        r#"SELECT CAST(p.id AS INTEGER), p.tenant_id, p.datasource_id, p.user_id,
                  u.name AS user_name, u.email AS user_email,
                  p.allowed_tables, p.denied_tables, p.allowed_columns, p.denied_columns,
                  p.row_filter_expr, p.description, p.enabled,
                  strftime('%Y-%m-%d %H:%M:%S', p.created_at) as created_at,
                  strftime('%Y-%m-%d %H:%M:%S', p.updated_at) as updated_at
           FROM nl2sql_query_policies p
           LEFT JOIN users u ON p.user_id = u.id
           WHERE p.tenant_id = ?
           ORDER BY p.updated_at DESC LIMIT ? OFFSET ?"#,
    )
    .bind(tenant_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(row_to_policy)
    .collect();

    Ok(Json(QueryPolicyListResponse {
        total: usize::try_from(total.0).unwrap_or(0),
        items: rows,
    }))
}

/// POST /api/v1/nl2sql/query-policies — create a new policy. Admin only.
pub(crate) async fn create_query_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateQueryPolicyRequest>,
) -> Result<Json<QueryPolicyRecord>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    let resolved_user_id = resolve_policy_user_id(&state.db, tenant_id, &req.user_id).await?;

    let insert_result = sqlx::query(
        r#"INSERT INTO nl2sql_query_policies
              (tenant_id, datasource_id, user_id,
               allowed_tables, denied_tables, allowed_columns, denied_columns,
               row_filter_expr, description, enabled)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(tenant_id)
    .bind(&req.datasource_id)
    .bind(&resolved_user_id)
    .bind(to_json_vec(&req.allowed_tables))
    .bind(to_json_vec(&req.denied_tables))
    .bind(to_json_vec(&req.allowed_columns))
    .bind(to_json_vec(&req.denied_columns))
    .bind(&req.row_filter_expr)
    .bind(&req.description)
    .bind(req.enabled.unwrap_or(true))
    .execute(&state.db)
    .await;

    if let Err(err) = insert_result {
        if let sqlx::Error::Database(db_err) = &err {
            let msg = db_err.message().to_ascii_lowercase();
            let is_policy_scope_duplicate = db_err.is_unique_violation()
                && (msg.contains("uk_tenant_ds_user")
                    || (msg.contains("nl2sql_query_policies.tenant_id")
                        && msg.contains("nl2sql_query_policies.datasource_id")
                        && msg.contains("nl2sql_query_policies.user_id")));
            if is_policy_scope_duplicate {
                return Err(AppError::Conflict(
                    "query policy already exists for this user and data source".into(),
                ));
            }
        }
        return Err(err.into());
    }

    let policy: QueryPolicyRecord = sqlx::query_as(
        r#"SELECT CAST(p.id AS INTEGER), p.tenant_id, p.datasource_id, p.user_id,
                  u.name AS user_name, u.email AS user_email,
                  p.allowed_tables, p.denied_tables, p.allowed_columns, p.denied_columns,
                  p.row_filter_expr, p.description, p.enabled,
                  strftime('%Y-%m-%d %H:%M:%S', p.created_at) as created_at,
                  strftime('%Y-%m-%d %H:%M:%S', p.updated_at) as updated_at
           FROM nl2sql_query_policies p
           LEFT JOIN users u ON p.user_id = u.id
           WHERE p.tenant_id = ? AND p.datasource_id = ? AND p.user_id = ?
           ORDER BY p.id DESC LIMIT 1"#,
    )
    .bind(tenant_id)
    .bind(&req.datasource_id)
    .bind(&resolved_user_id)
    .fetch_one(&state.db)
    .await
    .map(row_to_policy)?;

    Ok(Json(policy))
}

/// PATCH /api/v1/nl2sql/query-policies/{id} — update an existing policy. Admin only.
pub(crate) async fn update_query_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateQueryPolicyRequest>,
) -> Result<Json<QueryPolicyRecord>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    sqlx::query("SELECT id FROM nl2sql_query_policies WHERE id = ? AND tenant_id = ?")
        .bind(id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("policy not found".into()))?;

    if let Some(user_identifier) = req.user_id.as_deref() {
        let resolved_user_id =
            resolve_policy_user_id(&state.db, tenant_id, user_identifier).await?;
        let update_result = sqlx::query(
            "UPDATE nl2sql_query_policies
             SET user_id = ?, updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND tenant_id = ?",
        )
        .bind(resolved_user_id)
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await;
        if let Err(error) = update_result {
            if error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
            {
                return Err(AppError::Conflict(
                    "query policy already exists for this user and data source".into(),
                ));
            }
            return Err(error.into());
        }
    }

    if let Some(allowed) = req.allowed_tables {
        sqlx::query("UPDATE nl2sql_query_policies SET allowed_tables = ? WHERE id = ?")
            .bind(to_json_vec(&allowed))
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    if let Some(denied) = req.denied_tables {
        sqlx::query("UPDATE nl2sql_query_policies SET denied_tables = ? WHERE id = ?")
            .bind(to_json_vec(&denied))
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    if let Some(allowed_cols) = req.allowed_columns {
        sqlx::query("UPDATE nl2sql_query_policies SET allowed_columns = ? WHERE id = ?")
            .bind(to_json_vec(&allowed_cols))
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    if let Some(denied_cols) = req.denied_columns {
        sqlx::query("UPDATE nl2sql_query_policies SET denied_columns = ? WHERE id = ?")
            .bind(to_json_vec(&denied_cols))
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    if let Some(row_filter) = req.row_filter_expr {
        sqlx::query("UPDATE nl2sql_query_policies SET row_filter_expr = ? WHERE id = ?")
            .bind(&row_filter)
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    if let Some(desc) = req.description {
        sqlx::query("UPDATE nl2sql_query_policies SET description = ? WHERE id = ?")
            .bind(&desc)
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    if let Some(enabled) = req.enabled {
        sqlx::query("UPDATE nl2sql_query_policies SET enabled = ? WHERE id = ?")
            .bind(enabled)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    let policy: QueryPolicyRecord = sqlx::query_as(
        r#"SELECT CAST(p.id AS INTEGER), p.tenant_id, p.datasource_id, p.user_id,
                  u.name AS user_name, u.email AS user_email,
                  p.allowed_tables, p.denied_tables, p.allowed_columns, p.denied_columns,
                  p.row_filter_expr, p.description, p.enabled,
                  strftime('%Y-%m-%d %H:%M:%S', p.created_at) as created_at,
                  strftime('%Y-%m-%d %H:%M:%S', p.updated_at) as updated_at
           FROM nl2sql_query_policies p
           LEFT JOIN users u ON p.user_id = u.id
           WHERE p.id = ?"#,
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .map(row_to_policy)?;

    Ok(Json(policy))
}

/// DELETE /api/v1/nl2sql/query-policies/{id} — delete a policy. Admin only.
pub(crate) async fn delete_query_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    let affected = sqlx::query("DELETE FROM nl2sql_query_policies WHERE id = ? AND tenant_id = ?")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?
        .rows_affected();

    if affected == 0 {
        return Err(AppError::NotFound("policy not found".into()));
    }

    Ok(Json(serde_json::json!({ "deleted": true })))
}

// ── Public shim wrappers used by the router ────────────────────────────────
//
// These keep the historical `qp_*` route names stable while delegating to the
// canonical implementations above. The `require_admin` guard on the impls is
// authoritative; the shims are kept for backward-compatibility with the routes()
// wiring in `mod.rs`.

/// GET /api/v1/nl2sql/query-policies
pub(crate) async fn qp_list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<QueryPolicyListResponse>> {
    list_query_policies(State(state), Extension(claims), Query(params)).await
}

/// POST /api/v1/nl2sql/query-policies
pub(crate) async fn qp_create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateQueryPolicyRequest>,
) -> Result<Json<QueryPolicyRecord>> {
    require_admin(&claims)?;
    create_query_policy(State(state), Extension(claims), Json(req)).await
}

/// PATCH /api/v1/nl2sql/query-policies/{id}
pub(crate) async fn qp_update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateQueryPolicyRequest>,
) -> Result<Json<QueryPolicyRecord>> {
    require_admin(&claims)?;
    update_query_policy(State(state), Extension(claims), Path(id), Json(req)).await
}

/// DELETE /api/v1/nl2sql/query-policies/{id}
pub(crate) async fn qp_delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    delete_query_policy(State(state), Extension(claims), Path(id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::query(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                email TEXT NOT NULL,
                is_active INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create users");
        pool
    }

    #[tokio::test]
    async fn policy_user_email_resolves_to_active_user_in_same_tenant() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO users (id, tenant_id, email, is_active) VALUES
             ('user-a', 'tenant-a', 'Member@Example.com', 1),
             ('user-b', 'tenant-b', 'member@example.com', 1),
             ('user-c', 'tenant-a', 'disabled@example.com', 0)",
        )
        .execute(&pool)
        .await
        .expect("insert users");

        assert_eq!(
            resolve_policy_user_id(&pool, "tenant-a", "member@example.com")
                .await
                .expect("email should resolve"),
            "user-a"
        );
        assert!(
            resolve_policy_user_id(&pool, "tenant-a", "disabled@example.com")
                .await
                .is_err()
        );
        assert!(resolve_policy_user_id(&pool, "tenant-a", "user-b")
            .await
            .is_err());
    }
}
