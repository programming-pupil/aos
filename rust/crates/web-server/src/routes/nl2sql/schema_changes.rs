use super::auth::require_admin;
use super::{
    AffectedQuery, ApproveSchemaChangeResponse, ListSchemaChangesResponse,
    RejectSchemaChangeResponse, SchemaChangeDetailResponse, SchemaChangeNotification,
    SchemaChangesQuery,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use sqlx::Row;

// GET /nl2sql/schema-changes
pub(crate) async fn list_schema_changes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<SchemaChangesQuery>,
) -> Result<Json<ListSchemaChangesResponse>> {
    let tenant_id = &claims.tenant_id;
    let status = params.status.as_deref().unwrap_or("pending");
    let page = params.page.unwrap_or(1) as i64;
    let per_page = params.per_page.unwrap_or(20) as i64;
    let offset = (page - 1) * per_page;

    let rows: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        i32,
        String,
        String,
    )> = sqlx::query_as(
        r#"
            SELECT n.id, n.datasource_id, n.change_type, n.details,
                   n.recommended_action, n.status, n.affected_queries_count,
                   n.created_at, COALESCE(n.reviewed_by, '')
            FROM nl2sql_schema_change_notifications n
            JOIN data_sources ds ON ds.id = n.datasource_id
            WHERE n.tenant_id = ? AND ds.tenant_id = ? AND (? = 'all' OR n.status = ?)
            ORDER BY n.created_at DESC
            LIMIT ? OFFSET ?
            "#,
    )
    .bind(tenant_id)
    .bind(tenant_id)
    .bind(status)
    .bind(status)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nl2sql_schema_change_notifications n JOIN data_sources ds ON ds.id = n.datasource_id WHERE n.tenant_id = ? AND ds.tenant_id = ? AND (? = 'all' OR n.status = ?)",
    )
    .bind(tenant_id)
    .bind(tenant_id)
    .bind(status)
    .bind(status)
    .fetch_one(&state.db)
    .await?;

    let changes: Vec<SchemaChangeNotification> = rows
        .into_iter()
        .map(
            |(
                id,
                datasource_id,
                change_type,
                details,
                recommended_action,
                status,
                affected_queries_count,
                created_at,
                reviewed_by,
            )| {
                SchemaChangeNotification {
                    id,
                    datasource_id,
                    change_type,
                    details: serde_json::from_str(&details).unwrap_or_default(),
                    recommended_action,
                    status,
                    affected_queries_count,
                    created_at,
                    reviewed_by,
                    reviewed_at: None,
                }
            },
        )
        .collect();

    Ok(Json(ListSchemaChangesResponse { changes, total }))
}

// GET /api/v1/nl2sql/schema/:data_source_id — get data source schema for UI.
pub(crate) async fn get_schema(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(data_source_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let row = sqlx::query("SELECT tenant_id, user_id, schema_info FROM data_sources WHERE id = ?")
        .bind(&data_source_id)
        .fetch_optional(&state.db)
        .await?;

    let (tenant_id, user_id, schema_info): (String, Option<String>, Option<serde_json::Value>) =
        match row {
            Some(r) => (r.get("tenant_id"), r.get("user_id"), r.get("schema_info")),
            None => return Err(AppError::NotFound("data source not found".into())),
        };

    if tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if user_id.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    Ok(Json(schema_info.unwrap_or(serde_json::json!([]))))
}

// GET /nl2sql/schema-changes/:notification_id
pub(crate) async fn get_schema_change_detail(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(notification_id): Path<String>,
) -> Result<Json<SchemaChangeDetailResponse>> {
    let tenant_id = &claims.tenant_id;
    let id = notification_id.parse::<i64>().unwrap_or(0);

    let row: Option<(String, String, String, String, String, i32, String)> = sqlx::query_as(
        r#"
        SELECT n.datasource_id, n.change_type, n.details, n.recommended_action, n.status,
               n.affected_queries_count, n.created_at
        FROM nl2sql_schema_change_notifications n
        JOIN data_sources ds ON ds.id = n.datasource_id
        WHERE n.id = ? AND n.tenant_id = ?
        "#,
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?;

    let (
        datasource_id,
        change_type,
        details,
        recommended_action,
        status,
        affected_queries_count,
        created_at,
    ) = row.ok_or(AppError::NotFound("schema change not found".to_string()))?;

    let affected: Vec<AffectedQuery> = sqlx::query_as(
        "SELECT query_id, question, generated_sql, impact_level FROM nl2sql_affected_queries WHERE notification_id = ?",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    Ok(Json(SchemaChangeDetailResponse {
        datasource_id,
        change_type,
        details: serde_json::from_str(&details).unwrap_or_default(),
        recommended_action,
        status,
        affected_queries_count,
        created_at,
        affected_queries: affected,
    }))
}

// POST /nl2sql/schema-changes/:notification_id/approve
pub(crate) async fn approve_schema_change(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(notification_id): Path<String>,
) -> Result<Json<ApproveSchemaChangeResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    let id = notification_id.parse::<i64>().unwrap_or(0);

    let datasource_id: Option<String> = sqlx::query_scalar(
        "SELECT datasource_id FROM nl2sql_schema_change_notifications WHERE id = ? AND tenant_id = ?",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?;

    let datasource_id =
        datasource_id.ok_or(AppError::NotFound("datasource not found".to_string()))?;

    // Atomic: marking the notification approved AND flipping the reindex flag must commit
    // together so the UI never observes "approved but not scheduled for reindex".
    let mut tx = state.db.begin().await?;

    sqlx::query(
        "UPDATE nl2sql_schema_change_notifications SET status = 'approved', reviewed_by = ?, reviewed_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(&claims.sub)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE data_sources SET embedding_needs_reindex = 1 WHERE id = ?")
        .bind(&datasource_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(Json(ApproveSchemaChangeResponse { success: true }))
}

// POST /nl2sql/schema-changes/:notification_id/reject
pub(crate) async fn reject_schema_change(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(notification_id): Path<String>,
) -> Result<Json<RejectSchemaChangeResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    let id = notification_id.parse::<i64>().unwrap_or(0);

    sqlx::query(
        "UPDATE nl2sql_schema_change_notifications SET status = 'rejected', reviewed_by = ?, reviewed_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ?",
    )
    .bind(&claims.sub)
    .bind(id)
    .bind(tenant_id)
    .execute(&state.db)
    .await?;

    Ok(Json(RejectSchemaChangeResponse { success: true }))
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Time Patterns Handlers
// ══════════════════════════════════════════════════════════════════════════════

// GET /nl2sql/time-patterns
