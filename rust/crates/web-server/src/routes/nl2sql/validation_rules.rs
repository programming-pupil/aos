use super::{
    auth::require_admin, validate_data_source_access, CreateValidationRuleRequest,
    CreateValidationRuleResponse, DeleteValidationRuleResponse, ListValidationRulesResponse,
    UpdateValidationRuleRequest, UpdateValidationRuleResponse, ValidationRuleRow,
};
use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};

// GET /nl2sql/validation-rules/:datasource_id
pub(crate) async fn list_validation_rules(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<ListValidationRulesResponse>> {
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let rows: Vec<(i64, String, String, String, serde_json::Value, String, String, bool)> = sqlx::query_as::<sqlx::Sqlite, _>(
        r#"
        SELECT CAST(id AS INTEGER), table_name, column_name, rule_type, rule_config, severity, description, enabled
        FROM nl2sql_result_validation_rules
        WHERE datasource_id = ?
        ORDER BY table_name, column_name
        "#,
    )
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await?;

    let rules: Vec<ValidationRuleRow> = rows
        .into_iter()
        .map(
            |(
                id,
                table_name,
                column_name,
                rule_type,
                rule_config,
                severity,
                description,
                enabled,
            )| {
                ValidationRuleRow {
                    id,
                    table_name,
                    column_name,
                    rule_type,
                    rule_config,
                    severity,
                    description,
                    enabled,
                }
            },
        )
        .collect();

    Ok(Json(ListValidationRulesResponse { rules }))
}

// POST /nl2sql/validation-rules/:datasource_id
pub(crate) async fn create_validation_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<CreateValidationRuleRequest>,
) -> Result<Json<CreateValidationRuleResponse>> {
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;
    require_admin(&claims)?;

    let id: u64 = sqlx::query::<sqlx::Sqlite>(
        r#"
        INSERT INTO nl2sql_result_validation_rules
          (tenant_id, datasource_id, table_name, column_name, rule_type, rule_config, severity, description, created_by)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(tenant_id)
    .bind(&datasource_id)
    .bind(&req.table_name)
    .bind(&req.column_name)
    .bind(&req.rule_type)
    .bind(&req.rule_config)
    .bind(&req.severity)
    .bind(&req.description)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?
    .last_insert_rowid()
    .try_into()
    .map_err(|_| crate::AppError::Internal("invalid SQLite rowid".into()))?;

    Ok(Json(CreateValidationRuleResponse { id }))
}

// PATCH /nl2sql/validation-rules/:datasource_id/:rule_id
pub(crate) async fn update_validation_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, rule_id)): Path<(String, String)>,
    Json(req): Json<UpdateValidationRuleRequest>,
) -> Result<Json<UpdateValidationRuleResponse>> {
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;
    require_admin(&claims)?;
    let id = rule_id.parse::<u64>().unwrap_or(0);

    sqlx::query::<sqlx::Sqlite>(
        r#"
        UPDATE nl2sql_result_validation_rules SET
          table_name = COALESCE(?, table_name),
          column_name = COALESCE(?, column_name),
          rule_type = COALESCE(?, rule_type),
          rule_config = COALESCE(?, rule_config),
          severity = COALESCE(?, severity),
          description = COALESCE(?, description),
          enabled = COALESCE(?, enabled)
        WHERE id = ? AND datasource_id = ?
        "#,
    )
    .bind(&req.table_name)
    .bind(&req.column_name)
    .bind(&req.rule_type)
    .bind(req.rule_config.as_ref())
    .bind(&req.severity)
    .bind(&req.description)
    .bind(req.enabled)
    .bind(crate::sqlite_i64(id))
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    Ok(Json(UpdateValidationRuleResponse { success: true }))
}

// DELETE /nl2sql/validation-rules/:datasource_id/:rule_id
pub(crate) async fn delete_validation_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, rule_id)): Path<(String, String)>,
) -> Result<Json<DeleteValidationRuleResponse>> {
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;
    require_admin(&claims)?;
    let id = rule_id.parse::<u64>().unwrap_or(0);

    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_result_validation_rules WHERE id = ? AND datasource_id = ?",
    )
    .bind(crate::sqlite_i64(id))
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    Ok(Json(DeleteValidationRuleResponse { success: true }))
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Query Understanding Handler
// ══════════════════════════════════════════════════════════════════════════════

// POST /nl2sql/query-understanding/:datasource_id
