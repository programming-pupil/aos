use super::auth::require_admin;
use super::{
    CreateCrossDSRelationRequest, CrossDSRelationItem, ListCrossDSRelationsResponse,
    UpdateCrossDSRelationRequest,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::{Deserialize, Serialize};
use sqlx::QueryBuilder;

fn normalize_match_type(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        // Canonical DB enum values
        "id" => Some("id"),
        "email" => Some("email"),
        "name" => Some("name"),
        "foreign_key" => Some("foreign_key"),
        "custom" => Some("custom"),
        // Backward-compatible aliases from older UI values
        "exact" => Some("id"),
        "fuzzy" => Some("custom"),
        "schema_similarity" => Some("foreign_key"),
        "name_similarity" => Some("name"),
        _ => None,
    }
}

fn validate_match_type(raw: &str) -> Result<&'static str> {
    normalize_match_type(raw).ok_or_else(|| {
        AppError::ValidationError(
            "invalid match_type, expected one of: id, email, name, foreign_key, custom".into(),
        )
    })
}

fn relation_hash(lds: &str, lt: &str, lc: &str, rds: &str, rt: &str, rc: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(lds.as_bytes());
    hasher.update(b"|");
    hasher.update(lt.as_bytes());
    hasher.update(b"|");
    hasher.update(lc.as_bytes());
    hasher.update(b"|");
    hasher.update(rds.as_bytes());
    hasher.update(b"|");
    hasher.update(rt.as_bytes());
    hasher.update(b"|");
    hasher.update(rc.as_bytes());
    format!("{:x}", hasher.finalize())
}

// GET /nl2sql/cross-ds-relations
pub(crate) async fn list_cross_ds_relations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ListCrossDSRelationsResponse>> {
    let tenant_id = &claims.tenant_id;

    let rows: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        f64,
        bool,
        String,
        String,
    )> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(id AS INTEGER), left_datasource_id, left_table, left_column, right_datasource_id, right_table, right_column, \
         match_type, confidence, verified, source, strftime('%Y-%m-%d %H:%M:%S', created_at) \
         FROM nl2sql_cross_datasource_relations WHERE tenant_id = ? AND deleted_at IS NULL ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let relations = rows
        .into_iter()
        .map(|r| CrossDSRelationItem {
            id: r.0,
            left_datasource: r.1,
            left_table: r.2,
            left_column: r.3,
            right_datasource: r.4,
            right_table: r.5,
            right_column: r.6,
            match_type: r.7,
            confidence: r.8 as f32,
            verified: r.9,
            source: r.10,
            created_at: r.11,
        })
        .collect();

    Ok(Json(ListCrossDSRelationsResponse { relations }))
}

// POST /nl2sql/cross-ds-relations
pub(crate) async fn create_cross_ds_relation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateCrossDSRelationRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    let match_type = validate_match_type(&req.match_type)?;
    let rel_hash = relation_hash(
        &req.left_datasource,
        &req.left_table,
        &req.left_column,
        &req.right_datasource,
        &req.right_table,
        &req.right_column,
    );
    let semantic_description = format!(
        "Manual relation: {}.{} ↔ {}.{}",
        req.left_table, req.left_column, req.right_table, req.right_column
    );

    // BUG-FIX: Validate both datasources belong to this tenant before creating.
    let left_valid = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT COUNT(*) FROM data_sources WHERE id = ? AND tenant_id = ?",
    )
    .bind(&req.left_datasource)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let right_valid = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT COUNT(*) FROM data_sources WHERE id = ? AND tenant_id = ?",
    )
    .bind(&req.right_datasource)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if left_valid == 0 {
        return Err(AppError::ValidationError(
            format!(
                "left datasource '{}' not found or access denied",
                req.left_datasource
            )
            .into(),
        ));
    }
    if right_valid == 0 {
        return Err(AppError::ValidationError(
            format!(
                "right datasource '{}' not found or access denied",
                req.right_datasource
            )
            .into(),
        ));
    }

    let result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_cross_datasource_relations \
         (tenant_id, left_datasource_id, left_table, left_column, right_datasource_id, right_table, right_column, \
          relation_hash, semantic_description, match_type, confidence, verified, source, created_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1.0, false, 'manual', ?)",
    )
    .bind(tenant_id)
    .bind(&req.left_datasource)
    .bind(&req.left_table)
    .bind(&req.left_column)
    .bind(&req.right_datasource)
    .bind(&req.right_table)
    .bind(&req.right_column)
    .bind(&rel_hash)
    .bind(&semantic_description)
    .bind(match_type)
    .bind(&claims.sub)
    .execute(&state.db)
    .await;

    let result = match result {
        Ok(r) => r,
        Err(sqlx::Error::Database(db_err))
            if db_err.code().map(|c| c.as_ref() == "1062").unwrap_or(false)
                || db_err
                    .message()
                    .to_ascii_lowercase()
                    .contains("duplicate entry") =>
        {
            return Err(AppError::Conflict(
                "cross-datasource relation already exists".into(),
            ));
        }
        Err(e) => return Err(e.into()),
    };

    Ok(Json(
        serde_json::json!({ "id": result.last_insert_rowid() }),
    ))
}

// PATCH /nl2sql/cross-ds-relations/:id
pub(crate) async fn update_cross_ds_relation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(relation_id): Path<String>,
    Json(req): Json<UpdateCrossDSRelationRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    // BUG-FIX: Fetch the relation first to validate datasources belong to this tenant.
    let (left_ds, right_ds): (String, String) = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT left_datasource_id, right_datasource_id FROM nl2sql_cross_datasource_relations \
         WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(
        relation_id
            .parse::<i64>()
            .map_err(|_| AppError::ValidationError("invalid relation id".into()))?,
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .map(|r: (String, String)| r)
    .ok_or_else(|| AppError::NotFound("relation not found or access denied".into()))?;

    // Validate both datasources belong to this tenant.
    let left_valid = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT COUNT(*) FROM data_sources WHERE id = ? AND tenant_id = ?",
    )
    .bind(&left_ds)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let right_valid = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT COUNT(*) FROM data_sources WHERE id = ? AND tenant_id = ?",
    )
    .bind(&right_ds)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if left_valid == 0 || right_valid == 0 {
        return Err(AppError::Forbidden);
    }

    let mut qb: QueryBuilder<sqlx::Sqlite> =
        QueryBuilder::new("UPDATE nl2sql_cross_datasource_relations SET ");
    let mut needs_comma = false;

    if let Some(v) = req.verified {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("verified = ");
        qb.push_bind(v);
        needs_comma = true;
    }
    if let Some(ref v) = req.match_type {
        let normalized = validate_match_type(v)?;
        if needs_comma {
            qb.push(", ");
        }
        qb.push("match_type = ");
        qb.push_bind(normalized);
        needs_comma = true;
    }

    if !needs_comma {
        return Err(AppError::ValidationError("No fields to update".into()));
    }

    let rel_id = relation_id
        .parse::<u64>()
        .map_err(|_| AppError::ValidationError("invalid relation id".into()))?;

    qb.push(" WHERE id = ");
    qb.push_bind(crate::sqlite_i64(rel_id));
    qb.push(" AND tenant_id = ");
    qb.push_bind(tenant_id);

    let result = qb.build().execute(&state.db).await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "relation not found or access denied".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "updated": true })))
}

// DELETE /nl2sql/cross-ds-relations/:id
pub(crate) async fn delete_cross_ds_relation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(relation_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    // Only allow deleting manual relations (auto-discovered cannot be deleted).
    let affected = sqlx::query::<sqlx::Sqlite>("DELETE FROM nl2sql_cross_datasource_relations WHERE id = ? AND tenant_id = ? AND source = 'manual' AND deleted_at IS NULL")
        .bind(relation_id.parse::<i64>().map_err(|_| AppError::ValidationError("invalid relation id".into()))?)
        .bind(tenant_id)
        .execute(&state.db)
        .await?
        .rows_affected();

    if affected == 0 {
        return Err(AppError::ValidationError(
            "Cannot delete auto-discovered relations. Only manual relations can be deleted.".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "deleted": true })))
}

// ══════════════════════════════════════════════════════════════════════════════
// P2-3: Cross-Domain Cluster Management
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CrossDomainClusterItem {
    id: i64,
    cluster_name: String,
    datasource_ids: serde_json::Value,
    domain_ids: serde_json::Value,
    description: Option<String>,
    auto_discovered: bool,
    created_by: Option<String>,
    created_at: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListCrossDomainClustersResponse {
    clusters: Vec<CrossDomainClusterItem>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateCrossDomainClusterRequest {
    cluster_name: String,
    #[serde(default)]
    datasource_ids: serde_json::Value,
    #[serde(default)]
    domain_ids: serde_json::Value,
    #[serde(default)]
    description: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateCrossDomainClusterRequest {
    cluster_name: Option<String>,
    datasource_ids: Option<serde_json::Value>,
    domain_ids: Option<serde_json::Value>,
    description: Option<String>,
}

// GET /nl2sql/cross-domain-clusters
pub(crate) async fn list_cross_domain_clusters(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ListCrossDomainClustersResponse>> {
    let tenant_id = &claims.tenant_id;

    let rows: Vec<(
        i64,
        String,
        serde_json::Value,
        serde_json::Value,
        Option<String>,
        bool,
        Option<String>,
        String,
    )> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(c.id AS INTEGER), c.cluster_name, c.datasource_ids, c.domain_ids, c.description, c.auto_discovered, \
         COALESCE(NULLIF(u.name, ''), NULLIF(u.email, ''), c.created_by) AS created_by, \
         strftime('%Y-%m-%d %H:%M:%S', c.created_at) \
         FROM nl2sql_cross_domain_clusters c \
         LEFT JOIN users u ON c.created_by = u.id \
         WHERE c.tenant_id = ? AND c.deleted_at IS NULL \
         ORDER BY c.cluster_name",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let clusters = rows
        .into_iter()
        .map(|r| CrossDomainClusterItem {
            id: r.0,
            cluster_name: r.1,
            datasource_ids: r.2,
            domain_ids: r.3,
            description: r.4,
            auto_discovered: r.5,
            created_by: r.6,
            created_at: r.7,
        })
        .collect();

    Ok(Json(ListCrossDomainClustersResponse { clusters }))
}

// POST /nl2sql/cross-domain-clusters
pub(crate) async fn create_cross_domain_cluster(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateCrossDomainClusterRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    let result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_cross_domain_clusters (tenant_id, cluster_name, datasource_ids, domain_ids, \
         description, auto_discovered, created_by) VALUES (?, ?, ?, ?, ?, false, ?)",
    )
    .bind(tenant_id)
    .bind(&req.cluster_name)
    .bind(&req.datasource_ids)
    .bind(&req.domain_ids)
    .bind(&req.description)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;

    Ok(Json(
        serde_json::json!({ "id": result.last_insert_rowid() }),
    ))
}

// PATCH /nl2sql/cross-domain-clusters/:id
pub(crate) async fn update_cross_domain_cluster(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(cluster_id): Path<String>,
    Json(req): Json<UpdateCrossDomainClusterRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    let cluster_id = cluster_id
        .parse::<i64>()
        .map_err(|_| AppError::ValidationError("invalid cluster id".into()))?;

    let mut qb: QueryBuilder<sqlx::Sqlite> =
        QueryBuilder::new("UPDATE nl2sql_cross_domain_clusters SET ");
    let mut needs_comma = false;

    if let Some(ref v) = req.cluster_name {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("cluster_name = ");
        qb.push_bind(v);
        needs_comma = true;
    }
    if let Some(ref v) = req.datasource_ids {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("datasource_ids = ");
        qb.push_bind(v);
        needs_comma = true;
    }
    if let Some(ref v) = req.domain_ids {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("domain_ids = ");
        qb.push_bind(v);
        needs_comma = true;
    }
    if let Some(ref v) = req.description {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("description = ");
        qb.push_bind(v);
    }

    if !needs_comma && req.description.is_none() {
        return Err(AppError::ValidationError("No fields to update".into()));
    }

    qb.push(" WHERE id = ");
    qb.push_bind(cluster_id);
    qb.push(" AND tenant_id = ");
    qb.push_bind(tenant_id);

    let _result = qb.build().execute(&state.db).await?;

    Ok(Json(serde_json::json!({ "updated": true })))
}

// DELETE /nl2sql/cross-domain-clusters/:id
pub(crate) async fn delete_cross_domain_cluster(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(cluster_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    sqlx::query::<sqlx::Sqlite>("DELETE FROM nl2sql_cross_domain_clusters WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL")
        .bind(
            cluster_id
                .parse::<i64>()
                .map_err(|_| AppError::ValidationError("invalid cluster id".into()))?,
        )
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}

// POST /nl2sql/cross-domain-clusters/auto-discover
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutoDiscoverClustersRequest {
    datasource_ids: Vec<String>,
    #[serde(default)]
    auto_save: bool,
}

pub(crate) async fn auto_discover_clusters(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AutoDiscoverClustersRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    if req.datasource_ids.is_empty() {
        return Err(AppError::ValidationError(
            "datasource_ids is required".into(),
        ));
    }

    // BUG-FIX: Validate all datasource_ids belong to this tenant.
    for ds_id in &req.datasource_ids {
        let count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT COUNT(*) FROM data_sources WHERE id = ? AND tenant_id = ?",
        )
        .bind(ds_id)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        if count == 0 {
            return Err(AppError::ValidationError(
                format!("datasource '{}' not found or access denied", ds_id).into(),
            ));
        }
    }

    // Load schemas for the given datasources.
    let mut schema_summary = String::new();
    for ds_id in &req.datasource_ids {
        let tables: Vec<(String, String)> = sqlx::query_as::<sqlx::Sqlite, _>(
            "SELECT table_name, COALESCE(ai_description, '') FROM nl2sql_table_desc_semantics \
             WHERE datasource_id = ? AND column_name = 'table_name' AND deleted_at IS NULL LIMIT 100",
        )
        .bind(ds_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        if !tables.is_empty() {
            schema_summary.push_str(&format!("Datasource {}:\n", ds_id));
            for (t, desc) in tables {
                schema_summary.push_str(&format!("  - {}: {}\n", t, desc));
            }
        }
    }

    // Use LLM to suggest cross-domain clusters based on schema summary.
    let llm_model_default = std::env::var("NL2SQL_ROUTING_LLM_MODEL")
        .ok()
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    let Ok((llm_client, llm_model, _llm_meta)) = crate::nl2sql::routing::resolve_routing_llm(
        &state.config_registry,
        tenant_id,
        &claims.sub,
        &llm_model_default,
    )
    .await
    else {
        return Ok(Json(
            serde_json::json!({ "suggestions": [], "error": "Failed to resolve LLM for cluster discovery" }),
        ));
    };

    let prompt = format!(
        r#"Analyze the following database schemas and suggest logical business domain clusters.
Group tables from different datasources that likely belong to the same business domain.
Respond with ONLY valid JSON array:
[
  {{
    "cluster_name": "string",
    "description": "string",
    "datasource_ids": ["ds1", "ds2"],
    "table_names": ["table1", "table2"]
  }}
]

Schemas:
{schema_summary}"#
    );

    let request = api::MessageRequest {
        model: llm_model,
        max_tokens: 2048,
        messages: vec![api::InputMessage {
            role: "user".to_string(),
            content: vec![api::InputContentBlock::Text { text: prompt }],
        }],
        system: Some(
            "You are a data modeling expert. Respond with ONLY valid JSON array.".to_string(),
        ),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.3),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = llm_client.send_message(&request).await.ok();

    let text = response
        .and_then(|r| {
            r.content.iter().find_map(|b| match b {
                api::OutputContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default();

    let suggestions: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();

    // BUG-FIX: Optionally auto-save suggestions to nl2sql_cross_domain_clusters.
    let mut saved_ids: Vec<i64> = Vec::new();
    if req.auto_save && !suggestions.is_empty() {
        let fallback_ds_ids = serde_json::Value::Array(vec![]);
        for suggestion in &suggestions {
            let cluster_name = suggestion
                .get("cluster_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unnamed Cluster");
            let description = suggestion.get("description").and_then(|v| v.as_str());
            let ds_ids_ref = suggestion.get("datasource_ids").unwrap_or(&fallback_ds_ids);
            let table_names_json = suggestion
                .get("table_names")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    serde_json::json!(arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(String::from)
                        .collect::<Vec<_>>())
                })
                .unwrap_or(serde_json::json!([]));

            let result = sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO nl2sql_cross_domain_clusters \
                 (tenant_id, cluster_name, datasource_ids, domain_ids, description, auto_discovered, created_by) \
                 VALUES (?, ?, ?, ?, ?, true, ?)",
            )
            .bind(tenant_id)
            .bind(cluster_name)
            .bind(ds_ids_ref)
            .bind(&table_names_json)
            .bind(description)
            .bind(&claims.sub)
            .execute(&state.db)
            .await;
            if let Ok(r) = result {
                saved_ids.push(r.last_insert_rowid() as i64);
            }
        }
    }

    Ok(Json(serde_json::json!({
        "suggestions": suggestions,
        "savedIds": saved_ids,
    })))
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-1: NL2SQL Analytics Dashboard API
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnalyticsOverview {
    total_queries_30d: i64,
    success_rate_30d: f64,
    avg_route_confidence: f64,
    avg_execution_ms: f64,
    total_datasources: i64,
    total_tables_indexed: i64,
    avg_semantic_coverage: f64,
    total_conversations_30d: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnalyticsRouting {
    confidence_distribution: Vec<serde_json::Value>,
    method_distribution: Vec<serde_json::Value>,
    top_routed_tables: Vec<serde_json::Value>,
    clarification_rate: f64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DatasourceCoverage {
    datasource_id: String,
    datasource_name: String,
    total_tables: i64,
    indexed_tables: i64,
    total_columns: i64,
    indexed_columns: i64,
    coverage_pct: f64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnalyticsSemanticCoverage {
    datasources: Vec<DatasourceCoverage>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DailyTrend {
    date: String,
    queries: i64,
    success_rate: f64,
    avg_confidence: f64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnalyticsTrends {
    daily: Vec<DailyTrend>,
}

// GET /nl2sql/analytics/overview
