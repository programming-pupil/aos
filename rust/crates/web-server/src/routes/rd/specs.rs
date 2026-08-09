//! Spec-driven RD documents and Plan Mode execution.

use super::*;

const STAGE_SPEC: &str = "spec";
const STAGE_DESIGN: &str = "design";
const STAGE_TASKS: &str = "tasks";
const STAGE_IMPLEMENTATION: &str = "implementation";
const STAGE_FINAL: &str = "final";
const RD_PLAN_AGENT_NAME: &str = "AOS Code Studio Plan";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSpecCreateRequest {
    repository_id: Option<String>,
    title: Option<String>,
    prompt: String,
    model: Option<String>,
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSpecUpdateRequest {
    title: Option<String>,
    requirements_md: Option<String>,
    design_md: Option<String>,
    tasks_md: Option<String>,
    acceptance_md: Option<String>,
    task_items: Option<Vec<RdSpecTaskItemDto>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSpecDto {
    id: String,
    repository_id: Option<String>,
    title: String,
    prompt: String,
    requirements_md: Option<String>,
    design_md: Option<String>,
    tasks_md: Option<String>,
    acceptance_md: Option<String>,
    status: String,
    mode: String,
    current_stage: String,
    spec_version: i32,
    design_version: i32,
    tasks_version: i32,
    approved_requirements_at: Option<String>,
    approved_design_at: Option<String>,
    approved_tasks_at: Option<String>,
    approved_by: Option<String>,
    stage_status_json: Option<Value>,
    task_items: Vec<RdSpecTaskItemDto>,
    implementation_summary_json: Option<Value>,
    linked_agent_task_id: Option<String>,
    last_error: Option<String>,
    model: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSpecEventDto {
    id: String,
    spec_id: String,
    event_type: String,
    stage: Option<String>,
    status: Option<String>,
    message: String,
    metadata_json: Option<Value>,
    created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSpecTaskItemDto {
    id: String,
    title: String,
    description: String,
    status: String,
    priority: String,
    linked_rd_task_id: Option<String>,
    acceptance: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSpecTaskRequest {
    model: Option<String>,
    task_item_id: Option<String>,
    agent_profile_id: Option<String>,
    workflow_id: Option<String>,
}

pub(super) async fn list_specs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Vec<RdSpecDto>>, AppError> {
    let rows = sqlx::query(&spec_select_sql(
        "WHERE tenant_id = ? AND user_id = ? ORDER BY created_at DESC LIMIT 100",
    ))
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(row_to_spec).collect()))
}

pub(super) async fn get_spec(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    refresh_plan_task_statuses(&state.db, &claims.tenant_id, &claims.sub, &spec_id).await?;
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, &spec_id)
        .await
        .map(Json)
}

pub(super) async fn list_spec_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<Vec<RdSpecEventDto>>, AppError> {
    ensure_spec_access(&state.db, &claims.tenant_id, &claims.sub, &spec_id).await?;
    let rows = sqlx::query(
        r"
        SELECT id, spec_id, event_type, stage, status, message,
               CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM rd_spec_events
        WHERE tenant_id = ? AND spec_id = ?
        ORDER BY created_at DESC
        LIMIT 100
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&spec_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| RdSpecEventDto {
                id: row.get("id"),
                spec_id: row.get("spec_id"),
                event_type: row.get("event_type"),
                stage: row.get("stage"),
                status: row.get("status"),
                message: row.get("message"),
                metadata_json: parse_json_opt(row.get("metadata_json")),
                created_at: row.get("created_at"),
            })
            .collect(),
    ))
}

pub(super) async fn create_spec(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdSpecCreateRequest>,
) -> Result<Json<RdSpecDto>, AppError> {
    if req.prompt.trim().is_empty() {
        return Err(AppError::ValidationError("prompt is required".to_string()));
    }
    let title = req
        .title
        .clone()
        .unwrap_or_else(|| derive_title(&req.prompt));
    let id = uuid::Uuid::new_v4().to_string();
    let mode = req.mode.unwrap_or_else(|| "plan".to_string());
    sqlx::query(
        r"
        INSERT INTO rd_specs
            (id, tenant_id, user_id, repository_id, title, prompt, status, mode,
             current_stage, spec_version, design_version, tasks_version, model,
             stage_status_json)
        VALUES (?, ?, ?, ?, ?, ?, 'queued', ?, 'spec', 0, 0, 0, ?, ?)
        ",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&req.repository_id)
    .bind(&title)
    .bind(req.prompt.trim())
    .bind(&mode)
    .bind(&req.model)
    .bind(json_to_string(&json!({"spec": "queued"}))?)
    .execute(&state.db)
    .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        &id,
        "spec.created",
        Some(STAGE_SPEC),
        Some("queued"),
        "规格已保存，正在后台生成",
        Some(json!({ "mode": mode })),
    )
    .await?;
    let worker_state = state.clone();
    let worker_claims = claims.clone();
    let worker_id = id.clone();
    let worker_model = req.model.clone();
    tokio::spawn(async move {
        if let Err(error) = generate_spec_inner(
            &worker_state,
            &worker_claims,
            &worker_id,
            worker_model.as_deref(),
        )
        .await
        {
            let error_text = error.to_string();
            tracing::error!(
                spec_id = %worker_id,
                error = %error_text,
                "background Plan Spec generation failed"
            );
            // Covers failures before an AgentOps task could be created (for
            // example, a transient database/model configuration error).
            let _ = sqlx::query(
                "UPDATE rd_specs SET status = 'failed', last_error = ?, stage_status_json = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND user_id = ?",
            )
            .bind(&error_text)
            .bind(json!({"spec": "failed"}).to_string())
            .bind(&worker_id)
            .bind(&worker_claims.tenant_id)
            .bind(&worker_claims.sub)
            .execute(&worker_state.db)
            .await;
            let _ = record_spec_event(
                &worker_state.db,
                &worker_claims.tenant_id,
                &worker_id,
                "spec.failed",
                Some(STAGE_SPEC),
                Some("failed"),
                "规格后台生成失败",
                Some(json!({"error": error_text})),
            )
            .await;
        }
    });
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, &id)
        .await
        .map(Json)
}

pub(super) async fn update_spec(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
    Json(req): Json<RdSpecUpdateRequest>,
) -> Result<Json<RdSpecDto>, AppError> {
    ensure_spec_access(&state.db, &claims.tenant_id, &claims.sub, &spec_id).await?;
    let task_items_json = req
        .task_items
        .as_ref()
        .map(|items| serde_json::to_value(items))
        .transpose()
        .map_err(AppError::Json)?;
    sqlx::query(
        r"
        UPDATE rd_specs
        SET title = COALESCE(?, title),
            requirements_md = COALESCE(?, requirements_md),
            design_md = COALESCE(?, design_md),
            tasks_md = COALESCE(?, tasks_md),
            acceptance_md = COALESCE(?, acceptance_md),
            task_items_json = COALESCE(?, task_items_json),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
        ",
    )
    .bind(req.title)
    .bind(req.requirements_md)
    .bind(req.design_md)
    .bind(req.tasks_md)
    .bind(req.acceptance_md)
    .bind(task_items_json.as_ref().map(json_to_string).transpose()?)
    .bind(&spec_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        &spec_id,
        "spec.updated",
        None,
        Some("updated"),
        "Plan Mode 文档已更新",
        None,
    )
    .await?;
    if let Ok(agent_task_id) =
        ensure_plan_agent_task_for_existing_spec(&state, &claims, &spec_id).await
    {
        record_plan_agent_event(
            &state,
            &claims.tenant_id,
            &agent_task_id,
            "rd_plan.spec.updated",
            STAGE_SPEC,
            crate::routes::agent_ops::STATUS_RUNNING,
            "Plan Mode 文档已更新",
            Some(json!({ "specId": spec_id })),
        )
        .await?;
    }
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, &spec_id)
        .await
        .map(Json)
}

pub(super) async fn generate_spec(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    generate_spec_inner(&state, &claims, &spec_id, None)
        .await
        .map(Json)
}

pub(super) async fn approve_spec(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    approve_stage(&state, &claims, &spec_id, STAGE_SPEC)
        .await
        .map(Json)
}

pub(super) async fn generate_design(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    generate_design_inner(&state, &claims, &spec_id)
        .await
        .map(Json)
}

pub(super) async fn approve_design(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    approve_stage(&state, &claims, &spec_id, STAGE_DESIGN)
        .await
        .map(Json)
}

pub(super) async fn generate_tasks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    generate_tasks_inner(&state, &claims, &spec_id)
        .await
        .map(Json)
}

pub(super) async fn approve_tasks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    approve_stage(&state, &claims, &spec_id, STAGE_TASKS)
        .await
        .map(Json)
}

pub(super) async fn implement_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
    Json(req): Json<RdSpecTaskRequest>,
) -> Result<Json<RdTaskDto>, AppError> {
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, &spec_id).await?;
    if spec.approved_tasks_at.is_none() {
        return Err(AppError::ValidationError(
            "tasks must be approved before implementation".to_string(),
        ));
    }
    let item = select_task_item(&spec, req.task_item_id.as_deref())?;
    let item_json = serde_json::to_string_pretty(&item).map_err(AppError::Json)?;
    let prompt = prompts::plan_implement_task_prompt(
        &spec.title,
        spec.requirements_md.as_deref().unwrap_or_default(),
        spec.design_md.as_deref().unwrap_or_default(),
        &item_json,
    );
    let Json(task) = create_task(
        State(state.clone()),
        Extension(claims.clone()),
        Json(RdTaskCreateRequest {
            repository_id: spec.repository_id.clone(),
            spec_id: Some(spec_id.clone()),
            agent_profile_id: req.agent_profile_id,
            workflow_id: req.workflow_id,
            parent_task_id: None,
            baseline_policy: None,
            mode: Some("modify".to_string()),
            context_profile: Some("modify".to_string()),
            context_depth: Some("standard".to_string()),
            should_deep_scan: Some(false),
            title: Some(format!("{}：{}", spec.title, item.title)),
            prompt,
            model: req.model,
            agent_task_id: None,
        }),
    )
    .await?;
    upsert_spec_task_link(
        &state.db,
        &claims.tenant_id,
        &spec_id,
        &item.id,
        Some(&task.id),
        None,
        &task.status,
    )
    .await?;
    update_task_item_status(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &spec_id,
        &item.id,
        &task.status,
        Some(&task.id),
    )
    .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        &spec_id,
        "spec.implementation_task_started",
        Some(STAGE_IMPLEMENTATION),
        Some(&task.status),
        "Plan task item 已创建真实 RD 任务",
        Some(json!({ "taskItemId": item.id, "rdTaskId": task.id })),
    )
    .await?;
    Ok(Json(task))
}

pub(super) async fn implement_all(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
    Json(req): Json<RdSpecTaskRequest>,
) -> Result<Json<Vec<RdTaskDto>>, AppError> {
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, &spec_id).await?;
    let mut created = Vec::new();
    for item in spec
        .task_items
        .iter()
        .filter(|item| item.status == "pending" || item.status == "failed")
    {
        let Json(task) = implement_task(
            State(state.clone()),
            Extension(claims.clone()),
            AxumPath(spec_id.clone()),
            Json(RdSpecTaskRequest {
                model: req.model.clone(),
                task_item_id: Some(item.id.clone()),
                agent_profile_id: req.agent_profile_id.clone(),
                workflow_id: req.workflow_id.clone(),
            }),
        )
        .await?;
        created.push(task);
    }
    Ok(Json(created))
}

pub(super) async fn final_report_spec(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
) -> Result<Json<RdSpecDto>, AppError> {
    refresh_plan_task_statuses(&state.db, &claims.tenant_id, &claims.sub, &spec_id).await?;
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, &spec_id).await?;
    let agent_task_id = ensure_plan_agent_task_for_spec(&state, &claims, &spec).await?;
    record_plan_agent_event(
        &state,
        &claims.tenant_id,
        &agent_task_id,
        "rd_plan.final_report.started",
        STAGE_FINAL,
        crate::routes::agent_ops::STATUS_RUNNING,
        "正在生成 Plan Mode 最终交付报告",
        Some(json!({ "specId": spec_id })),
    )
    .await?;
    let implementation_evidence =
        build_implementation_summary(&state.db, &claims.tenant_id, &claims.sub, &spec).await?;
    let summary_json =
        serde_json::to_string_pretty(&implementation_evidence).map_err(AppError::Json)?;
    let prompt = prompts::plan_final_report_prompt(
        &spec.title,
        spec.requirements_md.as_deref().unwrap_or_default(),
        spec.design_md.as_deref().unwrap_or_default(),
        spec.tasks_md.as_deref().unwrap_or_default(),
        &summary_json,
    );
    let completion = match run_rd_completion(
        &state,
        &claims.tenant_id,
        &claims.sub,
        spec.model.as_deref(),
        prompt,
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            record_plan_agent_failure(
                &state,
                &claims.tenant_id,
                &agent_task_id,
                STAGE_FINAL,
                "生成最终交付报告失败",
                &error,
            )
            .await?;
            sqlx::query(
                "UPDATE rd_specs SET status = 'failed', last_error = ?, stage_status_json = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND user_id = ?",
            )
            .bind(error.to_string())
            .bind(json_to_string(&json!({"spec": "failed"}))?)
            .bind(&spec_id)
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .execute(&state.db)
            .await?;
            record_spec_event(
                &state.db,
                &claims.tenant_id,
                &spec_id,
                "spec.failed",
                Some(STAGE_SPEC),
                Some("failed"),
                "规格文档生成失败",
                Some(json!({ "error": error.to_string() })),
            )
            .await?;
            return Err(error);
        }
    };
    let parsed = parse_json_object(&completion.text);
    let final_report = string_json_field(&parsed, "finalReportMd").unwrap_or(completion.text);
    let implementation_summary = json!({
        "finalReportMd": final_report,
        "model": completion.model,
        "generatedAt": Utc::now().to_rfc3339(),
        "evidence": implementation_evidence,
    });
    sqlx::query(
        r"
        UPDATE rd_specs
        SET current_stage = 'final',
            status = 'completed',
            implementation_summary_json = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
        ",
    )
    .bind(json_to_string(&implementation_summary)?)
    .bind(&spec_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        &spec_id,
        "spec.final_report_generated",
        Some(STAGE_FINAL),
        Some("completed"),
        "Plan Mode 最终交付报告已生成",
        Some(json!({ "model": completion.model })),
    )
    .await?;
    crate::routes::agent_ops::complete_task(
        &state,
        &claims.tenant_id,
        &agent_task_id,
        "Plan Mode 最终交付报告已生成",
        Some(json!({ "specId": spec_id, "model": completion.model })),
    )
    .await?;
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, &spec_id)
        .await
        .map(Json)
}

pub(super) async fn create_task_from_spec(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(spec_id): AxumPath<String>,
    Json(req): Json<RdSpecTaskRequest>,
) -> Result<Json<RdTaskDto>, AppError> {
    implement_task(
        State(state),
        Extension(claims),
        AxumPath(spec_id),
        Json(req),
    )
    .await
}

async fn generate_spec_inner(
    state: &AppState,
    claims: &Claims,
    spec_id: &str,
    model: Option<&str>,
) -> Result<RdSpecDto, AppError> {
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await?;
    let agent_task_id = ensure_plan_agent_task_for_spec(state, claims, &spec).await?;
    sqlx::query(
        "UPDATE rd_specs SET status = 'running', current_stage = 'spec', last_error = NULL, stage_status_json = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(json_to_string(&json!({"spec": "running"}))?)
    .bind(spec_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    record_plan_agent_event(
        state,
        &claims.tenant_id,
        &agent_task_id,
        "rd_plan.spec.generate_started",
        STAGE_SPEC,
        crate::routes::agent_ops::STATUS_RUNNING,
        "正在生成 Plan Spec",
        Some(json!({ "specId": spec_id, "repositoryId": spec.repository_id })),
    )
    .await?;
    let context = repository_context_for_spec(state, claims, &spec).await;
    let prompt = prompts::plan_generate_spec_prompt(&spec.prompt, &context);
    let completion = match run_rd_completion(
        state,
        &claims.tenant_id,
        &claims.sub,
        model.or(spec.model.as_deref()),
        prompt,
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            record_plan_agent_failure(
                state,
                &claims.tenant_id,
                &agent_task_id,
                STAGE_SPEC,
                "生成 Plan Spec 失败",
                &error,
            )
            .await?;
            return Err(error);
        }
    };
    let parsed = parse_json_object(&completion.text);
    let requirements = string_json_field(&parsed, "requirementsMd").unwrap_or(completion.text);
    let acceptance = string_json_field(&parsed, "acceptanceMd").unwrap_or_default();
    sqlx::query(
        r"
        UPDATE rd_specs
        SET requirements_md = ?, acceptance_md = ?, current_stage = 'spec',
            spec_version = spec_version + 1, status = 'draft', model = ?, last_error = NULL,
            stage_status_json = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
        ",
    )
    .bind(requirements)
    .bind(acceptance)
    .bind(&completion.model)
    .bind(json_to_string(&json!({"spec": "generated"}))?)
    .bind(spec_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        spec_id,
        "spec.generated",
        Some(STAGE_SPEC),
        Some("generated"),
        "规格文档已生成",
        Some(json!({ "model": completion.model })),
    )
    .await?;
    record_plan_agent_event(
        state,
        &claims.tenant_id,
        &agent_task_id,
        "rd_plan.spec.generated",
        STAGE_SPEC,
        crate::routes::agent_ops::STATUS_RUNNING,
        "规格文档已生成，等待用户确认",
        Some(json!({ "specId": spec_id, "model": completion.model })),
    )
    .await?;
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await
}

async fn generate_design_inner(
    state: &AppState,
    claims: &Claims,
    spec_id: &str,
) -> Result<RdSpecDto, AppError> {
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await?;
    let agent_task_id = ensure_plan_agent_task_for_spec(state, claims, &spec).await?;
    if spec.approved_requirements_at.is_none() {
        return Err(AppError::ValidationError(
            "requirements must be approved before design".to_string(),
        ));
    }
    record_plan_agent_event(
        state,
        &claims.tenant_id,
        &agent_task_id,
        "rd_plan.design.generate_started",
        STAGE_DESIGN,
        crate::routes::agent_ops::STATUS_RUNNING,
        "正在生成技术设计",
        Some(json!({ "specId": spec_id, "repositoryId": spec.repository_id })),
    )
    .await?;
    let context = repository_context_for_spec(state, claims, &spec).await;
    let prompt = prompts::plan_generate_design_prompt(
        &spec.prompt,
        spec.requirements_md.as_deref().unwrap_or_default(),
        spec.acceptance_md.as_deref().unwrap_or_default(),
        &context,
    );
    let completion = match run_rd_completion(
        state,
        &claims.tenant_id,
        &claims.sub,
        spec.model.as_deref(),
        prompt,
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            record_plan_agent_failure(
                state,
                &claims.tenant_id,
                &agent_task_id,
                STAGE_DESIGN,
                "生成技术设计失败",
                &error,
            )
            .await?;
            return Err(error);
        }
    };
    let parsed = parse_json_object(&completion.text);
    let design = string_json_field(&parsed, "designMd").unwrap_or(completion.text);
    sqlx::query(
        r"
        UPDATE rd_specs
        SET design_md = ?, current_stage = 'design', design_version = design_version + 1,
            model = ?, last_error = NULL, stage_status_json = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
        ",
    )
    .bind(design)
    .bind(&completion.model)
    .bind(json_to_string(
        &json!({"spec": "approved", "design": "generated"}),
    )?)
    .bind(spec_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        spec_id,
        "design.generated",
        Some(STAGE_DESIGN),
        Some("generated"),
        "技术设计已生成",
        Some(json!({ "model": completion.model })),
    )
    .await?;
    record_plan_agent_event(
        state,
        &claims.tenant_id,
        &agent_task_id,
        "rd_plan.design.generated",
        STAGE_DESIGN,
        crate::routes::agent_ops::STATUS_RUNNING,
        "技术设计已生成，等待用户确认",
        Some(json!({ "specId": spec_id, "model": completion.model })),
    )
    .await?;
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await
}

async fn generate_tasks_inner(
    state: &AppState,
    claims: &Claims,
    spec_id: &str,
) -> Result<RdSpecDto, AppError> {
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await?;
    let agent_task_id = ensure_plan_agent_task_for_spec(state, claims, &spec).await?;
    if spec.approved_design_at.is_none() {
        return Err(AppError::ValidationError(
            "design must be approved before tasks".to_string(),
        ));
    }
    record_plan_agent_event(
        state,
        &claims.tenant_id,
        &agent_task_id,
        "rd_plan.tasks.generate_started",
        STAGE_TASKS,
        crate::routes::agent_ops::STATUS_RUNNING,
        "正在拆解实施任务",
        Some(json!({ "specId": spec_id, "repositoryId": spec.repository_id })),
    )
    .await?;
    let context = repository_context_for_spec(state, claims, &spec).await;
    let prompt = prompts::plan_generate_tasks_prompt(
        &spec.prompt,
        spec.requirements_md.as_deref().unwrap_or_default(),
        spec.design_md.as_deref().unwrap_or_default(),
        spec.acceptance_md.as_deref().unwrap_or_default(),
        &context,
    );
    let completion = match run_rd_completion(
        state,
        &claims.tenant_id,
        &claims.sub,
        spec.model.as_deref(),
        prompt,
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            record_plan_agent_failure(
                state,
                &claims.tenant_id,
                &agent_task_id,
                STAGE_TASKS,
                "拆解实施任务失败",
                &error,
            )
            .await?;
            return Err(error);
        }
    };
    let parsed = parse_json_object(&completion.text);
    let tasks_md = string_json_field(&parsed, "tasksMd").unwrap_or(completion.text);
    let task_items = parse_task_items(parsed.get("taskItems")).unwrap_or_else(|| {
        vec![RdSpecTaskItemDto {
            id: "task-1".to_string(),
            title: "实现规格".to_string(),
            description: tasks_md.clone(),
            status: "pending".to_string(),
            priority: "p0".to_string(),
            linked_rd_task_id: None,
            acceptance: Vec::new(),
        }]
    });
    sqlx::query(
        r"
        UPDATE rd_specs
        SET tasks_md = ?, task_items_json = ?, current_stage = 'tasks',
            tasks_version = tasks_version + 1, model = ?, last_error = NULL,
            stage_status_json = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
        ",
    )
    .bind(tasks_md)
    .bind(json_to_string(
        &serde_json::to_value(&task_items).map_err(AppError::Json)?,
    )?)
    .bind(&completion.model)
    .bind(json_to_string(
        &json!({"spec": "approved", "design": "approved", "tasks": "generated"}),
    )?)
    .bind(spec_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        spec_id,
        "tasks.generated",
        Some(STAGE_TASKS),
        Some("generated"),
        "任务拆解已生成",
        Some(json!({ "model": completion.model, "taskItemCount": task_items.len() })),
    )
    .await?;
    record_plan_agent_event(
        state,
        &claims.tenant_id,
        &agent_task_id,
        "rd_plan.tasks.generated",
        STAGE_TASKS,
        crate::routes::agent_ops::STATUS_RUNNING,
        "任务拆解已生成，等待用户确认",
        Some(json!({ "specId": spec_id, "model": completion.model, "taskItemCount": task_items.len() })),
    )
    .await?;
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await
}

async fn approve_stage(
    state: &AppState,
    claims: &Claims,
    spec_id: &str,
    stage: &str,
) -> Result<RdSpecDto, AppError> {
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await?;
    match stage {
        STAGE_SPEC
            if spec
                .requirements_md
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty() =>
        {
            return Err(AppError::ValidationError(
                "requirements are empty".to_string(),
            ));
        }
        STAGE_DESIGN => {
            if spec.approved_requirements_at.is_none() {
                return Err(AppError::ValidationError(
                    "requirements must be approved first".to_string(),
                ));
            }
            if spec.design_md.as_deref().unwrap_or("").trim().is_empty() {
                return Err(AppError::ValidationError("design is empty".to_string()));
            }
        }
        STAGE_TASKS => {
            if spec.approved_design_at.is_none() {
                return Err(AppError::ValidationError(
                    "design must be approved first".to_string(),
                ));
            }
            if spec.task_items.is_empty() {
                return Err(AppError::ValidationError(
                    "task items are empty".to_string(),
                ));
            }
        }
        _ => {}
    }
    let (field, next_stage, event_type) = match stage {
        STAGE_SPEC => ("approved_requirements_at", STAGE_DESIGN, "spec.approved"),
        STAGE_DESIGN => ("approved_design_at", STAGE_TASKS, "design.approved"),
        STAGE_TASKS => ("approved_tasks_at", STAGE_IMPLEMENTATION, "tasks.approved"),
        _ => {
            return Err(AppError::ValidationError(
                "unsupported approval stage".to_string(),
            ))
        }
    };
    let sql = format!(
        "UPDATE rd_specs SET {field} = COALESCE({field}, CURRENT_TIMESTAMP), approved_by = ?, current_stage = ?, status = 'approved', updated_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND user_id = ?"
    );
    sqlx::query(&sql)
        .bind(&claims.sub)
        .bind(next_stage)
        .bind(spec_id)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .execute(&state.db)
        .await?;
    record_spec_event(
        &state.db,
        &claims.tenant_id,
        spec_id,
        event_type,
        Some(stage),
        Some("approved"),
        "Plan Mode 阶段已确认",
        Some(json!({ "approvedBy": claims.sub })),
    )
    .await?;
    if let Ok(agent_task_id) = ensure_plan_agent_task_for_spec(state, claims, &spec).await {
        record_plan_agent_event(
            state,
            &claims.tenant_id,
            &agent_task_id,
            &format!("rd_plan.{stage}.approved"),
            stage,
            crate::routes::agent_ops::STATUS_RUNNING,
            "Plan Mode 阶段已确认",
            Some(json!({ "specId": spec_id, "approvedBy": claims.sub, "nextStage": next_stage })),
        )
        .await?;
    }
    get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await
}

async fn repository_context_for_spec(
    state: &AppState,
    claims: &Claims,
    spec: &RdSpecDto,
) -> String {
    match spec.repository_id.as_deref() {
        Some(repo_id) => build_repository_context(state, claims, repo_id)
            .await
            .unwrap_or_default(),
        None => String::new(),
    }
}

async fn ensure_plan_agent_task_for_existing_spec(
    state: &AppState,
    claims: &Claims,
    spec_id: &str,
) -> Result<String, AppError> {
    let spec = get_spec_row(&state.db, &claims.tenant_id, &claims.sub, spec_id).await?;
    ensure_plan_agent_task_for_spec(state, claims, &spec).await
}

async fn ensure_plan_agent_task_for_spec(
    state: &AppState,
    claims: &Claims,
    spec: &RdSpecDto,
) -> Result<String, AppError> {
    ensure_plan_agent_task(state, claims, &spec.id, &spec.title, &spec.prompt).await
}

async fn ensure_plan_agent_task(
    state: &AppState,
    claims: &Claims,
    spec_id: &str,
    title: &str,
    prompt: &str,
) -> Result<String, AppError> {
    let outcome = crate::routes::agent_ops::create_task_with_outcome(
        state,
        crate::routes::agent_ops::CreateAgentTaskInput {
            tenant_id: claims.tenant_id.clone(),
            source: "webui".to_string(),
            source_ref: Some(spec_id.to_string()),
            source_label: Some("Code Studio Plan Mode".to_string()),
            capability_key: "rd_agent".to_string(),
            agent_id: None,
            agent_name: Some(RD_PLAN_AGENT_NAME.to_string()),
            title: format!("Plan Mode：{title}"),
            summary: Some(
                "Code Studio Spec -> Design -> Tasks structured development flow".to_string(),
            ),
            owner_user_id: Some(claims.sub.clone()),
            correlation_id: Some(format!("rd_spec:{spec_id}")),
            parent_task_id: None,
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
            external_message_id: None,
            idempotency_key: Some(format!("rd_plan:{spec_id}")),
            input_json: Some(json!({
                "specId": spec_id,
                "prompt": prompt,
                "mode": "plan",
            })),
        },
    )
    .await?;
    crate::routes::agent_ops::link_task_resource(
        state,
        &claims.tenant_id,
        &outcome.id,
        "rd_spec",
        spec_id,
    )
    .await?;
    sqlx::query(
        "UPDATE rd_specs SET linked_agent_task_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND user_id = ? AND (linked_agent_task_id IS NULL OR linked_agent_task_id = '')",
    )
    .bind(&outcome.id)
    .bind(spec_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    Ok(outcome.id)
}

async fn record_plan_agent_event(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    event_type: &str,
    stage: &str,
    status: &str,
    message: &str,
    metadata_json: Option<Value>,
) -> Result<(), AppError> {
    crate::routes::agent_ops::mark_task_running(
        state,
        tenant_id,
        agent_task_id,
        stage,
        message,
        progress_for_plan_stage(stage, status),
    )
    .await?;
    crate::routes::agent_ops::add_event(
        state,
        tenant_id,
        agent_task_id,
        event_type,
        Some(stage),
        Some(status),
        "info",
        message,
        metadata_json,
    )
    .await?;
    Ok(())
}

async fn record_plan_agent_failure(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    stage: &str,
    message: &str,
    error: &AppError,
) -> Result<(), AppError> {
    crate::routes::agent_ops::add_event(
        state,
        tenant_id,
        agent_task_id,
        "rd_plan.failed",
        Some(stage),
        Some(crate::routes::agent_ops::STATUS_FAILED),
        "error",
        message,
        Some(json!({ "error": error.to_string() })),
    )
    .await?;
    crate::routes::agent_ops::fail_task(
        state,
        tenant_id,
        agent_task_id,
        "RD_PLAN_STAGE_FAILED",
        &format!("{message}: {error}"),
    )
    .await?;
    Ok(())
}

fn progress_for_plan_stage(stage: &str, status: &str) -> i32 {
    if status == crate::routes::agent_ops::STATUS_FAILED {
        return 100;
    }
    match stage {
        STAGE_SPEC => 20,
        STAGE_DESIGN => 40,
        STAGE_TASKS => 60,
        STAGE_IMPLEMENTATION => 78,
        STAGE_FINAL => 92,
        _ => 10,
    }
}

async fn ensure_spec_access(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    spec_id: &str,
) -> Result<(), AppError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS c FROM rd_specs WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(spec_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_one(db)
    .await?;
    let count: i64 = row.get("c");
    if count == 0 {
        return Err(AppError::NotFound("rd spec not found".to_string()));
    }
    Ok(())
}

async fn get_spec_row(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    spec_id: &str,
) -> Result<RdSpecDto, AppError> {
    let row = sqlx::query(&spec_select_sql(
        "WHERE id = ? AND tenant_id = ? AND user_id = ?",
    ))
    .bind(spec_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("rd spec not found".to_string()))?;
    Ok(row_to_spec(&row))
}

fn spec_select_sql(where_sql: &str) -> String {
    format!(
        r#"
        SELECT id, repository_id, title, prompt, requirements_md, design_md, tasks_md,
               acceptance_md, status,
               COALESCE(mode, 'plan') AS mode,
               COALESCE(current_stage, 'spec') AS current_stage,
               COALESCE(spec_version, 0) AS spec_version,
               COALESCE(design_version, 0) AS design_version,
               COALESCE(tasks_version, 0) AS tasks_version,
               CAST(approved_requirements_at AS TEXT) AS approved_requirements_at,
               CAST(approved_design_at AS TEXT) AS approved_design_at,
               CAST(approved_tasks_at AS TEXT) AS approved_tasks_at,
               approved_by,
               CAST(stage_status_json AS TEXT) AS stage_status_json,
               CAST(task_items_json AS TEXT) AS task_items_json,
               CAST(implementation_summary_json AS TEXT) AS implementation_summary_json,
               linked_agent_task_id, last_error, model,
               CAST(created_at AS TEXT) created_at,
               CAST(updated_at AS TEXT) updated_at
        FROM rd_specs
        {where_sql}
        "#
    )
}

fn row_to_spec(row: &sqlx::sqlite::SqliteRow) -> RdSpecDto {
    let task_items_json: Option<String> = row.get("task_items_json");
    RdSpecDto {
        id: row.get("id"),
        repository_id: row.get("repository_id"),
        title: row.get("title"),
        prompt: row.get("prompt"),
        requirements_md: row.get("requirements_md"),
        design_md: row.get("design_md"),
        tasks_md: row.get("tasks_md"),
        acceptance_md: row.get("acceptance_md"),
        status: row.get("status"),
        mode: row.get("mode"),
        current_stage: row.get("current_stage"),
        spec_version: row.get("spec_version"),
        design_version: row.get("design_version"),
        tasks_version: row.get("tasks_version"),
        approved_requirements_at: row.get("approved_requirements_at"),
        approved_design_at: row.get("approved_design_at"),
        approved_tasks_at: row.get("approved_tasks_at"),
        approved_by: row.get("approved_by"),
        stage_status_json: parse_json_opt(row.get("stage_status_json")),
        task_items: parse_task_items_from_raw(task_items_json.as_deref()),
        implementation_summary_json: parse_json_opt(row.get("implementation_summary_json")),
        linked_agent_task_id: row.get("linked_agent_task_id"),
        last_error: row.get("last_error"),
        model: row.get("model"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn parse_json_object(text: &str) -> Value {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    let Some(start) = trimmed.find('{') else {
        return json!({});
    };
    let Some(end) = trimmed.rfind('}') else {
        return json!({});
    };
    serde_json::from_str::<Value>(&trimmed[start..=end]).unwrap_or_else(|_| json!({}))
}

fn string_json_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_task_items(value: Option<&Value>) -> Option<Vec<RdSpecTaskItemDto>> {
    let items = value?.as_array()?;
    let parsed = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let title = item.get("title")?.as_str()?.trim();
            if title.is_empty() {
                return None;
            }
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("task-{}", idx + 1));
            let description = item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(title)
                .to_string();
            let priority = item
                .get("priority")
                .and_then(Value::as_str)
                .unwrap_or("p1")
                .to_ascii_lowercase();
            let acceptance = item
                .get("acceptance")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Some(RdSpecTaskItemDto {
                id,
                title: title.to_string(),
                description,
                status: "pending".to_string(),
                priority: if matches!(priority.as_str(), "p0" | "p1" | "p2") {
                    priority
                } else {
                    "p1".to_string()
                },
                linked_rd_task_id: None,
                acceptance,
            })
        })
        .collect::<Vec<_>>();
    (!parsed.is_empty()).then_some(parsed)
}

fn parse_task_items_from_raw(raw: Option<&str>) -> Vec<RdSpecTaskItemDto> {
    raw.and_then(|value| serde_json::from_str::<Value>(value).ok())
        .as_ref()
        .and_then(|value| parse_task_items(Some(value)))
        .unwrap_or_default()
}

fn select_task_item(
    spec: &RdSpecDto,
    task_item_id: Option<&str>,
) -> Result<RdSpecTaskItemDto, AppError> {
    if let Some(task_item_id) = task_item_id {
        return spec
            .task_items
            .iter()
            .find(|item| item.id == task_item_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("task item not found".to_string()));
    }
    spec.task_items
        .iter()
        .find(|item| item.status == "pending")
        .or_else(|| spec.task_items.first())
        .cloned()
        .ok_or_else(|| AppError::ValidationError("task items are empty".to_string()))
}

async fn update_task_item_status(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    spec_id: &str,
    task_item_id: &str,
    status: &str,
    linked_rd_task_id: Option<&str>,
) -> Result<(), AppError> {
    let spec = get_spec_row(db, tenant_id, user_id, spec_id).await?;
    let mut items = spec.task_items;
    for item in &mut items {
        if item.id == task_item_id {
            item.status = status.to_string();
            if let Some(linked_rd_task_id) = linked_rd_task_id {
                item.linked_rd_task_id = Some(linked_rd_task_id.to_string());
            }
        }
    }
    sqlx::query(
        "UPDATE rd_specs SET task_items_json = ?, current_stage = 'implementation', updated_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(json_to_string(&serde_json::to_value(&items).map_err(AppError::Json)?)?)
    .bind(spec_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn refresh_plan_task_statuses(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    spec_id: &str,
) -> Result<(), AppError> {
    let spec = get_spec_row(db, tenant_id, user_id, spec_id).await?;
    if spec.task_items.is_empty() {
        return Ok(());
    }

    let rows = sqlx::query(
        r"
        SELECT l.task_item_id, l.rd_task_id, t.status
        FROM rd_spec_task_links l
        INNER JOIN rd_tasks t
          ON t.tenant_id = l.tenant_id
         AND t.id = l.rd_task_id
        WHERE l.tenant_id = ?
          AND l.spec_id = ?
          AND t.user_id = ?
        ",
    )
    .bind(tenant_id)
    .bind(spec_id)
    .bind(user_id)
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut task_statuses = BTreeMap::<String, (String, String)>::new();
    for row in rows {
        let task_item_id: String = row.get("task_item_id");
        let rd_task_id: String = row.get("rd_task_id");
        let status: String = row.get("status");
        task_statuses.insert(task_item_id, (status, rd_task_id));
    }

    let mut changed = false;
    let mut items = spec.task_items;
    for item in &mut items {
        let Some((status, rd_task_id)) = task_statuses.get(&item.id) else {
            continue;
        };
        if &item.status != status {
            item.status = status.clone();
            changed = true;
        }
        if item.linked_rd_task_id.as_deref() != Some(rd_task_id.as_str()) {
            item.linked_rd_task_id = Some(rd_task_id.clone());
            changed = true;
        }
    }

    if !changed {
        return Ok(());
    }

    sqlx::query(
        "UPDATE rd_specs SET task_items_json = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(json_to_string(&serde_json::to_value(&items).map_err(AppError::Json)?)?)
    .bind(spec_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(db)
    .await?;

    for (task_item_id, (status, rd_task_id)) in task_statuses {
        upsert_spec_task_link(
            db,
            tenant_id,
            spec_id,
            &task_item_id,
            Some(&rd_task_id),
            None,
            &status,
        )
        .await?;
    }

    Ok(())
}

async fn build_implementation_summary(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    spec: &RdSpecDto,
) -> Result<Value, AppError> {
    let mut items = Vec::new();
    for item in &spec.task_items {
        let Some(rd_task_id) = item.linked_rd_task_id.as_deref() else {
            items.push(json!({
                "taskItemId": item.id,
                "title": item.title,
                "status": item.status,
                "linkedRdTaskId": Value::Null,
                "evidenceStatus": "not_started",
            }));
            continue;
        };

        let task = get_task_row(db, tenant_id, user_id, rd_task_id).await.ok();
        let diff_summary = load_plan_diff_summary(db, tenant_id, rd_task_id).await?;
        let test_summary = load_plan_test_summary(db, tenant_id, rd_task_id).await?;
        let recent_events = load_plan_recent_events(db, tenant_id, rd_task_id).await?;

        items.push(json!({
            "taskItemId": item.id,
            "title": item.title,
            "description": item.description,
            "priority": item.priority,
            "acceptance": item.acceptance,
            "status": task.as_ref().map(|task| task.status.as_str()).unwrap_or(item.status.as_str()),
            "linkedRdTaskId": rd_task_id,
            "rdTaskTitle": task.as_ref().map(|task| task.title.as_str()),
            "rdTaskMode": task.as_ref().map(|task| task.mode.as_str()),
            "answerPreview": task.as_ref().and_then(|task| task.answer_md.as_deref()).map(|text| truncate_text(text, 2_000)),
            "errorMessage": task.as_ref().and_then(|task| task.error_message.as_deref()).map(|text| truncate_text(text, 1_000)),
            "completedAt": task.as_ref().and_then(|task| task.completed_at.as_deref()),
            "diff": diff_summary,
            "tests": test_summary,
            "recentEvents": recent_events,
        }));
    }

    let total = items.len();
    let completed = spec
        .task_items
        .iter()
        .filter(|item| item.status == "completed")
        .count();
    let failed = spec
        .task_items
        .iter()
        .filter(|item| item.status == "failed")
        .count();
    let waiting_approval = spec
        .task_items
        .iter()
        .filter(|item| item.status == "waiting_approval")
        .count();

    Ok(json!({
        "specId": spec.id,
        "title": spec.title,
        "generatedAt": Utc::now().to_rfc3339(),
        "totals": {
            "taskItems": total,
            "completed": completed,
            "waitingApproval": waiting_approval,
            "failed": failed,
        },
        "taskItems": items,
    }))
}

async fn load_plan_diff_summary(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<Value, AppError> {
    let rows = sqlx::query(
        r"
        SELECT file_path, change_type, applied, LENGTH(diff_patch) AS diff_chars
        FROM rd_file_changes
        WHERE task_id = ? AND tenant_id = ?
        ORDER BY created_at ASC
        LIMIT 80
        ",
    )
    .bind(task_id)
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    let mut pending = 0usize;
    let mut applied = 0usize;
    let mut files = Vec::new();
    for row in rows {
        let is_applied: bool = row.get("applied");
        if is_applied {
            applied += 1;
        } else {
            pending += 1;
        }
        files.push(json!({
            "filePath": row.get::<String, _>("file_path"),
            "changeType": row.get::<String, _>("change_type"),
            "applied": is_applied,
            "diffChars": row.try_get::<Option<u64>, _>("diff_chars").ok().flatten().unwrap_or_default(),
        }));
    }

    Ok(json!({
        "changeCount": files.len(),
        "pendingCount": pending,
        "appliedCount": applied,
        "files": files,
    }))
}

async fn load_plan_test_summary(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<Value, AppError> {
    let rows = sqlx::query(
        r"
        SELECT command, status, exit_code, duration_ms,
               CAST(created_at AS TEXT) AS created_at
        FROM rd_test_runs
        WHERE task_id = ? AND tenant_id = ?
        ORDER BY created_at DESC
        LIMIT 8
        ",
    )
    .bind(task_id)
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    let runs = rows
        .into_iter()
        .map(|row| {
            json!({
                "command": row.get::<String, _>("command"),
                "status": row.get::<String, _>("status"),
                "exitCode": row.get::<Option<i32>, _>("exit_code"),
                "durationMs": row.get::<Option<i64>, _>("duration_ms"),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "runCount": runs.len(),
        "latest": runs.first().cloned(),
        "runs": runs,
    }))
}

async fn load_plan_recent_events(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        r"
        SELECT stage, status, message, CAST(created_at AS TEXT) AS created_at
        FROM rd_task_events
        WHERE task_id = ? AND tenant_id = ?
        ORDER BY id DESC
        LIMIT 10
        ",
    )
    .bind(task_id)
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "stage": row.get::<String, _>("stage"),
                "status": row.get::<String, _>("status"),
                "message": row.get::<Option<String>, _>("message").map(|message| truncate_text(&message, 500)),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect())
}

async fn upsert_spec_task_link(
    db: &SqlitePool,
    tenant_id: &str,
    spec_id: &str,
    task_item_id: &str,
    rd_task_id: Option<&str>,
    agent_task_id: Option<&str>,
    status: &str,
) -> Result<(), AppError> {
    let id = format!("rdstl-{}", uuid::Uuid::new_v4());
    sqlx::query(
        r"
        INSERT INTO rd_spec_task_links
            (id, tenant_id, spec_id, task_item_id, rd_task_id, agent_task_id, status)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT DO UPDATE SET
            rd_task_id = excluded.rd_task_id,
            agent_task_id = excluded.agent_task_id,
            status = excluded.status,
            updated_at = CURRENT_TIMESTAMP
        ",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(spec_id)
    .bind(task_item_id)
    .bind(rd_task_id)
    .bind(agent_task_id)
    .bind(status)
    .execute(db)
    .await?;
    Ok(())
}

async fn record_spec_event(
    db: &SqlitePool,
    tenant_id: &str,
    spec_id: &str,
    event_type: &str,
    stage: Option<&str>,
    status: Option<&str>,
    message: &str,
    metadata_json: Option<Value>,
) -> Result<(), AppError> {
    sqlx::query(
        r"
        INSERT INTO rd_spec_events
            (id, tenant_id, spec_id, event_type, stage, status, message, metadata_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(format!("rdse-{}", uuid::Uuid::new_v4()))
    .bind(tenant_id)
    .bind(spec_id)
    .bind(event_type)
    .bind(stage)
    .bind(status)
    .bind(message)
    .bind(metadata_json.as_ref().map(json_to_string).transpose()?)
    .execute(db)
    .await?;
    Ok(())
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn json_to_string(value: &Value) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(AppError::Json)
}
