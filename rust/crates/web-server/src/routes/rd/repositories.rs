//! Repository CRUD, browsing, and indexing endpoints for AOS Code Studio.

use super::*;

mod search;
pub(super) use search::{
    repository_file_suggestions, repository_search, run_exact_repository_search,
    run_rg_repository_search,
};

#[derive(Debug, Deserialize)]
pub(super) struct RdRepositoryCreateRequest {
    name: String,
    url: String,
    branch: Option<String>,
    gitlab_token: Option<String>,
    description: Option<String>,
    default_test_command: Option<String>,
    default_build_command: Option<String>,
    auto_sync_enabled: Option<bool>,
    auto_sync_interval_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdRepositoryUpdateRequest {
    name: Option<String>,
    url: Option<String>,
    branch: Option<String>,
    gitlab_token: Option<String>,
    description: Option<String>,
    default_test_command: Option<String>,
    default_build_command: Option<String>,
    auto_sync_enabled: Option<bool>,
    auto_sync_interval_minutes: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdRepositoryDto {
    id: String,
    name: String,
    url: String,
    branch: String,
    description: Option<String>,
    is_cloned: bool,
    clone_path: Option<String>,
    last_sync_at: Option<String>,
    created_at: String,
    default_test_command: Option<String>,
    default_build_command: Option<String>,
    index_status: Option<String>,
    indexed_file_count: i32,
    indexed_symbol_count: i32,
    indexed_import_count: i32,
    detected_languages: Vec<RdLanguageStatDto>,
    detected_stack: Vec<String>,
    detected_test_command: Option<String>,
    detected_build_command: Option<String>,
    auto_sync_enabled: bool,
    auto_sync_interval_minutes: i64,
    last_auto_sync_at: Option<String>,
    last_sync_error: Option<String>,
}

#[derive(Debug, Clone)]
struct RdRepositorySetting {
    default_test_command: Option<String>,
    default_build_command: Option<String>,
    auto_sync_enabled: bool,
    auto_sync_interval_minutes: i64,
    last_auto_sync_at: Option<String>,
    last_sync_error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdRepositoryListResponse {
    repositories: Vec<RdRepositoryDto>,
    total: usize,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdRepositoryFileQuery {
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdRepositorySearchQuery {
    q: String,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdRepositoryFileSuggestionQuery {
    q: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdRepositorySearchHitDto {
    pub(super) path: String,
    pub(super) line_number: u64,
    pub(super) snippet: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdRepositoryFileSuggestionDto {
    path: String,
    name: String,
    language: Option<String>,
    size_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdRepositoryWorktreeStatusDto {
    pub(super) repository_id: String,
    pub(super) head_sha: Option<String>,
    pub(super) dirty: bool,
    pub(super) dirty_path_count: usize,
    pub(super) tracked_modified_count: usize,
    pub(super) untracked_count: usize,
    pub(super) dirty_paths_sample: Vec<String>,
    pub(super) status_short: String,
    pub(super) default_baseline_policy: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdRepositorySymbolQuery {
    q: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdRepositoryImportQuery {
    q: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdRepositorySymbolDto {
    id: u64,
    file_path: String,
    language: Option<String>,
    symbol_name: String,
    symbol_kind: String,
    signature: Option<String>,
    line_number: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdRepositoryImportDto {
    id: u64,
    file_path: String,
    language: Option<String>,
    import_path: String,
    import_kind: String,
    line_number: u64,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RdRepositoryIndexSnapshot {
    status: String,
    file_count: i32,
    symbol_count: i32,
    import_count: i32,
    detection: Option<RdRepositoryDetection>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdFileNode {
    name: String,
    path: String,
    node_type: String,
    size_bytes: Option<u64>,
    language: Option<String>,
    children: Option<Vec<RdFileNode>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdFileContentResponse {
    path: String,
    content: String,
    size_bytes: u64,
    language: Option<String>,
}

pub(super) async fn delete_repository(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    state
        .gitlab_manager()
        .delete_project(&claims.tenant_id, &claims.sub, &repository_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(json!({ "deleted": true })))
}

pub(super) async fn list_repositories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<RdRepositoryListResponse>, AppError> {
    let projects = state
        .gitlab_manager()
        .list_projects(&claims.tenant_id, &claims.sub)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let settings = load_repo_settings_map(&state.db, &claims.tenant_id, &claims.sub).await?;
    let indexes = load_repo_index_map(&state.db, &claims.tenant_id).await?;
    let repositories = projects
        .into_iter()
        .map(|p| {
            let setting = settings.get(&p.id);
            let index = indexes.get(&p.id);
            build_repository_dto(p, setting, index)
        })
        .collect::<Vec<_>>();
    Ok(Json(RdRepositoryListResponse {
        total: repositories.len(),
        repositories,
    }))
}

pub(super) async fn create_repository(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdRepositoryCreateRequest>,
) -> Result<Json<RdRepositoryDto>, AppError> {
    if req.name.trim().is_empty() || req.url.trim().is_empty() {
        return Err(AppError::ValidationError(
            "name and url are required".to_string(),
        ));
    }
    let auto_sync_enabled = req.auto_sync_enabled.unwrap_or(true);
    let auto_sync_interval_minutes = normalize_auto_sync_interval(req.auto_sync_interval_minutes)?;
    let project = state
        .gitlab_manager()
        .add_project(
            &claims.tenant_id,
            &claims.sub,
            agent_gateway::AddProjectRequest {
                name: req.name.trim().to_string(),
                url: req.url.trim().to_string(),
                branch: req.branch,
                gitlab_token: req.gitlab_token,
                description: req.description,
            },
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    upsert_repo_settings(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &project.id,
        req.default_test_command.as_deref(),
        req.default_build_command.as_deref(),
    )
    .await?;
    upsert_repo_auto_sync_settings(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &project.id,
        auto_sync_enabled,
        auto_sync_interval_minutes,
    )
    .await?;
    mark_repository_index_status(&state.db, &claims.tenant_id, &project.id, "syncing", None)
        .await?;
    let sync_state = state.clone();
    let sync_tenant = claims.tenant_id.clone();
    let sync_user = claims.sub.clone();
    let sync_repository_id = project.id.clone();
    tokio::spawn(async move {
        if let Err(error) = perform_repository_sync(
            &sync_state,
            &sync_tenant,
            &sync_user,
            &sync_repository_id,
            "repository_create",
        )
        .await
        {
            let safe_error = runtime::protect_sensitive_text(
                &error.to_string(),
                runtime::configured_data_protection_mode(),
            )
            .value;
            let message = safe_error.chars().take(1000).collect::<String>();
            tracing::warn!(
                repository_id = %sync_repository_id,
                error = %message,
                "initial repository synchronization failed"
            );
            let _ = mark_repository_index_status(
                &sync_state.db,
                &sync_tenant,
                &sync_repository_id,
                "failed",
                Some(&message),
            )
            .await;
            let _ = sqlx::query("UPDATE rd_repository_settings SET last_sync_error = ?, updated_at = CURRENT_TIMESTAMP WHERE tenant_id = ? AND user_id = ? AND project_id = ?")
                .bind(&message)
                .bind(&sync_tenant)
                .bind(&sync_user)
                .bind(&sync_repository_id)
                .execute(&sync_state.db)
                .await;
        }
    });
    Ok(Json(RdRepositoryDto {
        id: project.id,
        name: project.name,
        url: project.url,
        branch: project.branch,
        description: project.description,
        is_cloned: project.is_cloned,
        clone_path: project.clone_path,
        last_sync_at: project.last_sync_at.map(|dt| dt.to_rfc3339()),
        created_at: project.created_at.to_rfc3339(),
        default_test_command: req.default_test_command,
        default_build_command: req.default_build_command,
        index_status: Some("syncing".to_string()),
        indexed_file_count: 0,
        indexed_symbol_count: 0,
        indexed_import_count: 0,
        detected_languages: Vec::new(),
        detected_stack: Vec::new(),
        detected_test_command: None,
        detected_build_command: None,
        auto_sync_enabled,
        auto_sync_interval_minutes,
        last_auto_sync_at: None,
        last_sync_error: None,
    }))
}

pub(super) async fn update_repository(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Json(req): Json<RdRepositoryUpdateRequest>,
) -> Result<Json<RdRepositoryDto>, AppError> {
    if let Some(name) = &req.name {
        require_non_empty(name, "name")?;
    }
    if let Some(url) = &req.url {
        require_non_empty(url, "url")?;
    }
    let requested_auto_sync_interval = req
        .auto_sync_interval_minutes
        .map(|value| normalize_auto_sync_interval(Some(value)))
        .transpose()?;

    let current_setting =
        load_repo_setting(&state.db, &claims.tenant_id, &claims.sub, &repository_id)
            .await?
            .unwrap_or((None, None));
    let next_test = match &req.default_test_command {
        Some(value) => value
            .trim()
            .is_empty()
            .then_some(None)
            .unwrap_or_else(|| Some(value.trim().to_string())),
        None => current_setting.0,
    };
    let next_build = match &req.default_build_command {
        Some(value) => value
            .trim()
            .is_empty()
            .then_some(None)
            .unwrap_or_else(|| Some(value.trim().to_string())),
        None => current_setting.1,
    };

    let project = state
        .gitlab_manager()
        .update_project(
            &claims.tenant_id,
            &claims.sub,
            &repository_id,
            agent_gateway::UpdateProjectRequest {
                name: req.name,
                url: req.url,
                branch: req.branch,
                gitlab_token: req.gitlab_token,
                description: req.description,
            },
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if req.default_test_command.is_some() || req.default_build_command.is_some() {
        upsert_repo_settings(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &repository_id,
            next_test.as_deref(),
            next_build.as_deref(),
        )
        .await?;
    }
    if req.auto_sync_enabled.is_some() || requested_auto_sync_interval.is_some() {
        let current =
            load_repo_auto_sync_setting(&state.db, &claims.tenant_id, &claims.sub, &repository_id)
                .await?
                .unwrap_or((true, 60));
        upsert_repo_auto_sync_settings(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &repository_id,
            req.auto_sync_enabled.unwrap_or(current.0),
            requested_auto_sync_interval.unwrap_or(current.1),
        )
        .await?;
    }

    let setting = load_repo_settings_map(&state.db, &claims.tenant_id, &claims.sub)
        .await?
        .remove(&repository_id);
    let indexes = load_repo_index_map(&state.db, &claims.tenant_id).await?;
    Ok(Json(build_repository_dto(
        project,
        setting.as_ref(),
        indexes.get(&repository_id),
    )))
}

pub(super) async fn sync_repository(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    ensure_repository_exists(&state, &claims, &repository_id).await?;
    let indexes = load_repo_index_map(&state.db, &claims.tenant_id).await?;
    if indexes
        .get(&repository_id)
        .is_some_and(|index| index.status == "syncing")
    {
        return Ok(Json(json!({ "accepted": true, "status": "syncing" })));
    }
    mark_repository_index_status(
        &state.db,
        &claims.tenant_id,
        &repository_id,
        "syncing",
        None,
    )
    .await?;
    let worker_state = state.clone();
    let tenant_id = claims.tenant_id.clone();
    let user_id = claims.sub.clone();
    let worker_repository_id = repository_id.clone();
    tokio::spawn(async move {
        if let Err(error) = perform_repository_sync(
            &worker_state,
            &tenant_id,
            &user_id,
            &worker_repository_id,
            "repository_sync",
        )
        .await
        {
            let safe_error = runtime::protect_sensitive_text(
                &error.to_string(),
                runtime::configured_data_protection_mode(),
            )
            .value;
            let message = safe_error.chars().take(1000).collect::<String>();
            tracing::warn!(
                repository_id = %worker_repository_id,
                error = %message,
                "manual repository synchronization failed"
            );
            let _ = mark_repository_index_status(
                &worker_state.db,
                &tenant_id,
                &worker_repository_id,
                "failed",
                Some(&message),
            )
            .await;
            let _ = sqlx::query(
                "UPDATE rd_repository_settings SET last_sync_error = ?, updated_at = CURRENT_TIMESTAMP \
                 WHERE tenant_id = ? AND user_id = ? AND project_id = ?",
            )
            .bind(&message)
            .bind(&tenant_id)
            .bind(&user_id)
            .bind(&worker_repository_id)
            .execute(&worker_state.db)
            .await;
        }
    });
    Ok(Json(json!({ "accepted": true, "status": "syncing" })))
}

async fn perform_repository_sync(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    repository_id: &str,
    reason: &'static str,
) -> Result<Value, AppError> {
    let path = state
        .gitlab_manager()
        .sync_project(tenant_id, user_id, repository_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let file_count = count_repository_files(&path);
    let detection = detect_repository_profile(&path);
    // Do not use `try_join!` for concurrent database work. Its fail-fast
    // cancellation can drop a sibling query while rows are in flight,
    // leaving SQLx to retire that connection when its release ping sees the
    // unread packet.
    let (symbol_result, import_result, summary_result) = tokio::join!(
        rebuild_repository_symbol_index(&state.db, tenant_id, repository_id, &path),
        rebuild_repository_import_index(&state.db, tenant_id, repository_id, &path),
        rebuild_repository_file_summary_index(&state.db, tenant_id, repository_id, &path),
    );
    let symbol_count = symbol_result?;
    let import_count = import_result?;
    let summary_count = summary_result?;
    let context_summary_count = rebuild_repository_context_summary_index(
        &state.db,
        tenant_id,
        repository_id,
        &path,
        &detection,
    )
    .await?;
    upsert_repository_index(
        &state.db,
        tenant_id,
        repository_id,
        file_count,
        symbol_count,
        import_count,
        context_summary_count,
        Some(&detection),
    )
    .await?;
    fill_missing_repo_settings_from_detection(
        &state.db,
        tenant_id,
        user_id,
        repository_id,
        &detection,
    )
    .await?;
    schedule_rd_repository_embedding_index(
        state.clone(),
        tenant_id.to_string(),
        user_id.to_string(),
        repository_id.to_string(),
        reason,
    );
    schedule_rd_repository_llm_summary_refinement(
        state.clone(),
        tenant_id.to_string(),
        user_id.to_string(),
        repository_id.to_string(),
        reason,
    );
    sqlx::query("UPDATE rd_repository_settings SET last_sync_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE tenant_id = ? AND user_id = ? AND project_id = ?")
        .bind(tenant_id)
        .bind(user_id)
        .bind(repository_id)
        .execute(&state.db)
        .await?;
    Ok(json!({
        "synced": true,
        "clonePath": path.to_string_lossy(),
        "indexedFileCount": file_count,
        "symbolCount": symbol_count,
        "importCount": import_count,
        "summaryCount": summary_count,
        "contextSummaryCount": context_summary_count,
        "detection": detection,
    }))
}

async fn mark_repository_index_status(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), AppError> {
    let detail = error_message.map(|error| json!({ "error": error }));
    sqlx::query("INSERT INTO rd_repository_indexes (id, tenant_id, repository_id, status, file_count, symbol_count, detail_json) VALUES (?, ?, ?, ?, 0, 0, ?) ON CONFLICT(repository_id) DO UPDATE SET status = excluded.status, detail_json = excluded.detail_json, updated_at = CURRENT_TIMESTAMP")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(repository_id)
        .bind(status)
        .bind(detail)
        .execute(db)
        .await?;
    Ok(())
}

pub(super) async fn repository_tree(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
) -> Result<Json<Vec<RdFileNode>>, AppError> {
    let root = repository_root(&state, &claims, &repository_id).await?;
    let mut budget = MAX_TREE_ITEMS;
    let nodes = build_file_tree(&root, &root, &mut budget)?;
    Ok(Json(nodes))
}

pub(super) async fn repository_symbols(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Query(query): Query<RdRepositorySymbolQuery>,
) -> Result<Json<Vec<RdRepositorySymbolDto>>, AppError> {
    ensure_repository_exists(&state, &claims, &repository_id).await?;
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let rows = if let Some(q) = q {
        let like = format!("%{q}%");
        sqlx::query("SELECT id, file_path, language, symbol_name, symbol_kind, signature, line_number FROM rd_repository_symbols WHERE tenant_id = ? AND repository_id = ? AND (symbol_name LIKE ? OR file_path LIKE ? OR signature LIKE ?) ORDER BY symbol_name ASC, file_path ASC LIMIT ?")
            .bind(&claims.tenant_id)
            .bind(&repository_id)
            .bind(&like)
            .bind(&like)
            .bind(&like)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
    } else {
        sqlx::query("SELECT id, file_path, language, symbol_name, symbol_kind, signature, line_number FROM rd_repository_symbols WHERE tenant_id = ? AND repository_id = ? ORDER BY symbol_kind ASC, symbol_name ASC LIMIT ?")
            .bind(&claims.tenant_id)
            .bind(&repository_id)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
    };
    Ok(Json(rows.iter().map(row_to_repository_symbol).collect()))
}

pub(super) async fn repository_imports(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Query(query): Query<RdRepositoryImportQuery>,
) -> Result<Json<Vec<RdRepositoryImportDto>>, AppError> {
    ensure_repository_exists(&state, &claims, &repository_id).await?;
    let limit = i64::from(query.limit.unwrap_or(80).clamp(1, 300));
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let rows = if let Some(q) = q {
        let like = format!("%{q}%");
        sqlx::query("SELECT id, file_path, language, import_path, import_kind, line_number FROM rd_repository_imports WHERE tenant_id = ? AND repository_id = ? AND (import_path LIKE ? OR file_path LIKE ?) ORDER BY import_path ASC, file_path ASC LIMIT ?")
            .bind(&claims.tenant_id)
            .bind(&repository_id)
            .bind(&like)
            .bind(&like)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
    } else {
        sqlx::query("SELECT id, file_path, language, import_path, import_kind, line_number FROM rd_repository_imports WHERE tenant_id = ? AND repository_id = ? ORDER BY import_path ASC, file_path ASC LIMIT ?")
            .bind(&claims.tenant_id)
            .bind(&repository_id)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
    };
    Ok(Json(
        rows.iter()
            .map(row_to_repository_import)
            .filter(repository_import_is_plausible)
            .collect(),
    ))
}

pub(super) async fn repository_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Query(query): Query<RdRepositoryFileQuery>,
) -> Result<Json<RdFileContentResponse>, AppError> {
    let rel = query.path.unwrap_or_default();
    let root = repository_root(&state, &claims, &repository_id).await?;
    let path = safe_join(&root, &rel)?;
    let meta = std::fs::metadata(&path)?;
    if !meta.is_file() {
        return Err(AppError::ValidationError("path is not a file".to_string()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(AppError::PayloadTooLarge(format!(
            "file is too large to preview: {} bytes",
            meta.len()
        )));
    }
    let bytes = std::fs::read(&path)?;
    let content = String::from_utf8(bytes).map_err(|_| {
        AppError::ValidationError("binary file preview is not supported".to_string())
    })?;
    Ok(Json(RdFileContentResponse {
        path: rel,
        content,
        size_bytes: meta.len(),
        language: language_for_path(&path),
    }))
}

pub(super) async fn repository_branches(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    let branches = state
        .gitlab_manager()
        .list_branches(&claims.tenant_id, &claims.sub, &repository_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(json!({ "branches": branches })))
}

pub(super) async fn repository_worktree_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
) -> Result<Json<RdRepositoryWorktreeStatusDto>, AppError> {
    let root = repository_root(&state, &claims, &repository_id).await?;
    let status = read_rd_repository_worktree_status(&root, &repository_id).await?;
    Ok(Json(status))
}

pub(super) async fn repository_root(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
) -> Result<PathBuf, AppError> {
    let project = state
        .gitlab_manager()
        .get_project(&claims.tenant_id, &claims.sub, repository_id)
        .await
        .map_err(|e| AppError::NotFound(e.to_string()))?;
    let Some(path) = project.clone_path else {
        return Err(AppError::ValidationError(
            "repository has not been synced yet".to_string(),
        ));
    };
    let root = PathBuf::from(path);
    if !root.exists() || !root.is_dir() {
        return Err(AppError::ValidationError(
            "repository clone path is unavailable; sync it again".to_string(),
        ));
    }
    Ok(root)
}

pub(super) async fn ensure_repository_exists(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
) -> Result<(), AppError> {
    state
        .gitlab_manager()
        .get_project(&claims.tenant_id, &claims.sub, repository_id)
        .await
        .map_err(|e| AppError::NotFound(e.to_string()))?;
    Ok(())
}

fn build_file_tree(
    root: &Path,
    dir: &Path,
    budget: &mut usize,
) -> Result<Vec<RdFileNode>, AppError> {
    if *budget == 0 {
        return Ok(Vec::new());
    }
    let mut entries = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|entry| !should_skip_path(&entry.path()))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    let mut nodes = Vec::new();
    for entry in entries {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let meta = entry.metadata()?;
        if meta.is_dir() {
            nodes.push(RdFileNode {
                name: entry.file_name().to_string_lossy().to_string(),
                path: rel,
                node_type: "dir".to_string(),
                size_bytes: None,
                language: None,
                children: Some(build_file_tree(root, &path, budget)?),
            });
        } else if meta.is_file() {
            nodes.push(RdFileNode {
                name: entry.file_name().to_string_lossy().to_string(),
                path: rel,
                node_type: "file".to_string(),
                size_bytes: Some(meta.len()),
                language: language_for_path(&path),
                children: None,
            });
        }
    }
    Ok(nodes)
}

fn build_repository_dto(
    project: agent_gateway::GitlabProject,
    setting: Option<&RdRepositorySetting>,
    index: Option<&RdRepositoryIndexSnapshot>,
) -> RdRepositoryDto {
    let detection = index.and_then(|i| i.detection.as_ref());
    RdRepositoryDto {
        id: project.id,
        name: project.name,
        url: project.url,
        branch: project.branch,
        description: project.description,
        is_cloned: project.is_cloned,
        clone_path: project.clone_path,
        last_sync_at: project.last_sync_at.map(|dt| dt.to_rfc3339()),
        created_at: project.created_at.to_rfc3339(),
        default_test_command: setting.and_then(|s| s.default_test_command.clone()),
        default_build_command: setting.and_then(|s| s.default_build_command.clone()),
        index_status: index.map(|i| i.status.clone()),
        indexed_file_count: index.map_or(0, |i| i.file_count),
        indexed_symbol_count: index.map_or(0, |i| i.symbol_count),
        indexed_import_count: index.map_or(0, |i| i.import_count),
        detected_languages: detection.map(|d| d.languages.clone()).unwrap_or_default(),
        detected_stack: detection.map(|d| d.stack.clone()).unwrap_or_default(),
        detected_test_command: detection.and_then(|d| d.detected_test_command.clone()),
        detected_build_command: detection.and_then(|d| d.detected_build_command.clone()),
        auto_sync_enabled: setting.map_or(true, |s| s.auto_sync_enabled),
        auto_sync_interval_minutes: setting.map_or(60, |s| s.auto_sync_interval_minutes),
        // SQLite CURRENT_TIMESTAMP is UTC without an explicit offset. Expose
        // it as RFC3339 so browsers do not interpret it as local time (which
        // made a brand-new repository appear to have its last attempt 8 hours
        // ago in UTC+8 locales).
        last_auto_sync_at: setting
            .and_then(|s| s.last_auto_sync_at.as_deref())
            .and_then(sqlite_timestamp_to_rfc3339),
        last_sync_error: setting.and_then(|s| s.last_sync_error.clone()),
    }
}

fn sqlite_timestamp_to_rfc3339(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.contains('T') || value.ends_with('Z') || value.contains('+') {
        Some(value.to_string())
    } else {
        Some(format!("{}Z", value.replace(' ', "T")))
    }
}

async fn load_repo_settings_map(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
) -> Result<HashMap<String, RdRepositorySetting>, AppError> {
    let rows = sqlx::query("SELECT project_id, default_test_command, default_build_command, auto_sync_enabled, auto_sync_interval_minutes, CAST(last_auto_sync_at AS TEXT) last_auto_sync_at, last_sync_error FROM rd_repository_settings WHERE tenant_id = ? AND user_id = ?")
        .bind(tenant_id)
        .bind(user_id)
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("project_id"),
                RdRepositorySetting {
                    default_test_command: row.get("default_test_command"),
                    default_build_command: row.get("default_build_command"),
                    auto_sync_enabled: row.get("auto_sync_enabled"),
                    auto_sync_interval_minutes: row
                        .get::<i64, _>("auto_sync_interval_minutes")
                        .clamp(5, 10_080),
                    last_auto_sync_at: row.get("last_auto_sync_at"),
                    last_sync_error: row.get("last_sync_error"),
                },
            )
        })
        .collect())
}

async fn load_repo_index_map(
    db: &SqlitePool,
    tenant_id: &str,
) -> Result<HashMap<String, RdRepositoryIndexSnapshot>, AppError> {
    let rows = sqlx::query(
        "SELECT repository_id, status, file_count, symbol_count, detail_json FROM rd_repository_indexes WHERE tenant_id = ?",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let detail: Option<Value> = row.get("detail_json");
            let detection = detail
                .as_ref()
                .and_then(|value| value.get("detection"))
                .and_then(|value| {
                    serde_json::from_value::<RdRepositoryDetection>(value.clone()).ok()
                });
            let import_count = detail
                .as_ref()
                .and_then(|value| value.get("importCount"))
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0);
            (
                row.get::<String, _>("repository_id"),
                RdRepositoryIndexSnapshot {
                    status: row.get("status"),
                    file_count: row.get("file_count"),
                    symbol_count: row.get("symbol_count"),
                    import_count,
                    detection,
                },
            )
        })
        .collect())
}

async fn upsert_repo_settings(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    project_id: &str,
    test: Option<&str>,
    build: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO rd_repository_settings (project_id, tenant_id, user_id, default_test_command, default_build_command) VALUES (?, ?, ?, ?, ?) ON CONFLICT DO UPDATE SET default_test_command = excluded.default_test_command, default_build_command = excluded.default_build_command")
        .bind(project_id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(test.map(str::trim).filter(|v| !v.is_empty()))
        .bind(build.map(str::trim).filter(|v| !v.is_empty()))
        .execute(db)
        .await?;
    Ok(())
}

fn normalize_auto_sync_interval(value: Option<i64>) -> Result<i64, AppError> {
    let value = value.unwrap_or(60);
    if !(5..=10_080).contains(&value) {
        return Err(AppError::ValidationError(
            "auto_sync_interval_minutes must be between 5 and 10080".to_string(),
        ));
    }
    Ok(value)
}

async fn upsert_repo_auto_sync_settings(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    project_id: &str,
    enabled: bool,
    interval_minutes: i64,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO rd_repository_settings (project_id, tenant_id, user_id, auto_sync_enabled, auto_sync_interval_minutes) VALUES (?, ?, ?, ?, ?) ON CONFLICT DO UPDATE SET auto_sync_enabled = excluded.auto_sync_enabled, auto_sync_interval_minutes = excluded.auto_sync_interval_minutes, updated_at = CURRENT_TIMESTAMP")
        .bind(project_id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(enabled)
        .bind(interval_minutes)
        .execute(db)
        .await?;
    Ok(())
}

async fn load_repo_auto_sync_setting(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    project_id: &str,
) -> Result<Option<(bool, i64)>, AppError> {
    let row = sqlx::query("SELECT auto_sync_enabled, auto_sync_interval_minutes FROM rd_repository_settings WHERE tenant_id = ? AND user_id = ? AND project_id = ?")
        .bind(tenant_id)
        .bind(user_id)
        .bind(project_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|row| {
        (
            row.get("auto_sync_enabled"),
            row.get::<i64, _>("auto_sync_interval_minutes")
                .clamp(5, 10_080),
        )
    }))
}

pub(super) fn start_periodic_repository_sync(
    state: AppState,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let tick_seconds = std::env::var("RD_REPOSITORY_SYNC_TICK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60)
        .max(15);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move {
        if let Err(error) = recover_interrupted_repository_syncs(&state.db).await {
            tracing::warn!(%error, "failed to recover interrupted repository sync states");
        }
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(tick_seconds));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => run_due_repository_syncs(&state).await,
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        tracing::info!("periodic repository sync shutdown received");
                        break;
                    }
                }
            }
        }
    });
    (shutdown_tx, handle)
}

async fn recover_interrupted_repository_syncs(db: &SqlitePool) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE rd_repository_indexes \
         SET status = 'failed', \
             detail_json = json_set(COALESCE(detail_json, JSON_OBJECT()), '$.error', \
                 'AOS restarted while repository synchronization was running; retry synchronization'), \
             updated_at = CURRENT_TIMESTAMP \
         WHERE status = 'syncing'",
    )
    .execute(db)
    .await?;
    Ok(())
}

async fn run_due_repository_syncs(state: &AppState) {
    let rows = match sqlx::query("SELECT project_id, tenant_id, user_id FROM rd_repository_settings WHERE auto_sync_enabled = 1 AND (last_auto_sync_at IS NULL OR last_auto_sync_at <= datetime(CURRENT_TIMESTAMP, printf('-%d minutes', auto_sync_interval_minutes))) ORDER BY COALESCE(last_auto_sync_at, '1970-01-01 00:00:00') ASC LIMIT 10")
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "failed to load repositories due for automatic sync");
            return;
        }
    };
    for row in rows {
        let project_id: String = row.get("project_id");
        let tenant_id: String = row.get("tenant_id");
        let user_id: String = row.get("user_id");
        if let Err(error) = sqlx::query("UPDATE rd_repository_settings SET last_auto_sync_at = CURRENT_TIMESTAMP, last_sync_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE project_id = ? AND tenant_id = ? AND user_id = ?")
            .bind(&project_id)
            .bind(&tenant_id)
            .bind(&user_id)
            .execute(&state.db)
            .await
        {
            tracing::warn!(%error, %tenant_id, %project_id, "failed to mark automatic repository sync attempt");
            continue;
        }
        if let Err(error) = perform_repository_sync(
            state,
            &tenant_id,
            &user_id,
            &project_id,
            "repository_auto_sync",
        )
        .await
        {
            let safe_error = runtime::protect_sensitive_text(
                &error.to_string(),
                runtime::configured_data_protection_mode(),
            )
            .value;
            let message = safe_error.chars().take(1000).collect::<String>();
            let _ = sqlx::query("UPDATE rd_repository_settings SET last_sync_error = ?, updated_at = CURRENT_TIMESTAMP WHERE project_id = ? AND tenant_id = ? AND user_id = ?")
                .bind(&message)
                .bind(&project_id)
                .bind(&tenant_id)
                .bind(&user_id)
                .execute(&state.db)
                .await;
            tracing::warn!(error = %message, %tenant_id, %project_id, "automatic repository sync failed");
        }
    }
}

async fn fill_missing_repo_settings_from_detection(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    project_id: &str,
    detection: &RdRepositoryDetection,
) -> Result<(), AppError> {
    let current = load_repo_setting(db, tenant_id, user_id, project_id)
        .await?
        .unwrap_or((None, None));
    let test = current
        .0
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| detection.detected_test_command.clone());
    let build = current
        .1
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| detection.detected_build_command.clone());
    upsert_repo_settings(
        db,
        tenant_id,
        user_id,
        project_id,
        test.as_deref(),
        build.as_deref(),
    )
    .await
}

pub(super) async fn load_repo_setting(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    project_id: &str,
) -> Result<Option<(Option<String>, Option<String>)>, AppError> {
    let row = sqlx::query(
        "SELECT default_test_command, default_build_command FROM rd_repository_settings WHERE tenant_id = ? AND user_id = ? AND project_id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| {
        (
            r.get("default_test_command"),
            r.get("default_build_command"),
        )
    }))
}

async fn upsert_repository_index(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    file_count: i32,
    symbol_count: usize,
    import_count: usize,
    context_summary_count: usize,
    detection: Option<&RdRepositoryDetection>,
) -> Result<(), AppError> {
    let detail = json!({
        "detection": detection,
        "importCount": import_count,
        "contextSummaryCount": context_summary_count,
    });
    sqlx::query("INSERT INTO rd_repository_indexes (id, tenant_id, repository_id, status, file_count, symbol_count, detail_json, last_indexed_at) VALUES (?, ?, ?, 'ready', ?, ?, ?, CURRENT_TIMESTAMP) ON CONFLICT DO UPDATE SET status = 'ready', file_count = excluded.file_count, symbol_count = excluded.symbol_count, detail_json = json_patch(COALESCE(detail_json, JSON_OBJECT()), excluded.detail_json), last_indexed_at = CURRENT_TIMESTAMP")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(repository_id)
        .bind(file_count)
        .bind(i32::try_from(symbol_count).unwrap_or(i32::MAX))
        .bind(&detail)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod repository_auto_sync_tests {
    use super::{
        normalize_auto_sync_interval, recover_interrupted_repository_syncs,
        sqlite_timestamp_to_rfc3339,
    };

    #[test]
    fn sqlite_auto_sync_timestamp_is_explicitly_utc() {
        assert_eq!(
            sqlite_timestamp_to_rfc3339("2026-08-25 03:54:56").as_deref(),
            Some("2026-08-25T03:54:56Z")
        );
        assert_eq!(
            sqlite_timestamp_to_rfc3339("2026-08-25T03:54:56+00:00").as_deref(),
            Some("2026-08-25T03:54:56+00:00")
        );
    }

    #[test]
    fn repository_auto_sync_interval_is_bounded() {
        assert_eq!(normalize_auto_sync_interval(None).expect("default"), 60);
        assert_eq!(normalize_auto_sync_interval(Some(5)).expect("minimum"), 5);
        assert_eq!(
            normalize_auto_sync_interval(Some(10_080)).expect("maximum"),
            10_080
        );
        assert!(normalize_auto_sync_interval(Some(4)).is_err());
        assert!(normalize_auto_sync_interval(Some(10_081)).is_err());
    }

    #[tokio::test]
    async fn interrupted_repository_sync_is_recoverable_after_restart() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite");
        sqlx::query(
            "CREATE TABLE rd_repository_indexes (\
             id TEXT PRIMARY KEY, repository_id TEXT, status TEXT, detail_json TEXT, updated_at TEXT)",
        )
        .execute(&db)
        .await
        .expect("index schema");
        sqlx::query(
            "INSERT INTO rd_repository_indexes VALUES \
             ('index-1', 'repo-1', 'syncing', NULL, CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("syncing fixture");

        recover_interrupted_repository_syncs(&db)
            .await
            .expect("recover sync state");
        let (status, detail): (String, String) = sqlx::query_as(
            "SELECT status, CAST(detail_json AS TEXT) FROM rd_repository_indexes WHERE id = 'index-1'",
        )
        .fetch_one(&db)
        .await
        .expect("recovered row");
        assert_eq!(status, "failed");
        assert!(detail.contains("retry synchronization"));
    }
}

fn row_to_repository_symbol(row: &sqlx::sqlite::SqliteRow) -> RdRepositorySymbolDto {
    RdRepositorySymbolDto {
        id: row.get("id"),
        file_path: row.get("file_path"),
        language: row.get("language"),
        symbol_name: row.get("symbol_name"),
        symbol_kind: row.get("symbol_kind"),
        signature: row.get("signature"),
        line_number: row.get("line_number"),
    }
}

fn row_to_repository_import(row: &sqlx::sqlite::SqliteRow) -> RdRepositoryImportDto {
    RdRepositoryImportDto {
        id: row.get("id"),
        file_path: row.get("file_path"),
        language: row.get("language"),
        import_path: row.get("import_path"),
        import_kind: row.get("import_kind"),
        line_number: row.get("line_number"),
    }
}

fn repository_import_is_plausible(item: &RdRepositoryImportDto) -> bool {
    let inferred_language = item
        .language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| language_for_path(Path::new(&item.file_path)))
        .unwrap_or_else(|| "unknown".to_string());
    is_plausible_import_path(&inferred_language, &item.import_path)
}
