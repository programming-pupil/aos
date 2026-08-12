use super::auth::require_admin;
use super::{
    extract_schema_tables_and_fks, generate_sql, load_conversation_history,
    load_join_paths_for_datasource, load_manual_foreign_keys, now_ms, should_enable_qu,
    validate_data_source_access, ClarifyRequest, ClarifyResponse, ClarifyResponseData,
    CreateForeignKeyRequest, ForeignKeyListResponse, ForeignKeyPrompt, ForeignKeyResponse,
    InputContentBlock, InputMessage, MessageRequest, OutputContentBlock,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::Deserialize;
use sqlx::Row;

fn map_fk_write_error(e: sqlx::Error) -> AppError {
    if matches!(e, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
        AppError::Conflict("foreign key relationship already exists".into())
    } else {
        AppError::Database(e)
    }
}

/// GET /api/v1/nl2sql/foreign-keys/:datasource_id — list all manual FKs for a datasource.
pub(crate) async fn list_foreign_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<ForeignKeyListResponse>> {
    // Validate access and ensure the datasource belongs to the tenant.
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    )> = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ),
    >(
        "SELECT CAST(id AS TEXT) AS id, source_table, source_column, source_type, target_table, \
             target_column, target_type, created_by, updated_by, CAST(created_at AS TEXT) AS created_at \
             FROM nl2sql_foreign_keys \
             WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await?;

    let foreign_keys: Vec<ForeignKeyResponse> = rows
        .into_iter()
        .map(
            |(
                id,
                source_table,
                source_column,
                source_type,
                target_table,
                target_column,
                target_type,
                _created_by,
                updated_by,
                created_at,
            )| ForeignKeyResponse {
                id,
                datasource_id: datasource_id.clone(),
                source_table,
                source_column,
                source_type,
                target_table,
                target_column,
                target_type,
                updated_by,
                created_at,
            },
        )
        .collect();

    Ok(Json(ForeignKeyListResponse { foreign_keys }))
}

/// POST /api/v1/nl2sql/foreign-keys/:datasource_id — create a new manual FK definition.
pub(crate) async fn create_foreign_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<CreateForeignKeyRequest>,
) -> Result<Json<ForeignKeyResponse>> {
    require_admin(&claims)?;
    // Validate access and ensure the datasource belongs to the tenant.
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let insert_result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_foreign_keys \
         (tenant_id, datasource_id, source_table, source_column, source_type, \
          target_table, target_column, target_type, created_by, updated_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(&req.source_table)
    .bind(&req.source_column)
    .bind(&req.source_type)
    .bind(&req.target_table)
    .bind(&req.target_column)
    .bind(&req.target_type)
    .bind(&claims.sub)
    .bind(&claims.sub)
    .execute(&state.db)
    .await
    .map_err(map_fk_write_error)?;
    let inserted_id = insert_result.last_insert_rowid();

    // Fetch the created record to return the full response including created_at.
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    ) = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(id AS TEXT) AS id, source_table, source_column, source_type, target_table, \
             target_column, target_type, created_by, updated_by, CAST(created_at AS TEXT) AS created_at \
             FROM nl2sql_foreign_keys \
             WHERE id = ? AND tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(inserted_id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_one(&state.db)
    .await?;

    let (
        id,
        source_table,
        source_column,
        source_type,
        target_table,
        target_column,
        target_type,
        _created_by,
        updated_by,
        created_at,
    ) = row;

    Ok(Json(ForeignKeyResponse {
        id,
        datasource_id,
        source_table,
        source_column,
        source_type,
        target_table,
        target_column,
        target_type,
        updated_by,
        created_at,
    }))
}

/// PATCH /api/v1/nl2sql/foreign-keys/:datasource_id/:fk_id — update a manual FK.
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateForeignKeyRequest {
    pub source_table: Option<String>,
    pub source_column: Option<String>,
    pub source_type: Option<String>,
    pub target_table: Option<String>,
    pub target_column: Option<String>,
    pub target_type: Option<String>,
}

pub(crate) async fn update_foreign_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, fk_id)): Path<(String, String)>,
    Json(req): Json<UpdateForeignKeyRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let fk_id_num = fk_id
        .parse::<u64>()
        .map_err(|_| AppError::ValidationError("invalid foreign key id".into()))?;
    let _ = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let mut query = String::from(
        "UPDATE nl2sql_foreign_keys SET updated_at = CURRENT_TIMESTAMP, updated_by = ?",
    );
    let mut bindings: Vec<String> = vec![claims.sub.clone()];

    if let Some(ref v) = req.source_table {
        query.push_str(", source_table = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.source_column {
        query.push_str(", source_column = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.source_type {
        query.push_str(", source_type = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.target_table {
        query.push_str(", target_table = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.target_column {
        query.push_str(", target_column = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.target_type {
        query.push_str(", target_type = ?");
        bindings.push(v.clone());
    }

    query.push_str(" WHERE id = ? AND tenant_id = ? AND datasource_id = ?");

    let mut q = sqlx::query::<sqlx::Sqlite>(&query);
    for binding in &bindings {
        q = q.bind(binding);
    }
    q = q
        .bind(crate::sqlite_i64(fk_id_num))
        .bind(&claims.tenant_id)
        .bind(&datasource_id);

    let result = q.execute(&state.db).await.map_err(map_fk_write_error)?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Foreign key not found".into()));
    }

    Ok(Json(
        serde_json::json!({ "id": fk_id_num.to_string(), "updated_by": &claims.sub }),
    ))
}

/// DELETE /api/v1/nl2sql/foreign-keys/:datasource_id/:fk_id — delete a manual FK.
pub(crate) async fn delete_foreign_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, fk_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    let fk_id_num = fk_id
        .parse::<u64>()
        .map_err(|_| AppError::ValidationError("invalid foreign key id".into()))?;
    // Validate access and ensure the datasource belongs to the tenant.
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let result = sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_foreign_keys \
         WHERE id = ? AND tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(crate::sqlite_i64(fk_id_num))
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("foreign key not found".into()));
    }

    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// Map the typed update error from [`SchemaDescriber`] into an HTTP error.
/// Semantic records are created by `refresh_datasource`; a missing row
/// means the client is editing something that was never indexed.
fn map_update_err(e: crate::nl2sql::schema_describer::UpdateDescriptionError) -> AppError {
    use crate::nl2sql::schema_describer::UpdateDescriptionError as E;
    match e {
        E::NotFound => AppError::NotFound(
            "semantic record not found; run a schema refresh before editing".into(),
        ),
        E::UpdateFailed(err) => AppError::Internal(format!("update failed: {err}")),
        E::Database(err) => AppError::Internal(format!("database error: {err}")),
        E::Other(err) => AppError::Internal(err.to_string()),
    }
}

/// Generate a natural language explanation of the given SQL query.
/// The `language` parameter controls the output language ("zh-CN" or "en-US").
pub(crate) async fn explain_sql(
    state: &AppState,
    claims: &Claims,
    sql: &str,
    schema: &serde_json::Value,
    language: &str,
) -> anyhow::Result<String> {
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to resolve chat config: {}", e))?;

    let (system_prompt, lang_label) = if language == "zh-CN" {
        ("你是一位 SQL 专家。根据给定的数据库 schema 和 SQL 查询，用 1-2 句话简洁地解释这条查询的作用。直接返回解释文字，不需要任何格式或标记。".to_string(), "中文")
    } else {
        ("You are a SQL expert. Given a database schema and SQL query, explain what the query does in 1-2 sentences. Return your explanation as plain text with no formatting.".to_string(), "English")
    };

    let prompt = format!(
        "Schema:\n{}\n\nSQL:\n{}\n\nLanguage: {}",
        serde_json::to_string_pretty(schema).unwrap_or_default(),
        sql,
        lang_label
    );

    let request = MessageRequest {
        model: chat_cfg.model,
        max_tokens: 256,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt }],
        }],
        system: Some(system_prompt),
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

    let response = chat_cfg
        .client
        .send_message(&request)
        .await
        .map_err(|e| anyhow::anyhow!("LLM explanation call failed: {}", e))?;

    let text = response
        .content
        .iter()
        .find_map(|b| match b {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    Ok(text.trim().to_string())
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn clarify(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ClarifyRequest>,
) -> Result<Json<ClarifyResponse>> {
    let tenant_id = &claims.tenant_id;
    let user_id = &claims.sub;
    let session_id = &req.session_id;
    let ts = now_ms();

    // P3-1: Enforce maximum clarification turns.
    if let Some(ref ctx) = req.clarification_context {
        if ctx.turn > crate::nl2sql::max_clarification_turns() {
            return Ok(Json(ClarifyResponse {
                data: None,
                error: Some(
                    "Maximum clarification rounds reached. Please rephrase your question or pick a data source."
                        .to_string(),
                ),
                pending_clarification: None,
            }));
        }
    }

    let ClarifyRequest {
        session_id: _,
        conversation_id,
        question: _,
        clarification_context,
        selected_option,
        free_text,
        route_confidence,
        routing_method,
        semantic_context,
        ..
    } = &req;
    let mut route_confidence = route_confidence.map(|c| c.clamp(0.0, 1.0));
    let mut routing_method = routing_method
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let mut semantic_context = semantic_context.clone();

    let (data_source_id, question_text) = if let Some(opt) = selected_option {
        let ctx = clarification_context.as_ref().ok_or_else(|| {
            AppError::ValidationError(
                "clarification_context is required when selecting an option".into(),
            )
        })?;
        let chosen = ctx.options.get(opt.option_index).ok_or_else(|| {
            AppError::ValidationError(format!("invalid option index: {}", opt.option_index))
        })?;
        let enriched_question = format!(
            "{} (use table: {})",
            ctx.original_question, chosen.table_name
        );
        (chosen.data_source_id.clone(), enriched_question)
    } else if let Some(text) = free_text {
        if text.trim().is_empty() {
            return Err(AppError::ValidationError(
                "free_text cannot be empty".into(),
            ));
        }
        let ctx = clarification_context.as_ref().ok_or_else(|| {
            AppError::ValidationError(
                "clarification_context is required when providing free_text".into(),
            )
        })?;
        let enriched_question = format!("{}\n补充条件：{}", ctx.original_question, text.trim());
        let reroute = super::routing::route(
            State(state.clone()),
            Extension(claims.clone()),
            Json(super::routing::RouteRequest {
                question: enriched_question.clone(),
                data_source_id: None,
            }),
        )
        .await;

        match reroute {
            Ok(Json(super::routing::RouteResponse {
                routed: true,
                result: Some(result),
                error: _,
            })) if !result.data_source_id.trim().is_empty()
                && result.data_source_id != "multi-datasource" =>
            {
                tracing::info!(
                    tenant_id,
                    user_id,
                    session_id,
                    original_question_chars = ctx.original_question.chars().count(),
                    free_text_chars = text.trim().chars().count(),
                    datasource_id = %result.data_source_id,
                    confidence = result.confidence,
                    method = %result.method,
                    option_count = ctx.options.len(),
                    "legacy clarify free_text rerouted datasource"
                );
                route_confidence = Some(result.confidence.clamp(0.0, 1.0));
                routing_method = Some(format!("clarification_free_text:{}", result.method));
                semantic_context = serde_json::to_value(&result).ok();
                (result.data_source_id, enriched_question)
            }
            Ok(Json(super::routing::RouteResponse {
                result: Some(result),
                error,
                ..
            })) => {
                let mut options: Vec<crate::nl2sql::ClarificationOption> = result
                    .matched_tables
                    .into_iter()
                    .enumerate()
                    .take(5)
                    .map(|(idx, table)| crate::nl2sql::ClarificationOption {
                        option_index: idx,
                        data_source_id: table.data_source_id,
                        table_name: table.table_name,
                        column_name: table.best_column,
                        reason: "根据补充内容重新检索后的候选".to_string(),
                        sim_score: table.similarity_score,
                        business_meaning: table.column_description,
                    })
                    .collect();
                if options.is_empty() {
                    options = ctx.options.clone();
                }
                let new_ctx = crate::nl2sql::ClarificationContext {
                    original_question: enriched_question.clone(),
                    clarification_question: result.clarification_question.unwrap_or_else(|| {
                        error.unwrap_or_else(|| {
                            "补充后仍未能确定唯一数据源，请选择最匹配的数据源或继续补充。"
                                .to_string()
                        })
                    }),
                    options,
                    confirmed_requirements: ctx.confirmed_requirements.clone(),
                    missing_requirements: ctx.missing_requirements.clone(),
                    missing_requirement_reasons: ctx.missing_requirement_reasons.clone(),
                    clarification_history: ctx.clarification_history.clone(),
                    turn: ctx.turn.saturating_add(1),
                    conversation_id: ctx.conversation_id.clone(),
                };
                return Ok(Json(ClarifyResponse {
                    data: None,
                    pending_clarification: Some(new_ctx),
                    error: None,
                }));
            }
            Ok(Json(super::routing::RouteResponse { error, .. })) => {
                let new_ctx = crate::nl2sql::ClarificationContext {
                    original_question: enriched_question.clone(),
                    clarification_question: error.unwrap_or_else(|| {
                        "补充后仍未能确定唯一数据源，请选择最匹配的数据源或继续补充。".to_string()
                    }),
                    options: ctx.options.clone(),
                    confirmed_requirements: ctx.confirmed_requirements.clone(),
                    missing_requirements: ctx.missing_requirements.clone(),
                    missing_requirement_reasons: ctx.missing_requirement_reasons.clone(),
                    clarification_history: ctx.clarification_history.clone(),
                    turn: ctx.turn.saturating_add(1),
                    conversation_id: ctx.conversation_id.clone(),
                };
                return Ok(Json(ClarifyResponse {
                    data: None,
                    pending_clarification: Some(new_ctx),
                    error: None,
                }));
            }
            Err(e) => {
                tracing::warn!(
                    tenant_id,
                    user_id,
                    session_id,
                    original_question_chars = ctx.original_question.chars().count(),
                    free_text_chars = text.trim().chars().count(),
                    error = %e,
                    "legacy clarify free_text reroute failed"
                );
                let new_ctx = crate::nl2sql::ClarificationContext {
                    original_question: enriched_question.clone(),
                    clarification_question:
                        "补充后仍未能可靠确定数据源，请选择最匹配的数据源或继续补充。".to_string(),
                    options: ctx.options.clone(),
                    confirmed_requirements: ctx.confirmed_requirements.clone(),
                    missing_requirements: ctx.missing_requirements.clone(),
                    missing_requirement_reasons: ctx.missing_requirement_reasons.clone(),
                    clarification_history: ctx.clarification_history.clone(),
                    turn: ctx.turn.saturating_add(1),
                    conversation_id: ctx.conversation_id.clone(),
                };
                return Ok(Json(ClarifyResponse {
                    data: None,
                    pending_clarification: Some(new_ctx),
                    error: None,
                }));
            }
        }
    } else {
        return Err(AppError::ValidationError(
            "either selected_option or free_text must be provided".into(),
        ));
    };

    if let Some(ctx) = clarification_context {
        let ctx_record = super::chat::SessionMessageRecord {
            role: "clarification".to_string(),
            content: serde_json::json!(ctx),
            timestamp_ms: ts,
        };
        // B-01: Log (non-fatal) if session persistence fails.
        if let Err(e) = super::chat::append_message(
            &state.data_dir,
            tenant_id,
            user_id,
            session_id,
            &ctx_record,
        ) {
            tracing::warn!(error = %e, tenant_id, user_id, session_id, "failed to persist clarification context to session");
        }
    }

    let conv_id = conversation_id
        .as_ref()
        .filter(|id| !id.is_empty())
        .cloned()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let db_type =
        validate_data_source_access(&state, tenant_id, user_id, &claims.role, &data_source_id)
            .await?;

    if !matches!(
        db_type.as_str(),
        "mysql" | "tidb" | "postgres" | "clickhouse" | "presto" | "trino" | "mongodb"
    ) {
        return Err(AppError::ValidationError(format!(
            "NL2SQL is not supported for db_type: {db_type}. \
             Pick a supported data source (mysql, tidb, postgres, clickhouse, presto, trino, mongodb)."
        )));
    }

    let schema_info: serde_json::Value = {
        let row = sqlx::query::<sqlx::Sqlite>("SELECT schema_info FROM data_sources WHERE id = ?")
            .bind(&data_source_id)
            .fetch_optional(&state.db)
            .await?;
        match row {
            Some(r) => r
                .get::<Option<serde_json::Value>, _>("schema_info")
                .unwrap_or(serde_json::json!({"tables": [], "foreign_keys": []})),
            None => return Err(AppError::NotFound("data source not found".into())),
        }
    };

    let (schema_tables, foreign_keys) = extract_schema_tables_and_fks(&schema_info);
    let mut foreign_key_prompts: Vec<ForeignKeyPrompt> = foreign_keys
        .into_iter()
        .map(|fk| ForeignKeyPrompt {
            source_table: fk.source_table,
            source_column: fk.source_column,
            source_type: fk.source_column_type,
            target_table: fk.target_table,
            target_column: fk.target_column,
            target_type: fk.target_column_type,
        })
        .collect();

    let manual_fks = load_manual_foreign_keys(&state.db, tenant_id, &data_source_id).await;
    foreign_key_prompts.extend(manual_fks);

    let history = load_conversation_history(&state.db, tenant_id, &conv_id, 8).await;

    // ── Query Understanding for agent execute ───────────────────────────────────
    let qu_result: Option<crate::nl2sql::query_understanding::QueryUnderstandingResult> =
        if should_enable_qu() {
            let chat_cfg = match crate::nl2sql::resolve_chat_config(
                state.config_registry(),
                tenant_id,
                user_id,
                &state.default_model,
                Some("nl2sql"),
            )
            .await
            {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "agent_execute QU: failed to resolve chat config, skipping");
                    None
                }
            };
            if let Some(cfg) = chat_cfg {
                let qu = crate::nl2sql::query_understanding::QueryUnderstanding::new(
                    state.db.clone(),
                    cfg,
                );
                let schema_for_qu = serde_json::json!(schema_tables);
                match qu
                    .understand(&question_text, &data_source_id, tenant_id, &schema_for_qu)
                    .await
                {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "agent_execute QU: understand() failed, skipping");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

    // Load pre-computed JOIN paths for multi-table queries.
    let join_paths = load_join_paths_for_datasource(&state.db, &data_source_id).await;

    // P1-2: Load business metrics for SQL generation prompt injection.
    let metrics: Vec<(String, String, Option<String>)> = {
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as::<sqlx::Sqlite, _>(
            "SELECT metric_name, expression, filter_conditions FROM nl2sql_metrics \
             WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(&data_source_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        rows.into_iter()
            .map(|(name, expr, filters)| {
                let filter_str = filters;
                (name, expr, filter_str)
            })
            .collect()
    };

    let planning_start = std::time::Instant::now();
    let sql_result = generate_sql(
        &state,
        &claims,
        Some(&data_source_id),
        &question_text,
        &schema_tables,
        &foreign_key_prompts,
        &join_paths,
        history,
        req.clarification_context.as_ref(),
        qu_result.as_ref(),
        &db_type,
        false, // P1-4: agent path uses full schema
        &metrics
            .iter()
            .map(|(n, e, f)| (n.clone(), e.clone(), f.as_deref()))
            .collect::<Vec<_>>(),
        &[],
        &[],
        None,
        None,
        true,
    )
    .await;

    let planning_ms = planning_start.elapsed().as_millis() as i64;
    let query_id = uuid::Uuid::new_v4().to_string();

    let (sql, _err): (Option<String>, Option<String>) = match &sql_result {
        Ok(r) => (Some(r.sql.clone()), None),
        Err(e) => {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO nl2sql_queries \
                 (id, tenant_id, user_id, data_source_id, conversation_id, question, \
                  generated_sql, executed, error_message, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, 0, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&query_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(&data_source_id)
            .bind(&conv_id)
            .bind(&question_text)
            .bind(e.to_string())
            .bind(planning_ms)
            .bind(route_confidence)
            .bind(routing_method.as_deref())
            .bind(semantic_context.clone())
            .bind(super::applied_rules_json_value(&[]))
            .execute(&state.db)
            .await?;

            let summary_version = super::fetch_summary_version_i32(&state.db, &conv_id).await;

            let resp_data = ClarifyResponseData {
                data_source_id: data_source_id.clone(),
                question: question_text.clone(),
                sql: None,
                explanation: None,
                error: Some(e.to_string()),
                query_id: query_id.clone(),
                conversation_id: Some(conv_id.clone()),
                clarification_context: clarification_context.clone(),
                fallback_mode: None,
                summary_version,
                applied_rules: Vec::new(),
            };
            let record = super::chat::SessionMessageRecord {
                role: "assistant".to_string(),
                content: serde_json::json!(&resp_data),
                timestamp_ms: ts + 1,
            };
            // B-01: Log (non-fatal) if session persistence fails.
            if let Err(e) = super::chat::append_message(
                &state.data_dir,
                tenant_id,
                user_id,
                session_id,
                &record,
            ) {
                tracing::warn!(error = %e, tenant_id, user_id, session_id, "failed to persist assistant clarification response to session");
            }
            return Ok(Json(ClarifyResponse {
                data: Some(resp_data),
                pending_clarification: None,
                error: None,
            }));
        }
    };

    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_queries \
         (id, tenant_id, user_id, data_source_id, conversation_id, question, \
          generated_sql, executed, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?)",
    )
    .bind(&query_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&data_source_id)
    .bind(&conv_id)
    .bind(&question_text)
    .bind(&sql)
    .bind(planning_ms)
    .bind(route_confidence)
    .bind(routing_method.as_deref())
    .bind(semantic_context.clone())
    .bind(super::applied_rules_json_value(&[]))
    .execute(&state.db)
    .await?;

    let explanation = match explain_sql(
        &state,
        &claims,
        sql.as_deref().unwrap_or(""),
        &schema_info,
        "en-US",
    )
    .await
    {
        Ok(e) if !e.is_empty() => Some(e),
        _ => None,
    };

    let summary_version = super::fetch_summary_version_i32(&state.db, &conv_id).await;

    let resp_data = ClarifyResponseData {
        data_source_id: data_source_id.clone(),
        question: question_text.clone(),
        sql: sql.clone(),
        explanation: explanation.clone(),
        error: None,
        query_id: query_id.clone(),
        conversation_id: Some(conv_id.clone()),
        clarification_context: clarification_context.clone(),
        fallback_mode: None,
        summary_version,
        applied_rules: Vec::new(),
    };

    let record = super::chat::SessionMessageRecord {
        role: "assistant".to_string(),
        content: serde_json::json!(&resp_data),
        timestamp_ms: ts + 1,
    };
    // B-01: Log (non-fatal) if session persistence fails.
    if let Err(e) =
        super::chat::append_message(&state.data_dir, tenant_id, user_id, session_id, &record)
    {
        tracing::warn!(error = %e, tenant_id, user_id, session_id, "failed to persist final assistant response to session");
    }
    append_clarification_closed(&state, tenant_id, user_id, session_id, ts + 2);

    Ok(Json(ClarifyResponse {
        data: Some(ClarifyResponseData {
            data_source_id,
            question: question_text,
            sql,
            explanation,
            error: None,
            query_id,
            conversation_id: Some(conv_id),
            clarification_context: clarification_context.clone(),
            fallback_mode: None,
            summary_version,
            applied_rules: Vec::new(),
        }),
        pending_clarification: None,
        error: None,
    }))
}

pub(crate) async fn get_clarify(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> Result<Json<ClarifyResponse>> {
    let messages = super::chat::load_session_messages(
        &state.data_dir,
        &claims.tenant_id,
        &claims.sub,
        &session_id,
    )?;

    let latest_state = messages.iter().rev().find(|message| {
        matches!(
            message.role.as_str(),
            "clarification" | "clarification_closed"
        )
    });
    let pending = latest_state
        .filter(|message| message.role == "clarification")
        .and_then(|m| {
            let content_str = if m.content.is_string() {
                m.content.as_str()?.to_string()
            } else {
                m.content.to_string()
            };
            serde_json::from_str::<crate::nl2sql::ClarificationContext>(&content_str).ok()
        });

    if let Some(ctx) = pending {
        return Ok(Json(ClarifyResponse {
            data: None,
            pending_clarification: Some(ctx),
            error: None,
        }));
    }

    Ok(Json(ClarifyResponse {
        data: None,
        pending_clarification: None,
        error: None,
    }))
}

pub(super) fn append_clarification_closed(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    timestamp_ms: u64,
) {
    let close_record = super::chat::SessionMessageRecord {
        role: "clarification_closed".to_string(),
        content: serde_json::json!({ "reason": "resolved" }),
        timestamp_ms,
    };
    if let Err(error) = super::chat::append_message(
        &state.data_dir,
        tenant_id,
        user_id,
        session_id,
        &close_record,
    ) {
        tracing::warn!(
            tenant_id,
            user_id,
            session_id,
            error = %error,
            "failed to close resolved clarification state"
        );
    }
}

pub(crate) async fn cancel_clarify(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let record = super::chat::SessionMessageRecord {
        role: "clarification_closed".to_string(),
        content: serde_json::json!({ "reason": "cancelled_by_user" }),
        timestamp_ms: super::now_ms(),
    };
    super::chat::append_message(
        &state.data_dir,
        &claims.tenant_id,
        &claims.sub,
        &session_id,
        &record,
    )?;
    sqlx::query(
        "UPDATE nl2sql_clarification_messages
         SET deleted_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND user_id = ? AND deleted_at IS NULL
           AND (conversation_id = ? OR session_id = ?)",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&session_id)
    .bind(&session_id)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({
        "cancelled": true,
        "sessionId": session_id
    })))
}
// P3-Enterprise: Clarification request DTOs moved to mod.rs

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Business Domains Handlers
// ══════════════════════════════════════════════════════════════════════════════

// GET /nl2sql/domains — list all domains across all datasources for tenant
