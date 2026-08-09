use super::auth::require_admin;
use super::{validate_data_source_access, JoinPathItem, ListJoinPathsResponse};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::{Deserialize, Serialize};
use sqlx::Row;

// GET /nl2sql/join-paths/:datasource_id
pub(crate) async fn list_join_paths(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<ListJoinPathsResponse>> {
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    // Query join paths scoped to this datasource and tenant.
    // Cast confidence/created_at into string-friendly types to avoid driver decode mismatches.
    let rows = sqlx::query(
        "SELECT id, path_text, hops, verified, \
         CAST(confidence AS TEXT) AS confidence_text, \
         source, CAST(created_at AS TEXT) AS created_at_text, \
         source_table, target_table, source_column, target_column, join_type, notes \
         FROM nl2sql_join_paths \
         WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL \
         ORDER BY hops ASC",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        AppError::Internal(format!(
            "failed to load join paths list for datasource: {}",
            e
        ))
    })?;

    // Also include cross-datasource relations for this tenant.
    #[derive(sqlx::FromRow)]
    struct CrossDsRow {
        id: i64,
        left_table: String,
        left_column: String,
        right_table: String,
        right_column: String,
        match_type: String,
        verified: bool,
        created_at: chrono::DateTime<chrono::Utc>,
    }

    let cross_ds_rows: Vec<CrossDsRow> = sqlx::query_as(
        "SELECT id, left_table, left_column, right_table, right_column, match_type, verified, \
         created_at \
         FROM nl2sql_cross_datasource_relations \
         WHERE tenant_id = ? \
         AND (left_datasource_id = ? OR right_datasource_id = ?) \
         AND deleted_at IS NULL \
         ORDER BY created_at DESC",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to load cross-ds join paths list: {}", e)))?;

    let mut all_paths: Vec<JoinPathItem> = Vec::with_capacity(rows.len());
    for r in rows {
        let confidence_text = r
            .try_get::<String, _>("confidence_text")
            .unwrap_or_else(|_| "1.0".to_string());
        let confidence = confidence_text.parse::<f32>().unwrap_or(1.0);
        let id = r
            .try_get::<i64, _>("id")
            .map_err(|e| AppError::Internal(format!("failed to decode join path id: {e}")))?;

        all_paths.push(JoinPathItem::from_path_text(
            id,
            &r.try_get::<String, _>("path_text").unwrap_or_default(),
            i32::from(r.try_get::<u16, _>("hops").unwrap_or(1)),
            r.try_get::<bool, _>("verified").unwrap_or(false),
            confidence,
            &r.try_get::<String, _>("source")
                .unwrap_or_else(|_| "auto".to_string()),
            r.try_get::<String, _>("created_at_text")
                .unwrap_or_default(),
            r.try_get::<Option<String>, _>("source_table")
                .ok()
                .flatten(),
            r.try_get::<Option<String>, _>("target_table")
                .ok()
                .flatten(),
            r.try_get::<Option<String>, _>("source_column")
                .ok()
                .flatten(),
            r.try_get::<Option<String>, _>("target_column")
                .ok()
                .flatten(),
            r.try_get::<Option<String>, _>("join_type").ok().flatten(),
            r.try_get::<Option<String>, _>("notes").ok().flatten(),
        ));
    }

    for r in cross_ds_rows {
        // Format cross-ds relation as a single-hop path_text.
        let path_text = format!(
            "{}.{} → {}.{} (1 hop)",
            r.left_table, r.left_column, r.right_table, r.right_column
        );
        all_paths.push(JoinPathItem::from_path_text(
            r.id,
            &path_text,
            1,
            r.verified,
            0.80,
            "cross_ds",
            r.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            Some(r.left_table),
            Some(r.right_table),
            Some(r.left_column),
            Some(r.right_column),
            Some(r.match_type),
            None,
        ));
    }

    Ok(Json(ListJoinPathsResponse { paths: all_paths }))
}

// POST /nl2sql/join-paths/:datasource_id/rediscover
pub(crate) async fn rediscover_join_paths(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    // Trigger join path refresh using the existing join_path module.
    let result =
        crate::nl2sql::join_path::refresh_join_paths(&state.db, &claims.tenant_id, &datasource_id)
            .await;

    match result {
        Ok(paths_discovered) => {
            let paths_visible: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) \
                 FROM nl2sql_join_paths \
                 WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
            )
            .bind(&claims.tenant_id)
            .bind(&datasource_id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| {
                AppError::Internal(format!(
                    "join path rediscover succeeded but count query failed: {}",
                    e
                ))
            })?;

            Ok(Json(serde_json::json!({
                "pathsDiscovered": paths_discovered,
                "pathsVisible": paths_visible
            })))
        }
        Err(e) => Err(AppError::Internal(format!(
            "failed to rediscover join paths: {}",
            e
        ))),
    }
}

// PATCH /nl2sql/join-paths/:datasource_id/:id/verify
pub(crate) async fn verify_join_path(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, path_id)): Path<(String, String)>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let verified = req
        .get("verified")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let id = path_id
        .parse::<i64>()
        .map_err(|_| AppError::ValidationError("invalid path id".into()))?;
    let result = sqlx::query("UPDATE nl2sql_join_paths SET verified = ? WHERE id = ? AND tenant_id = ? AND datasource_id = ?")
        .bind(verified)
        .bind(id)
        .bind(&claims.tenant_id)
        .bind(&datasource_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "join path not found or access denied".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "verified": verified })))
}

// POST /nl2sql/join-paths/:datasource_id
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateJoinPathRequest {
    source_table: String,
    target_table: String,
    source_column: String,
    target_column: String,
    #[serde(default)]
    join_type: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    notes: Option<String>,
}

pub(crate) async fn create_join_path(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<CreateJoinPathRequest>,
) -> Result<Json<JoinPathItem>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let join_type = req.join_type.as_deref().unwrap_or("INNER");
    let confidence = req.confidence.unwrap_or(1.0);

    let insert_result = sqlx::query(
        r#"INSERT INTO nl2sql_join_paths
           (tenant_id, datasource_id, source_table, target_table, source_column, target_column,
            path_text, sql_joins, hops, join_type, confidence, source, verified, notes)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 'manual', 1, ?)"#,
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(&req.source_table)
    .bind(&req.target_table)
    .bind(&req.source_column)
    .bind(&req.target_column)
    .bind(format!(
        "{}.{} → {}.{}",
        req.source_table, req.source_column, req.target_table, req.target_column
    ))
    .bind(format!(
        "INNER JOIN {} ON {}.{} = {}.{}",
        req.target_table, req.source_table, req.source_column, req.target_table, req.target_column
    ))
    .bind(join_type)
    .bind(confidence)
    .bind(&req.notes)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("create join path failed: {}", e)))?;

    let inserted_id = i64::try_from(insert_result.last_insert_rowid())
        .map_err(|_| AppError::Internal("join path id overflow".into()))?;

    let path_text = format!(
        "{}.{} → {}.{}",
        req.source_table, req.source_column, req.target_table, req.target_column
    );
    Ok(Json(JoinPathItem {
        id: inserted_id,
        path: vec![req.source_table.clone(), req.target_table.clone()],
        join_columns: vec![req.source_column.clone(), req.target_column.clone()],
        ds_ids: Vec::new(),
        total_columns: 1,
        verified: true,
        confidence,
        source: "manual".to_string(),
        created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        source_table: Some(req.source_table),
        target_table: Some(req.target_table),
        source_column: Some(req.source_column),
        target_column: Some(req.target_column),
        join_type: Some(join_type.to_string()),
        path_text: Some(path_text),
        sql_joins: None,
        notes: req.notes,
    }))
}

// PUT /nl2sql/join-paths/:datasource_id/:path_id
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateJoinPathRequest {
    #[serde(default)]
    source_table: Option<String>,
    #[serde(default)]
    target_table: Option<String>,
    #[serde(default)]
    source_column: Option<String>,
    #[serde(default)]
    target_column: Option<String>,
    #[serde(default)]
    join_type: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    verified: Option<bool>,
    #[serde(default)]
    notes: Option<String>,
}

pub(crate) async fn update_join_path(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, path_id)): Path<(String, String)>,
    Json(req): Json<UpdateJoinPathRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let id = path_id
        .parse::<i64>()
        .map_err(|_| AppError::ValidationError("invalid path id".into()))?;

    let result = sqlx::query(
        r#"UPDATE nl2sql_join_paths SET
           source_table = COALESCE(?, source_table),
           target_table = COALESCE(?, target_table),
           source_column = COALESCE(?, source_column),
           target_column = COALESCE(?, target_column),
           join_type = COALESCE(?, join_type),
           confidence = COALESCE(?, confidence),
           verified = COALESCE(?, verified),
           notes = ?
           WHERE id = ? AND tenant_id = ? AND datasource_id = ?"#,
    )
    .bind(&req.source_table)
    .bind(&req.target_table)
    .bind(&req.source_column)
    .bind(&req.target_column)
    .bind(&req.join_type)
    .bind(req.confidence)
    .bind(req.verified)
    .bind(&req.notes)
    .bind(id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "join path not found or access denied".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "success": true })))
}

// DELETE /nl2sql/join-paths/:datasource_id/:path_id
pub(crate) async fn delete_join_path(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, path_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let id = path_id
        .parse::<i64>()
        .map_err(|_| AppError::ValidationError("invalid path id".into()))?;

    // Soft-delete: set deleted_at instead of hard delete.
    let result = sqlx::query(
        "UPDATE nl2sql_join_paths SET deleted_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND datasource_id = ?",
    )
    .bind(id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "join path not found or access denied".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "success": true })))
}

// ══════════════════════════════════════════════════════════════════════════════
// P2-2: Cross-Datasource Relations Management
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CrossDSRelationItem {
    id: i64,
    left_datasource: String,
    left_table: String,
    left_column: String,
    right_datasource: String,
    right_table: String,
    right_column: String,
    match_type: String,
    confidence: f32,
    verified: bool,
    source: String,
    created_at: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListCrossDSRelationsResponse {
    relations: Vec<CrossDSRelationItem>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateCrossDSRelationRequest {
    left_datasource: String,
    left_table: String,
    left_column: String,
    right_datasource: String,
    right_table: String,
    right_column: String,
    #[serde(default = "default_match_type")]
    match_type: String,
}
fn default_match_type() -> String {
    "foreign_key".to_string()
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateCrossDSRelationRequest {
    verified: Option<bool>,
    match_type: Option<String>,
}

// GET /nl2sql/cross-ds-relations
