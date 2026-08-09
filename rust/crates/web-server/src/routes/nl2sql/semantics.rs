use super::{map_update_err, validate_data_source_access, *};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn aggregate_embedding_usage(usages: &[api::Usage]) -> Option<api::Usage> {
    if usages.is_empty() {
        return None;
    }
    let mut agg = api::Usage::default();
    for usage in usages {
        agg.input_tokens = agg.input_tokens.saturating_add(usage.input_tokens);
        agg.output_tokens = agg.output_tokens.saturating_add(usage.output_tokens);
        agg.cache_creation_input_tokens = agg
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
        agg.cache_read_input_tokens = agg
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
    }
    Some(agg)
}

async fn persist_embedding_usage(
    usage_writer: Option<Arc<crate::routes::chat::TokenUsageWriter>>,
    tenant_id: &str,
    user_id: &str,
    datasource_id: &str,
    request_id: Option<&str>,
    model: &str,
    api_key_id: Option<String>,
    usage: Option<api::Usage>,
) {
    let (Some(writer), Some(usage)) = (usage_writer, usage) else {
        return;
    };
    let total_tokens = usage.total_tokens();
    if total_tokens == 0 {
        return;
    }
    let record = crate::routes::chat::TokenUsageRecord {
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        session_id: format!("nl2sql:semantics:{datasource_id}"),
        request_id: request_id.map(std::string::ToString::to_string),
        model: model.to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        total_tokens,
        estimated_cost_usd: usage.estimated_cost_usd(model).total_cost_usd(),
        api_key_id,
        provider: "nl2sql_embedding".to_string(),
        created_at: chrono::Utc::now(),
    };
    if let Err(e) = writer.write(&record).await {
        tracing::warn!(
            tenant_id = %tenant_id,
            user_id = %user_id,
            datasource_id = %datasource_id,
            error = %e,
            "failed to persist NL2SQL embedding token usage"
        );
    }
}

/// GET /api/v1/nl2sql/embedding/config — returns current embedding configuration for the tenant.
pub(crate) async fn get_embedding_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<EmbeddingConfigResponse>> {
    let resolved =
        crate::nl2sql::embedding_profiles::reconcile_tenant_profiles(&state.db, &claims.tenant_id)
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to resolve embedding profiles: {error}"))
            })?;
    let profile_rows =
        crate::nl2sql::embedding_profiles::profile_status_rows(&state.db, &claims.tenant_id)
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to load embedding profile status: {error}"))
            })?;
    let selected = resolved.api.as_ref().unwrap_or(&resolved.local);
    Ok(Json(EmbeddingConfigResponse {
        available: true,
        model: Some(selected.config.model.clone()),
        base_url: selected.config.base_url.clone(),
        configured_via: selected.config.configured_via,
        dimensions: selected.config.dimensions,
        api_configured: resolved.api.is_some(),
        local_model: resolved.local.config.model,
        profiles: profile_rows,
    }))
}

/// GET /api/v1/nl2sql/semantics/:datasource_id — get column semantics for UI display.
pub(crate) async fn get_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<GetSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                datasource_id = %datasource_id,
                error = %e,
                "nl2sql get_semantics: failed to load datasource tenant"
            );
            AppError::from(e)
        })?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            datasource_id = %datasource_id,
            datasource_tenant = ?ds_tenant,
            "nl2sql get_semantics: forbidden datasource access"
        );
        return Err(AppError::Forbidden);
    }

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;

    let embed_store = state.nl2sql_embedding_store.as_ref().ok_or_else(|| {
        tracing::error!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            datasource_id = %datasource_id,
            "nl2sql get_semantics: embedding store not initialized"
        );
        AppError::Internal("embedding store not initialized".into())
    })?;

    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| {
        tracing::error!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            datasource_id = %datasource_id,
            error = %e,
            "nl2sql get_semantics: resolve_chat_config failed"
        );
        AppError::Internal(e)
    })?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let columns_internal = describer
        .get_column_semantics(&datasource_id)
        .await
        .map_err(|e| {
            tracing::error!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                datasource_id = %datasource_id,
                error = %e,
                "nl2sql get_semantics: get_column_semantics failed"
            );
            AppError::Internal(format!("get semantics failed: {e}"))
        })?;

    let columns = columns_internal
        .into_iter()
        .map(|c| ColumnSemanticsInfo {
            table_name: c.table_name,
            column_name: c.column_name,
            ai_description: c.semantic_description,
            is_indexed: c.is_indexed,
            version: c.version,
        })
        .collect();

    Ok(Json(GetSemanticsResponse { columns }))
}

/// PATCH /api/v1/nl2sql/semantics/:datasource_id — update user description for a column.
pub(crate) async fn update_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<UpdateSemanticsRequest>,
) -> Result<Json<UpdateSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }
    require_admin(&claims)?;

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;

    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;

    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let outcome = describer
        .update_user_description(
            &claims.tenant_id,
            &datasource_id,
            &req.table_name,
            &req.column_name,
            &req.user_description,
        )
        .await
        .map_err(map_update_err)?;

    Ok(Json(UpdateSemanticsResponse {
        success: true,
        indexed: outcome.indexed,
        index_error: outcome.index_error,
    }))
}

// ── Table-level semantics ─────────────────────────────────────────────────────

/// GET /api/v1/nl2sql/semantics/:datasource_id/tables — get all table-level semantics.
pub(crate) async fn get_all_table_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<GetAllTableSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let tables = describer
        .get_table_semantics(&datasource_id)
        .await
        .map_err(|e| AppError::Internal(format!("get table semantics failed: {e}")))?;

    let _has_indexed = tables
        .iter()
        .any(|t| t.columns.iter().any(|c| c.is_indexed));

    Ok(Json(GetAllTableSemanticsResponse {
        tables: tables
            .into_iter()
            .map(|t| TableSemanticsResponse {
                table_name: t.table_name,
                ai_description: t.ai_description,
                embedding_model: t.embedding_model,
                is_indexed: t.columns.iter().any(|c| c.is_indexed),
                version: t.version,
            })
            .collect(),
    }))
}

/// GET /api/v1/nl2sql/semantics/:datasource_id/tables/:table_name
pub(crate) async fn get_table_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, table_name)): Path<(String, String)>,
) -> Result<Json<TableSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let tables = describer
        .get_table_semantics(&datasource_id)
        .await
        .map_err(|e| AppError::Internal(format!("get table semantics failed: {e}")))?;

    let table = tables
        .into_iter()
        .find(|t| t.table_name == table_name)
        .ok_or_else(|| AppError::NotFound("table semantics not found".into()))?;

    Ok(Json(TableSemanticsResponse {
        table_name: table.table_name,
        ai_description: table.ai_description,
        embedding_model: table.embedding_model,
        is_indexed: table.columns.iter().any(|c| c.is_indexed),
        version: table.version,
    }))
}

/// PATCH /api/v1/nl2sql/semantics/:datasource_id/tables/:table_name
pub(crate) async fn update_table_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, table_name)): Path<(String, String)>,
    Json(req): Json<UpdateTableSemanticsRequest>,
) -> Result<Json<UpdateSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let outcome = describer
        .update_table_description(
            &claims.tenant_id,
            &datasource_id,
            &table_name,
            &req.user_description,
        )
        .await
        .map_err(map_update_err)?;

    Ok(Json(UpdateSemanticsResponse {
        success: true,
        indexed: outcome.indexed,
        index_error: outcome.index_error,
    }))
}

// ── Datasource-level semantics ────────────────────────────────────────────────

/// GET /api/v1/nl2sql/semantics/:datasource_id/datasource
pub(crate) async fn get_datasource_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<DatasourceSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let semantics = describer
        .get_datasource_semantics(&datasource_id)
        .await
        .map_err(|e| AppError::Internal(format!("get datasource semantics failed: {e}")))?;

    match semantics {
        Some(s) => Ok(Json(DatasourceSemanticsResponse {
            ai_description: s.ai_description,
            user_description: s.user_description,
            embedding_model: s.embedding_model,
            is_indexed: s.is_indexed,
            version: s.version,
        })),
        None => Ok(Json(DatasourceSemanticsResponse {
            ai_description: None,
            user_description: None,
            embedding_model: None,
            is_indexed: false,
            version: 0,
        })),
    }
}

/// PATCH /api/v1/nl2sql/semantics/:datasource_id/datasource — update datasource-level user description.
pub(crate) async fn update_datasource_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<crate::routes::nl2sql::UpdateSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }

    let user_description = req
        .get("user_description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let outcome = describer
        .update_datasource_description(&datasource_id, &user_description)
        .await
        .map_err(map_update_err)?;

    Ok(Json(crate::routes::nl2sql::UpdateSemanticsResponse {
        success: outcome.updated,
        indexed: outcome.indexed,
        index_error: outcome.index_error,
    }))
}

/// POST /api/v1/nl2sql/semantics/:datasource_id/refresh — regenerate all embeddings for a datasource.
pub(crate) async fn refresh_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<RefreshSemanticsResponse>> {
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    // Resolve per-tenant embedding config from api_keys, fall back to env vars.
    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_model_for_usage = embed_cfg
        .as_ref()
        .map(|cfg| cfg.model.clone())
        .unwrap_or_else(|| "text-embedding-3-small".to_string());
    let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());

    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;

    // Resolve per-tenant chat LLM config (DB key with failover, then env fallback).
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let result = describer
        .refresh_datasource(&claims.tenant_id, &datasource_id)
        .await
        .map_err(|e| AppError::Internal(format!("refresh failed: {e}")))?;
    let ann_registry = Arc::clone(embed_store);
    if let Err(error) =
        tokio::task::spawn_blocking(move || ann_registry.persist_ann_snapshots_if_dirty())
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result.map_err(|error| error.to_string()))
    {
        tracing::warn!(datasource_id, error, "ANN snapshot refresh failed after semantic indexing; brute-force retrieval remains available");
    }
    persist_embedding_usage(
        state.usage_writer.clone(),
        &claims.tenant_id,
        &claims.sub,
        &datasource_id,
        None,
        &embed_model_for_usage,
        embed_api_key_for_usage,
        aggregate_embedding_usage(&result.embedding_usage),
    )
    .await;

    Ok(Json(RefreshSemanticsResponse {
        tables_processed: result.tables_processed,
        columns_processed: result.columns_processed,
        failed_tables: result.failed_tables,
    }))
}

// ── Async Refresh Task ────────────────────────────────────────────────────────

/// POST /api/v1/nl2sql/semantics/:datasource_id/refresh-async
/// Kicks off a background refresh and returns a task_id immediately.
///
/// Request body is optional. Supply `{ "tables": ["t1", "t2"] }` to refresh
/// only a subset — used by the frontend's "retry failed tables" button.
pub(crate) async fn refresh_semantics_async(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    body: Option<Json<RefreshAsyncRequest>>,
) -> Result<Json<RefreshTaskCreatedResponse>> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    // Check for an in-flight refresh on this datasource before enqueuing
    // a new task. Two concurrent `refresh-async` calls on the same ds
    // would race to upsert into `nl2sql_*_semantics` and produce
    // duplicated LLM calls with an indeterminate final state.
    let in_flight: Option<String> = sqlx::query_scalar(
        "SELECT task_id FROM nl2sql_refresh_tasks \
         WHERE datasource_id = ? AND status IN ('pending', 'running') \
         LIMIT 1",
    )
    .bind(&datasource_id)
    .fetch_optional(&state.db)
    .await?;
    if let Some(existing) = in_flight {
        return Err(AppError::Conflict(format!(
            "a refresh is already in progress for this data source (task_id={existing})"
        )));
    }

    let task_id = uuid::Uuid::new_v4().to_string();

    // Count total tables to set `total_tables` in the task record.
    // Manual tables are excluded because `refresh_datasource` skips them,
    // so counting them would make the progress percentage plateau before
    // reaching 100%.
    // When a specific table list is provided (partial refresh), use that count.
    let total_tables: i32 = if let Some(ref tables) = req.tables {
        i32::try_from(tables.len()).unwrap_or(1)
    } else {
        let schema_info: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT schema_info FROM data_sources WHERE id = ?")
                .bind(&datasource_id)
                .fetch_optional(&state.db)
                .await?
                .flatten();
        let tables = schema_info
            .as_ref()
            .map(crate::nl2sql::schema_diff::extract_schema_tables)
            .unwrap_or_default();
        i32::try_from(
            tables
                .iter()
                .filter(|t| {
                    !t.get("is_manual")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .count(),
        )
        .unwrap_or(i32::MAX)
    };

    sqlx::query(
        "INSERT INTO nl2sql_refresh_tasks \
         (task_id, tenant_id, trigger_source, datasource_id, status, total_tables) \
         VALUES (?, ?, 'user', ?, 'pending', ?)",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(total_tables)
    .execute(&state.db)
    .await?;

    // Clone everything needed for the background task
    let tenant_id = claims.tenant_id.clone();
    let datasource_id_inner = datasource_id.clone();
    let task_id_inner = task_id.clone();
    let db = state.db.clone();
    let embed_store = state.nl2sql_embedding_store.clone();
    let default_model = state.default_model.clone();
    let config_registry = state.config_registry.clone();
    let usage_writer = state.usage_writer.clone();
    let user_id = claims.sub.clone();
    let only_tables = req.tables.clone();

    tokio::spawn(async move {
        // Take the per-datasource advisory lock to serialise against the
        // periodic scheduler. If we can't get it quickly, surface a
        // timeout so the task row doesn't stay "pending" forever when the
        // scheduler's cycle is unexpectedly long-running.
        let lock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let _lock = loop {
            match crate::nl2sql::refresh_lock::RefreshLock::try_acquire(&db, &datasource_id_inner)
                .await
            {
                Ok(Some(guard)) => break guard,
                Ok(None) => {
                    if std::time::Instant::now() >= lock_deadline {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                             error_message = 'timed out waiting for another refresh to finish (60s); try again later' \
                             WHERE task_id = ?",
                        )
                        .bind(&task_id_inner)
                        .execute(&db)
                        .await;
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    let _ = sqlx::query(
                        "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                         error_message = ? WHERE task_id = ?",
                    )
                    .bind(format!("failed to acquire refresh lock: {e}"))
                    .bind(&task_id_inner)
                    .execute(&db)
                    .await;
                    return;
                }
            }
        };

        // Mark task as running
        let _ = sqlx::query(
            "UPDATE nl2sql_refresh_tasks \
             SET status = 'running', updated_at = CURRENT_TIMESTAMP WHERE task_id = ?",
        )
        .bind(&task_id_inner)
        .execute(&db)
        .await;

        let embed_cfg =
            crate::nl2sql::resolve_embedding_config(&db, &tenant_id, Some("nl2sql")).await;
        let embed_model_for_usage = embed_cfg
            .as_ref()
            .map(|cfg| cfg.model.clone())
            .unwrap_or_else(|| "text-embedding-3-small".to_string());
        let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());
        let embed_store = match embed_store {
            Some(s) => s,
            None => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = 'embedding store not initialized' WHERE task_id = ?",
                )
                .bind(&task_id_inner)
                .execute(&db)
                .await;
                return;
            }
        };

        let chat_cfg = match config_registry.as_ref() {
            Some(registry) => {
                match crate::nl2sql::resolve_chat_config(
                    registry,
                    &tenant_id,
                    &tenant_id,
                    &default_model,
                    Some("nl2sql"),
                )
                .await
                {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                             error_message = ? WHERE task_id = ?",
                        )
                        .bind(format!("failed to resolve chat config: {}", e))
                        .bind(&task_id_inner)
                        .execute(&db)
                        .await;
                        return;
                    }
                }
            }
            None => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = 'config registry not available' WHERE task_id = ?",
                )
                .bind(&task_id_inner)
                .execute(&db)
                .await;
                return;
            }
        };

        let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
            db.clone(),
            std::sync::Arc::clone(&embed_store),
            embed_cfg,
            Some(chat_cfg),
        );

        // Push live progress into `nl2sql_refresh_tasks` as tables finish.
        struct DbProgress {
            db: sqlx::SqlitePool,
            task_id: String,
        }
        #[async_trait::async_trait]
        impl crate::nl2sql::schema_describer::ProgressReporter for DbProgress {
            async fn report(&self, percent: u32, processed_tables: u32) {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks \
                     SET progress = ?, processed_tables = ?, updated_at = CURRENT_TIMESTAMP \
                     WHERE task_id = ?",
                )
                .bind(percent)
                .bind(processed_tables)
                .bind(&self.task_id)
                .execute(&self.db)
                .await;
            }
        }

        let reporter = DbProgress {
            db: db.clone(),
            task_id: task_id_inner.clone(),
        };

        let result = match only_tables {
            Some(ref list) if !list.is_empty() => {
                describer
                    .refresh_tables(&tenant_id, &datasource_id_inner, list, reporter)
                    .await
            }
            _ => {
                describer
                    .refresh_datasource_with_progress(&tenant_id, &datasource_id_inner, reporter)
                    .await
            }
        };

        match result {
            Ok(r) => {
                let ann_registry = Arc::clone(&embed_store);
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    ann_registry.persist_ann_snapshots_if_dirty()
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()))
                {
                    tracing::warn!(datasource_id = %datasource_id_inner, error, "ANN snapshot refresh failed after async semantic indexing; brute-force retrieval remains available");
                }
                persist_embedding_usage(
                    usage_writer.clone(),
                    &tenant_id,
                    &user_id,
                    &datasource_id_inner,
                    Some(&task_id_inner),
                    &embed_model_for_usage,
                    embed_api_key_for_usage,
                    aggregate_embedding_usage(&r.embedding_usage),
                )
                .await;
                // Partial-success policy: if every table failed we mark
                // the task failed, otherwise we record it as completed
                // and attach the failed table list for operators to act on.
                let failed_json = if r.failed_tables.is_empty() {
                    None
                } else {
                    serde_json::to_value(
                        r.failed_tables
                            .iter()
                            .map(|(name, err)| serde_json::json!({ "table": name, "error": err }))
                            .collect::<Vec<_>>(),
                    )
                    .ok()
                };

                let all_failed = r.tables_processed == 0 && !r.failed_tables.is_empty();
                let status = if all_failed { "failed" } else { "completed" };

                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks \
                     SET status = ?, progress = 100, \
                         processed_tables = ?, failed_tables = ?, \
                         updated_at = CURRENT_TIMESTAMP, completed_at = CURRENT_TIMESTAMP \
                     WHERE task_id = ?",
                )
                .bind(status)
                .bind(i32::try_from(r.tables_processed).unwrap_or(i32::MAX))
                .bind(failed_json)
                .bind(&task_id_inner)
                .execute(&db)
                .await;
            }
            Err(e) => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = ?, updated_at = CURRENT_TIMESTAMP, \
                     completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                )
                .bind(e.to_string())
                .bind(&task_id_inner)
                .execute(&db)
                .await;
            }
        }
    });

    Ok(Json(RefreshTaskCreatedResponse {
        task_id,
        status: "pending".to_string(),
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct RefreshTaskCreatedResponse {
    pub task_id: String,
    pub status: String,
}

/// Body for `POST /nl2sql/semantics/:id/refresh-async`. All fields optional:
/// an empty body refreshes the whole datasource.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RefreshAsyncRequest {
    /// When set, only these tables are re-indexed. Used by the frontend's
    /// "retry failed tables" workflow.
    #[serde(default)]
    pub tables: Option<Vec<String>>,
}

/// GET /api/v1/nl2sql/semantics-tasks/:task_id
/// Returns the current status and progress of an async refresh task.
pub(crate) async fn get_refresh_task_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<RefreshTaskStatusResponse>> {
    type RefreshTaskRow = (
        String,
        String,
        String,
        i64,
        i64,
        Option<String>,
        Option<serde_json::Value>,
        Option<String>,
    );
    let row: Option<RefreshTaskRow> = sqlx::query_as(
        "SELECT task_id, datasource_id, status, \
         CAST(progress AS INTEGER) AS progress, \
         CAST(processed_tables AS INTEGER) AS processed_tables, \
         error_message, failed_tables, \
         strftime('%Y-%m-%dT%H:%M:%SZ', completed_at) AS completed_at \
         FROM nl2sql_refresh_tasks \
         WHERE task_id = ? AND tenant_id = ?",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some((
            task_id,
            datasource_id,
            mut status,
            progress,
            processed_tables,
            mut error_message,
            failed_tables,
            completed_at,
        )) => {
            let is_admin = claims.role == "admin" || claims.role == "superadmin";
            let visible: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM data_sources \
                 WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL \
                   AND (user_id IS NULL OR user_id = ? OR ?)",
            )
            .bind(&datasource_id)
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .bind(is_admin)
            .fetch_optional(&state.db)
            .await?;
            if visible.is_none() {
                return Err(AppError::Forbidden);
            }
            let progress = u32::try_from(progress).unwrap_or(u32::MAX);
            let processed_tables = u32::try_from(processed_tables).unwrap_or(u32::MAX);
            if status == "running" || status == "pending" {
                let stale: bool = sqlx::query_scalar(
                    "SELECT updated_at < datetime(CURRENT_TIMESTAMP, '-30 minutes') \
                     FROM nl2sql_refresh_tasks WHERE task_id = ?",
                )
                .bind(&task_id)
                .fetch_one(&state.db)
                .await?;

                if stale {
                    tracing::warn!(task_id = %task_id, "stuck task detected (no progress for 30 minutes), marking as failed");
                    sqlx::query(
                        "UPDATE nl2sql_refresh_tasks SET status='failed', \
                         error_message='task timed out (no progress for 30 minutes)', \
                         updated_at = CURRENT_TIMESTAMP, completed_at = CURRENT_TIMESTAMP \
                         WHERE task_id = ?",
                    )
                    .bind(&task_id)
                    .execute(&state.db)
                    .await?;
                    status = "failed".to_string();
                    error_message = Some("task timed out (no progress for 30 minutes)".to_string());
                }
            }
            Ok(Json(RefreshTaskStatusResponse {
                task_id,
                datasource_id,
                status,
                progress,
                processed_tables,
                error_message,
                failed_tables,
                completed_at,
            }))
        }
        None => Err(AppError::NotFound("refresh task not found".into())),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListRefreshTasksQuery {
    pub active_only: Option<bool>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RefreshTaskListItem {
    pub task_id: String,
    pub datasource_id: String,
    pub datasource_name: String,
    pub trigger_source: String,
    pub status: String,
    pub progress: u32,
    pub total_tables: u32,
    pub processed_tables: u32,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RefreshTaskListResponse {
    pub items: Vec<RefreshTaskListItem>,
}

/// Lists durable schema/semantic indexing tasks visible to the current user.
/// This is intentionally backed by SQLite rather than browser state so users
/// can leave the page, refresh it, and still reopen progress.
pub(crate) async fn list_refresh_tasks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ListRefreshTasksQuery>,
) -> Result<Json<RefreshTaskListResponse>> {
    type TaskRow = (
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
        i64,
        Option<String>,
        String,
        String,
        Option<String>,
    );
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    let active_only = params.active_only.unwrap_or(false);
    let limit = i64::from(params.limit.unwrap_or(50).clamp(1, 100));
    let rows: Vec<TaskRow> = sqlx::query_as(
        "SELECT t.task_id, t.datasource_id, ds.name, t.trigger_source, t.status, \
                CAST(t.progress AS INTEGER), CAST(t.total_tables AS INTEGER), \
                CAST(t.processed_tables AS INTEGER), t.error_message, \
                strftime('%Y-%m-%dT%H:%M:%SZ', t.created_at), \
                strftime('%Y-%m-%dT%H:%M:%SZ', t.updated_at), \
                strftime('%Y-%m-%dT%H:%M:%SZ', t.completed_at) \
         FROM nl2sql_refresh_tasks t \
         JOIN data_sources ds ON ds.id = t.datasource_id AND ds.tenant_id = t.tenant_id \
         WHERE t.tenant_id = ? AND t.deleted_at IS NULL AND ds.deleted_at IS NULL \
           AND (ds.user_id IS NULL OR ds.user_id = ? OR ?) \
           AND (? = 0 OR t.status IN ('pending', 'running')) \
         ORDER BY CASE WHEN t.status IN ('pending', 'running') THEN 0 ELSE 1 END, \
                  t.updated_at DESC LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(is_admin)
    .bind(active_only)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    let items = rows
        .into_iter()
        .map(
            |(
                task_id,
                datasource_id,
                datasource_name,
                trigger_source,
                status,
                progress,
                total_tables,
                processed_tables,
                error_message,
                created_at,
                updated_at,
                completed_at,
            )| RefreshTaskListItem {
                task_id,
                datasource_id,
                datasource_name,
                trigger_source,
                status,
                progress: u32::try_from(progress).unwrap_or(0),
                total_tables: u32::try_from(total_tables).unwrap_or(0),
                processed_tables: u32::try_from(processed_tables).unwrap_or(0),
                error_message,
                created_at,
                updated_at,
                completed_at,
            },
        )
        .collect();
    Ok(Json(RefreshTaskListResponse { items }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReindexRequest {
    /// Optional: switch to a different embedding model during re-index.
    /// If omitted, re-index uses the currently configured model.
    pub new_model: Option<String>,
}

pub(crate) async fn reindex_datasource(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<ReindexRequest>,
) -> Result<Json<ReindexResponse>> {
    // Validate access
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let _embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::ValidationError("embedding store not initialized".to_string()))?;

    // Keep the active profile intact while the replacement is built. Profile
    // activation happens only after the complete replacement index is ready.

    // Step 1: Update the tracking columns
    if let Some(ref new_model) = req.new_model {
        sqlx::query(
            "UPDATE data_sources SET embedding_model = ?, embedding_dimensions = ?, embedding_needs_reindex = 0 WHERE id = ?",
        )
        .bind(new_model)
        .bind(dimensions_for_model(new_model) as i64)
        .bind(&datasource_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    } else {
        sqlx::query("UPDATE data_sources SET embedding_needs_reindex = 0 WHERE id = ?")
            .bind(&datasource_id)
            .execute(&state.db)
            .await
            .ok();
    }

    // Step 3: Clear AI-generated column semantics (user descriptions preserved)
    sqlx::query(
        "UPDATE nl2sql_table_semantics SET semantic_description = '', embedding_model = '', is_indexed = 0 WHERE datasource_id = ?",
    )
    .bind(&datasource_id)
    .execute(&state.db)
    .await
    .ok();

    sqlx::query(
        "UPDATE nl2sql_table_desc_semantics SET ai_description = '', embedding_model = '' WHERE datasource_id = ?",
    )
    .bind(&datasource_id)
    .execute(&state.db)
    .await
    .ok();

    // Step 4: Trigger full schema refresh
    let task_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO nl2sql_refresh_tasks (task_id, tenant_id, datasource_id, status, progress) VALUES (?, ?, ?, 'pending', 0)",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .execute(&state.db)
    .await
    .ok();

    // Spawn background refresh task
    let db = state.db.clone();
    let config_registry = state.config_registry.clone();
    let default_model = state.default_model.clone();
    let embed_store = state.nl2sql_embedding_store.clone();
    let tenant_id = claims.tenant_id.clone();
    let user_id = claims.sub.clone();
    let datasource_id2 = datasource_id.clone();
    let task_id2 = task_id.clone();
    let usage_writer = state.usage_writer.clone();

    tokio::spawn(async move {
        let embed_cfg =
            crate::nl2sql::resolve_embedding_config(&db, &tenant_id, Some("nl2sql")).await;
        let embed_model_for_usage = embed_cfg
            .as_ref()
            .map(|cfg| cfg.model.clone())
            .unwrap_or_else(|| "text-embedding-3-small".to_string());
        let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());

        let chat_cfg = match config_registry.as_ref() {
            Some(registry) => match crate::nl2sql::resolve_chat_config(
                registry.as_ref(),
                &tenant_id,
                &tenant_id,
                &default_model,
                Some("nl2sql"),
            )
            .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("reindex: failed to resolve chat config: {}", e);
                    sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'failed', error_message = ? WHERE task_id = ?")
                        .bind(e.to_string())
                        .bind(&task_id2)
                        .execute(&db)
                        .await
                        .ok();
                    return;
                }
            },
            None => {
                tracing::error!("reindex: config registry not available");
                sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = 'config registry not available' WHERE task_id = ?",
                )
                .bind(&task_id2)
                .execute(&db)
                .await
                .ok();
                return;
            }
        };

        let store = match embed_store.as_ref() {
            Some(s) => std::sync::Arc::clone(s),
            None => {
                tracing::error!("reindex: embedding store not available");
                sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'failed', error_message = 'embedding store not initialized' WHERE task_id = ?")
                    .bind(&task_id2)
                    .execute(&db)
                    .await
                    .ok();
                return;
            }
        };

        let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
            db.clone(),
            store,
            embed_cfg,
            Some(chat_cfg),
        );

        let result = describer
            .refresh_datasource(&tenant_id, &datasource_id2)
            .await;
        match result {
            Ok(r) => {
                persist_embedding_usage(
                    usage_writer.clone(),
                    &tenant_id,
                    &user_id,
                    &datasource_id2,
                    Some(&task_id2),
                    &embed_model_for_usage,
                    embed_api_key_for_usage,
                    aggregate_embedding_usage(&r.embedding_usage),
                )
                .await;
                tracing::info!(
                    "reindex completed: {} tables, {} columns",
                    r.tables_processed,
                    r.columns_processed
                );
                sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'completed', progress = 100, processed_tables = ? WHERE task_id = ?")
                    .bind(r.tables_processed as i32)
                    .bind(&task_id2)
                    .execute(&db)
                    .await
                    .ok();
            }
            Err(e) => {
                tracing::error!("reindex failed: {}", e);
                sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'failed', error_message = ? WHERE task_id = ?")
                    .bind(e.to_string())
                    .bind(&task_id2)
                    .execute(&db)
                    .await
                    .ok();
            }
        }
    });

    Ok(Json(ReindexResponse {
        status: "reindexing".to_string(),
        task_id: Some(task_id),
        message: "Re-index started. Use the task endpoint to monitor progress.".to_string(),
    }))
}

/// Returns the expected embedding dimensions for a model name.
fn dimensions_for_model(model: &str) -> usize {
    nl2sql_domain::config::dimensions_for_model(model)
}
