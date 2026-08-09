use super::auth::require_admin;
use super::{
    validate_data_source_access, AssignTablesToDomainRequest, AssignTablesToDomainResponse,
    BusinessDomainResponse, CreateDomainRequest, CreateDomainResponse, DeleteDomainResponse,
    DomainTableMappingItem, ListBusinessDomainsResponse, ListDomainTableMappingsResponse,
    ListDomainsForDatasourceResponse, RediscoverDomainsResponse, UnassignTablesFromDomainRequest,
    UnassignTablesFromDomainResponse, UpdateDomainRequest, UpdateDomainResponse,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};

fn normalize_domain_routing_mode(input: Option<&str>) -> &'static str {
    match input
        .unwrap_or("assist")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "strict" => "strict",
        _ => "assist",
    }
}

fn manual_domain_confidence(table_count: usize) -> f32 {
    if table_count == 0 {
        0.0
    } else {
        // Manual domain confidence grows with mapped tables and caps at 0.95.
        // This avoids overconfidence while still providing strong routing signal.
        (0.35 + (table_count as f32 * 0.1)).min(0.95)
    }
}

// GET /nl2sql/domains — list all domains across all datasources for tenant
pub(crate) async fn list_business_domains(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ListBusinessDomainsResponse>> {
    let tenant_id = &claims.tenant_id;
    let rows: Vec<(i64, String, String, String, i64, f32, String, String)> = sqlx::query_as(
        r#"
        SELECT CAST(d.id AS INTEGER), d.datasource_id, d.domain_name, d.domain_description,
               CAST(d.table_count AS INTEGER), d.confidence_score, d.source, d.domain_routing_mode
        FROM nl2sql_business_domains d
        JOIN data_sources ds ON ds.id = d.datasource_id
        WHERE d.tenant_id = ? AND ds.tenant_id = ? AND d.deleted_at IS NULL
        ORDER BY ds.name, d.table_count DESC
        "#,
    )
    .bind(tenant_id)
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let domains: Vec<BusinessDomainResponse> = rows
        .into_iter()
        .map(
            |(
                id,
                datasource_id,
                domain_name,
                domain_description,
                table_count,
                confidence_score,
                source,
                domain_routing_mode,
            )| {
                BusinessDomainResponse {
                    id,
                    datasource_id,
                    domain_name,
                    domain_description,
                    table_count,
                    confidence_score,
                    source,
                    domain_routing_mode,
                    tables: vec![],
                }
            },
        )
        .collect();

    Ok(Json(ListBusinessDomainsResponse { domains }))
}

// GET /nl2sql/domains/:datasource_id — list domains + tables for one datasource
pub(crate) async fn list_domains_for_datasource(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<ListDomainsForDatasourceResponse>> {
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    // Single JOIN query instead of N+1: fetch domains and their table lists in one round-trip.
    let rows: Vec<(i64, String, String, String, i64, f32, String, String)> = sqlx::query_as(
        r#"
        SELECT
            CAST(d.id AS INTEGER),
            d.domain_name,
            d.domain_description,
            d.source,
            CAST(d.table_count AS INTEGER),
            d.confidence_score,
            d.domain_routing_mode,
            COALESCE((
                SELECT GROUP_CONCAT(ordered.table_name, ',')
                FROM (
                    SELECT m2.table_name
                    FROM nl2sql_table_domain_mapping m2
                    WHERE m2.domain_id = d.id AND m2.deleted_at IS NULL
                    ORDER BY m2.table_name
                ) AS ordered
            ), '') AS tables
        FROM nl2sql_business_domains d
        WHERE d.datasource_id = ? AND d.deleted_at IS NULL
        ORDER BY d.table_count DESC
        "#,
    )
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await?;

    let results: Vec<BusinessDomainResponse> = rows
        .into_iter()
        .map(
            |(
                id,
                domain_name,
                domain_description,
                source,
                table_count,
                confidence_score,
                domain_routing_mode,
                tables_csv,
            )| {
                BusinessDomainResponse {
                    id,
                    datasource_id: datasource_id.clone(),
                    domain_name,
                    domain_description,
                    table_count,
                    confidence_score,
                    source,
                    domain_routing_mode,
                    tables: if tables_csv.is_empty() {
                        vec![]
                    } else {
                        tables_csv.split(',').map(String::from).collect()
                    },
                }
            },
        )
        .collect();

    Ok(Json(ListDomainsForDatasourceResponse { domains: results }))
}

// POST /nl2sql/domains/:datasource_id — create a new business domain manually.
pub(crate) async fn create_business_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<CreateDomainRequest>,
) -> Result<Json<CreateDomainResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    // Atomic create: domain row + initial table mappings happen in one transaction.
    // Without this, a partial mapping insert leaves an empty domain with no members.
    let mut tx = state.db.begin().await?;
    let domain_routing_mode = normalize_domain_routing_mode(req.domain_routing_mode.as_deref());
    let confidence_score = manual_domain_confidence(req.table_names.len());

    let id: i64 = sqlx::query(
        "INSERT INTO nl2sql_business_domains \
         (tenant_id, datasource_id, domain_name, domain_description, source, confidence_score, domain_routing_mode) \
         VALUES (?, ?, ?, ?, 'manual', ?, ?)",
    )
    .bind(tenant_id)
    .bind(&datasource_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(confidence_score)
    .bind(domain_routing_mode)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid() as i64;

    if !req.table_names.is_empty() {
        for table_name in &req.table_names {
            sqlx::query(
                "INSERT INTO nl2sql_table_domain_mapping \
                 (tenant_id, domain_id, datasource_id, table_name, confidence_score) \
                 VALUES (?, ?, ?, ?, 1.0)",
            )
            .bind(tenant_id)
            .bind(id)
            .bind(&datasource_id)
            .bind(table_name)
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;

    Ok(Json(CreateDomainResponse {
        id,
        datasource_id: datasource_id.clone(),
        domain_name: req.name,
        domain_description: req.description,
        table_count: req.table_names.len() as i64,
        confidence_score: confidence_score as f64,
        source: "manual".to_string(),
        domain_routing_mode: domain_routing_mode.to_string(),
        tables: req.table_names,
    }))
}

// POST /nl2sql/domains/:datasource_id/rediscover
pub(crate) async fn rediscover_domains(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<RediscoverDomainsResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    // `data_sources.schema_info` is JSON text; decode it directly into a JSON value.
    let schema_json = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT schema_info FROM data_sources WHERE id = ?",
    )
    .bind(&datasource_id)
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or_else(super::empty_schema_info);

    let chat_config = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::ValidationError(e.to_string()))?;

    let discoverer =
        crate::nl2sql::domain_discoverer::DomainDiscoverer::new(state.db.clone(), chat_config);

    let clusters = discoverer
        .rediscover(tenant_id, &datasource_id, &schema_json, Some(&claims.sub))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let count = clusters.len();
    Ok(Json(RediscoverDomainsResponse {
        domains_discovered: count,
    }))
}

// PATCH /nl2sql/domains/:datasource_id/tables/:domain_id
pub(crate) async fn update_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, domain_id)): Path<(String, String)>,
    Json(req): Json<UpdateDomainRequest>,
) -> Result<Json<UpdateDomainResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let domain_id_int: i64 = domain_id.parse().unwrap_or(0);
    let mapped_table_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM nl2sql_table_domain_mapping WHERE domain_id = ? AND deleted_at IS NULL",
    )
    .bind(domain_id_int)
    .fetch_one(&state.db)
    .await?;
    let confidence_score = manual_domain_confidence(mapped_table_count.max(0) as usize);
    let domain_routing_mode = normalize_domain_routing_mode(req.domain_routing_mode.as_deref());
    sqlx::query(
        "UPDATE nl2sql_business_domains \
         SET domain_name = ?, domain_description = ?, source = 'manual', confidence_score = ?, domain_routing_mode = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND datasource_id = ?",
    )
    .bind(&req.domain_name)
    .bind(&req.domain_description)
    .bind(confidence_score)
    .bind(domain_routing_mode)
    .bind(domain_id_int)
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    Ok(Json(UpdateDomainResponse { success: true }))
}

// DELETE /nl2sql/domains/:datasource_id/tables/:domain_id
pub(crate) async fn delete_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, domain_id)): Path<(String, String)>,
) -> Result<Json<DeleteDomainResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let domain_id_int: i64 = domain_id.parse().unwrap_or(0);
    sqlx::query("DELETE FROM nl2sql_business_domains WHERE id = ? AND datasource_id = ? AND deleted_at IS NULL")
        .bind(domain_id_int)
        .bind(&datasource_id)
        .execute(&state.db)
        .await?;

    Ok(Json(DeleteDomainResponse { success: true }))
}

// GET /nl2sql/domains/:datasource_id/tables/:domain_id/mappings
pub(crate) async fn list_domain_table_mappings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, domain_id)): Path<(String, String)>,
) -> Result<Json<ListDomainTableMappingsResponse>> {
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let domain_id_int: i64 = domain_id.parse().unwrap_or(0);
    let mappings: Vec<(i64, String, String, i64, f32)> = sqlx::query_as(
        r#"
        SELECT CAST(m.id AS INTEGER), m.table_name, m.datasource_id, CAST(m.domain_id AS INTEGER), m.confidence_score
        FROM nl2sql_table_domain_mapping m
        WHERE m.domain_id = ? AND m.datasource_id = ? AND m.deleted_at IS NULL
        ORDER BY m.table_name
        "#,
    )
    .bind(domain_id_int)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<DomainTableMappingItem> = mappings
        .into_iter()
        .map(
            |(id, table_name, datasource_id, domain_id, confidence_score)| DomainTableMappingItem {
                id,
                table_name,
                datasource_id,
                domain_id,
                confidence_score: confidence_score as f64,
            },
        )
        .collect();

    Ok(Json(ListDomainTableMappingsResponse { mappings: items }))
}

// POST /nl2sql/domains/:datasource_id/tables/:domain_id/mappings
pub(crate) async fn assign_tables_to_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, domain_id)): Path<(String, String)>,
    Json(req): Json<AssignTablesToDomainRequest>,
) -> Result<Json<AssignTablesToDomainResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let domain_id_int: i64 = domain_id.parse().unwrap_or(0);

    // Atomic: assign mappings AND refresh table_count in one transaction so the cached count
    // cannot drift away from the actual mapping rows under concurrent writes.
    let mut tx = state.db.begin().await?;
    let mut assigned = 0;
    for table_name in &req.table_names {
        sqlx::query(
            r#"
            INSERT INTO nl2sql_table_domain_mapping
                (tenant_id, datasource_id, table_name, domain_id, confidence_score, created_at)
            VALUES (?, ?, ?, ?, 1.0, CURRENT_TIMESTAMP)
            ON CONFLICT DO UPDATE SET domain_id = excluded.domain_id, confidence_score = excluded.confidence_score
            "#,
        )
        .bind(tenant_id)
        .bind(&datasource_id)
        .bind(table_name)
        .bind(domain_id_int)
        .execute(&mut *tx)
        .await?;
        assigned += 1;
    }

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nl2sql_table_domain_mapping WHERE domain_id = ? AND deleted_at IS NULL",
    )
    .bind(domain_id_int)
    .fetch_one(&mut *tx)
    .await?;
    let confidence_score = manual_domain_confidence(count.max(0) as usize);
    sqlx::query(
        "UPDATE nl2sql_business_domains SET table_count = ?, source = 'manual', confidence_score = ? WHERE id = ?",
    )
    .bind(count)
    .bind(confidence_score)
    .bind(domain_id_int)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(AssignTablesToDomainResponse {
        assigned_count: assigned,
    }))
}

// DELETE /nl2sql/domains/:datasource_id/tables/:domain_id/mappings
pub(crate) async fn unassign_tables_from_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, domain_id)): Path<(String, String)>,
    Json(req): Json<UnassignTablesFromDomainRequest>,
) -> Result<Json<UnassignTablesFromDomainResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let domain_id_int: i64 = domain_id.parse().unwrap_or(0);

    // Atomic: remove mappings AND refresh table_count in one transaction.
    let mut tx = state.db.begin().await?;
    let mut removed = 0;
    for table_name in &req.table_names {
        let result = sqlx::query(
            "DELETE FROM nl2sql_table_domain_mapping WHERE domain_id = ? AND table_name = ? AND deleted_at IS NULL",
        )
        .bind(domain_id_int)
        .bind(table_name)
        .execute(&mut *tx)
        .await?;
        removed += result.rows_affected() as i32;
    }

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nl2sql_table_domain_mapping WHERE domain_id = ? AND deleted_at IS NULL",
    )
    .bind(domain_id_int)
    .fetch_one(&mut *tx)
    .await?;
    let confidence_score = manual_domain_confidence(count.max(0) as usize);
    sqlx::query(
        "UPDATE nl2sql_business_domains SET table_count = ?, confidence_score = ? WHERE id = ?",
    )
    .bind(count)
    .bind(confidence_score)
    .bind(domain_id_int)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(UnassignTablesFromDomainResponse {
        removed_count: removed,
    }))
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Schema Change Notifications Handlers
// ══════════════════════════════════════════════════════════════════════════════

// GET /nl2sql/schema-changes
