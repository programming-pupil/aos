use super::validate_data_source_access;
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::nl2sql::embedding::{cosine_similarity, EmbeddingModel};
use crate::state::AppState;
use axum::extract::{multipart::Multipart, DefaultBodyLimit, Extension, Json, Path, Query, State};
use axum::routing::{delete, get, patch, post};
use axum::Router;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqliteRow, QueryBuilder, Row};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

const MAX_REFERENCE_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_REFERENCE_BATCH_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 200;
const MAX_REFERENCE_CHARS: usize = 1_200_000;
const CHUNK_LINE_TARGET: usize = 80;
const CHUNK_LINE_OVERLAP: usize = 10;
const MAX_PROMPT_SNIPPETS: usize = 6;
const MAX_SEARCH_LIMIT: usize = 20;
const AUTO_REFERENCE_LIMIT: usize = 6;
const DEFAULT_SQL_KNOWLEDGE_STALE_AFTER_DAYS: u32 = 180;

fn sql_knowledge_full_example_max_chars() -> usize {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_FULL_EXAMPLE_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 4_000)
        .unwrap_or(24_000)
        .min(80_000)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceFileDto {
    pub id: String,
    pub pack_id: String,
    pub datasource_id: String,
    pub filename: String,
    pub media_type: Option<String>,
    pub language: Option<String>,
    pub size_bytes: u64,
    pub content_hash: String,
    pub status: String,
    pub error: Option<String>,
    pub summary: Option<String>,
    pub version_no: u64,
    pub metadata: Option<serde_json::Value>,
    pub chunk_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferencePackDto {
    pub id: String,
    pub datasource_id: String,
    pub datasource_bindings: Vec<String>,
    pub name: String,
    pub description: Option<String>,
    pub scope: String,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub verified: bool,
    pub stale: bool,
    pub knowledge_kind: String,
    pub metadata: Option<serde_json::Value>,
    pub file_count: u64,
    pub chunk_count: u64,
    pub writable: bool,
    pub files: Vec<ReferenceFileDto>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceImportTaskDto {
    pub id: String,
    pub pack_id: String,
    pub datasource_id: String,
    pub status: String,
    pub total_files: u64,
    pub processed_files: u64,
    pub failed_files: u64,
    pub current_filename: Option<String>,
    pub error_message: Option<String>,
    pub failure_details: Vec<serde_json::Value>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceImportManifestItem {
    filename: String,
    media_type: Option<String>,
    staged_filename: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceUsageDto {
    pub pack_id: String,
    pub pack_name: String,
    pub file_id: String,
    pub filename: String,
    pub chunk_id: String,
    pub language: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f64,
    pub reason: String,
    pub chunk_type: String,
    pub verified: bool,
    pub stale: bool,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReferencePromptSnippet {
    pub pack_id: String,
    pub pack_name: String,
    pub file_id: String,
    pub filename: String,
    pub chunk_id: String,
    pub language: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f64,
    pub reason: String,
    pub chunk_type: String,
    pub verified: bool,
    pub stale: bool,
    pub content: String,
}

impl ReferencePromptSnippet {
    pub(crate) fn to_usage_dto(&self) -> ReferenceUsageDto {
        ReferenceUsageDto {
            pack_id: self.pack_id.clone(),
            pack_name: self.pack_name.clone(),
            file_id: self.file_id.clone(),
            filename: self.filename.clone(),
            chunk_id: self.chunk_id.clone(),
            language: self.language.clone(),
            start_line: self.start_line,
            end_line: self.end_line,
            score: self.score,
            reason: self.reason.clone(),
            chunk_type: self.chunk_type.clone(),
            verified: self.verified,
            stale: self.stale,
            preview: preview_chars(&self.content, 520),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceBindingRequest {
    #[serde(default)]
    pub pack_ids: Vec<String>,
    #[serde(default)]
    pub file_ids: Vec<String>,
    #[serde(default)]
    pub include_all: bool,
}

impl ReferenceBindingRequest {
    pub(crate) fn is_active(&self) -> bool {
        self.include_all || !self.pack_ids.is_empty() || !self.file_ids.is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateReferencePackRequest {
    #[serde(alias = "data_source_id")]
    pub datasource_id: String,
    pub name: String,
    pub description: Option<String>,
    pub scope: Option<String>,
    pub tags: Option<Vec<String>>,
    #[serde(default, alias = "datasource_bindings")]
    pub datasource_bindings: Vec<String>,
    pub verified: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateReferencePackRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub tags: Option<Vec<String>>,
    #[serde(default, alias = "datasource_bindings")]
    pub datasource_bindings: Option<Vec<String>>,
    pub verified: Option<bool>,
    pub stale: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ListReferencePacksQuery {
    pub datasource_id: Option<String>,
    pub include_global: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReferenceSearchRequest {
    #[serde(alias = "data_source_id")]
    pub datasource_id: String,
    pub question: String,
    #[serde(default, alias = "reference_bindings")]
    pub reference_bindings: ReferenceBindingRequest,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReferenceSearchResponse {
    pub references: Vec<ReferenceUsageDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteReferenceResponse {
    pub deleted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListSqlKnowledgeQuery {
    pub datasource_id: Option<String>,
    pub include_global: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateSqlKnowledgeSpaceRequest {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub datasource_ids: Vec<String>,
    #[serde(default)]
    pub global: bool,
    pub tags: Option<Vec<String>>,
    pub verified: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSqlKnowledgeSpaceRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    #[serde(default)]
    pub datasource_ids: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub verified: Option<bool>,
    pub stale: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SqlKnowledgeSearchRequest {
    pub question: String,
    pub datasource_id: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SqlKnowledgeSearchResponse {
    pub references: Vec<ReferenceUsageDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UploadReferenceFilesResponse {
    pub files: Vec<ReferenceFileDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSqlKnowledgeFileRequest {
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KnowledgeReadQuery {
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KnowledgeReadResponse {
    pub file_id: String,
    pub filename: String,
    pub start_line: u32,
    pub end_line: u32,
    pub content: String,
}

#[derive(Debug)]
struct ReferenceFileRecord {
    id: String,
    pack_id: String,
    datasource_id: String,
    filename: String,
    media_type: Option<String>,
    language: Option<String>,
    size_bytes: u64,
    content_hash: String,
    storage_path: String,
    status: String,
    error: Option<String>,
    summary: Option<String>,
    version_no: u64,
    metadata: Option<serde_json::Value>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct CandidateChunk {
    pack_id: String,
    pack_name: String,
    file_id: String,
    filename: String,
    chunk_id: String,
    chunk_index: u32,
    language: Option<String>,
    start_line: u32,
    end_line: u32,
    content: String,
    keywords: Option<String>,
    chunk_type: String,
    summary: Option<String>,
    metadata: Option<serde_json::Value>,
    extracted_tables: Vec<String>,
    extracted_columns: Vec<String>,
    extracted_metrics: Vec<String>,
    embedding_model: Option<String>,
    embedding: Option<Vec<f32>>,
    fulltext_score: f64,
    verified: bool,
    stale: bool,
    file_age_days: Option<u32>,
    pack_scope: String,
}

struct ReferenceProfileVectors {
    profile: crate::nl2sql::embedding_profiles::ResolvedProfile,
    vectors: Vec<Vec<f32>>,
}

struct ReferenceEmbeddingBatch {
    local: ReferenceProfileVectors,
    api: Option<ReferenceProfileVectors>,
}

struct ReferenceQueryEmbedding {
    profile_id: String,
    vector: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceDatasourceBindingPolicy {
    visibility: String,
    owner_user_id: Option<String>,
}

fn validate_reference_binding_policy_set(
    policies: &[ReferenceDatasourceBindingPolicy],
) -> Result<String> {
    let Some(first) = policies.first() else {
        return Err(AppError::ValidationError(
            "knowledge spaces must bind at least one data source".into(),
        ));
    };
    if first.visibility != "tenant" && first.visibility != "private" {
        return Err(AppError::ValidationError(
            "data source visibility must be tenant or private".into(),
        ));
    }
    if policies
        .iter()
        .any(|policy| policy.visibility != first.visibility)
    {
        return Err(AppError::ValidationError(
            "a knowledge space cannot mix tenant and private data sources".into(),
        ));
    }
    if first.visibility == "private"
        && (first.owner_user_id.is_none()
            || policies
                .iter()
                .any(|policy| policy.owner_user_id != first.owner_user_id))
    {
        return Err(AppError::ValidationError(
            "a knowledge space cannot bind private data sources owned by different users".into(),
        ));
    }
    Ok(first.visibility.clone())
}

async fn validate_reference_datasource_bindings(
    state: &AppState,
    claims: &Claims,
    datasource_ids: &[String],
) -> Result<String> {
    let mut policies = Vec::with_capacity(datasource_ids.len());
    for datasource_id in datasource_ids {
        if datasource_id == "global" {
            return Err(AppError::ValidationError(
                "the global selector cannot be combined with a specific data source".into(),
            ));
        }
        validate_data_source_access(
            state,
            &claims.tenant_id,
            &claims.sub,
            &claims.role,
            datasource_id,
        )
        .await?;
        let row = sqlx::query(
            "SELECT user_id, visibility FROM data_sources \
             WHERE tenant_id = ? AND id = ? AND deleted_at IS NULL",
        )
        .bind(&claims.tenant_id)
        .bind(datasource_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("data source not found".into()))?;
        policies.push(ReferenceDatasourceBindingPolicy {
            owner_user_id: row.get("user_id"),
            visibility: row.get("visibility"),
        });
    }
    validate_reference_binding_policy_set(&policies)
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/reference/packs", get(list_reference_packs))
        .route("/reference/packs", post(create_reference_pack))
        .route("/reference/packs/{pack_id}", patch(update_reference_pack))
        .route("/reference/packs/{pack_id}", delete(delete_reference_pack))
        .route(
            "/reference/packs/{pack_id}/files",
            post(upload_reference_file).layer(DefaultBodyLimit::max(
                MAX_REFERENCE_UPLOAD_BYTES + 512 * 1024,
            )),
        )
        .route("/reference/files/{file_id}", delete(delete_reference_file))
        .route("/reference/search", post(search_references))
        .route("/sql-knowledge/spaces", get(list_sql_knowledge_spaces))
        .route("/sql-knowledge/spaces", post(create_sql_knowledge_space))
        .route(
            "/sql-knowledge/spaces/{space_id}",
            patch(update_sql_knowledge_space),
        )
        .route(
            "/sql-knowledge/spaces/{space_id}",
            delete(delete_reference_pack),
        )
        .route(
            "/sql-knowledge/spaces/{space_id}/files",
            post(upload_sql_knowledge_files).layer(DefaultBodyLimit::max(
                MAX_REFERENCE_UPLOAD_BYTES + 512 * 1024,
            )),
        )
        .route(
            "/sql-knowledge/spaces/{space_id}/import-tasks",
            get(list_sql_knowledge_import_tasks)
                .post(create_sql_knowledge_import_task)
                .layer(DefaultBodyLimit::max(
                    MAX_REFERENCE_BATCH_UPLOAD_BYTES + 1024 * 1024,
                )),
        )
        .route(
            "/sql-knowledge/files/{file_id}",
            delete(delete_reference_file),
        )
        .route(
            "/sql-knowledge/files/{file_id}",
            patch(update_sql_knowledge_file),
        )
        .route(
            "/sql-knowledge/files/{file_id}/read",
            get(read_sql_knowledge_file),
        )
        .route("/sql-knowledge/search", post(search_sql_knowledge))
}

pub(crate) async fn list_reference_packs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ListReferencePacksQuery>,
) -> Result<Json<Vec<ReferencePackDto>>> {
    if let Some(datasource_id) = params.datasource_id.as_deref() {
        validate_data_source_access(
            &state,
            &claims.tenant_id,
            &claims.sub,
            &claims.role,
            datasource_id,
        )
        .await?;
    }

    let packs = load_reference_packs(
        &state.db,
        &claims.tenant_id,
        params.datasource_id.as_deref(),
        params.include_global.unwrap_or(false),
        &claims.sub,
        claims.role == "admin" || claims.role == "superadmin",
    )
    .await?;
    Ok(Json(packs))
}

pub(crate) async fn create_reference_pack(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateReferencePackRequest>,
) -> Result<Json<ReferencePackDto>> {
    let bindings = if req.datasource_bindings.is_empty() {
        vec![req.datasource_id.clone()]
    } else {
        req.datasource_bindings.clone()
    };
    let pack = create_pack_record(
        &state,
        &claims,
        &req.datasource_id,
        &bindings,
        req.scope.as_deref().unwrap_or("datasource"),
        &req.name,
        req.description.as_deref(),
        req.tags.unwrap_or_default(),
        req.verified.unwrap_or(false),
    )
    .await?;
    Ok(Json(pack))
}

pub(crate) async fn update_reference_pack(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pack_id): Path<String>,
    Json(req): Json<UpdateReferencePackRequest>,
) -> Result<Json<ReferencePackDto>> {
    let (mut datasource_id, scope) = require_pack_write_access(&state, &claims, &pack_id).await?;

    if req.name.is_none()
        && req.description.is_none()
        && req.enabled.is_none()
        && req.tags.is_none()
        && req.datasource_bindings.is_none()
        && req.verified.is_none()
        && req.stale.is_none()
    {
        return Err(AppError::ValidationError("no fields to update".into()));
    }

    let mut qb: QueryBuilder<sqlx::Sqlite> =
        QueryBuilder::new("UPDATE nl2sql_reference_packs SET ");
    let mut has_update = false;
    if let Some(name) = req.name.as_ref() {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(AppError::ValidationError(
                "reference pack name cannot be empty".into(),
            ));
        }
        if has_update {
            qb.push(", ");
        }
        has_update = true;
        qb.push("name = ").push_bind(trimmed.to_string());
    }
    if let Some(description) = req.description.as_ref() {
        if has_update {
            qb.push(", ");
        }
        has_update = true;
        qb.push("description = ").push_bind(
            description
                .trim()
                .is_empty()
                .then_some(None::<String>)
                .unwrap_or_else(|| Some(description.trim().to_string())),
        );
    }
    if let Some(enabled) = req.enabled {
        if has_update {
            qb.push(", ");
        }
        has_update = true;
        qb.push("enabled = ")
            .push_bind(if enabled { 1i32 } else { 0i32 });
    }
    if let Some(tags) = req.tags {
        if has_update {
            qb.push(", ");
        }
        has_update = true;
        qb.push("tags_json = ")
            .push_bind(serde_json::to_value(normalize_tags(tags))?);
    }
    if let Some(bindings) = req.datasource_bindings.as_ref() {
        let normalized = normalize_ids(bindings);
        if scope == "tenant" && !normalized.is_empty() {
            return Err(AppError::ValidationError(
                "tenant-global knowledge spaces cannot bind a specific data source".into(),
            ));
        }
        if scope != "tenant" {
            validate_reference_datasource_bindings(&state, &claims, &normalized).await?;
            datasource_id = normalized.first().cloned().ok_or_else(|| {
                AppError::ValidationError(
                    "knowledge spaces must bind at least one data source".into(),
                )
            })?;
        }
        if has_update {
            qb.push(", ");
        }
        has_update = true;
        qb.push("datasource_id = ")
            .push_bind(&datasource_id)
            .push(", datasource_bindings_json = ")
            .push_bind(serde_json::to_value(&normalized)?);
    }
    if let Some(verified) = req.verified {
        if has_update {
            qb.push(", ");
        }
        has_update = true;
        qb.push("verified = ")
            .push_bind(if verified { 1i32 } else { 0i32 });
    }
    if let Some(stale) = req.stale {
        if has_update {
            qb.push(", ");
        }
        has_update = true;
        qb.push("stale = ")
            .push_bind(if stale { 1i32 } else { 0i32 });
    }
    if !has_update {
        return Err(AppError::ValidationError("no fields to update".into()));
    }
    qb.push(" WHERE tenant_id = ")
        .push_bind(&claims.tenant_id)
        .push(" AND id = ")
        .push_bind(&pack_id);
    qb.build().execute(&state.db).await?;

    let packs = load_reference_packs(
        &state.db,
        &claims.tenant_id,
        Some(&datasource_id),
        true,
        &claims.sub,
        claims.role == "admin" || claims.role == "superadmin",
    )
    .await?;
    let pack = packs
        .into_iter()
        .find(|p| p.id == pack_id)
        .ok_or_else(|| AppError::NotFound("reference pack not found".into()))?;
    Ok(Json(pack))
}

pub(crate) async fn delete_reference_pack(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pack_id): Path<String>,
) -> Result<Json<DeleteReferenceResponse>> {
    require_pack_write_access(&state, &claims, &pack_id).await?;

    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_reference_chunks WHERE tenant_id = ? AND pack_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&pack_id)
    .execute(&state.db)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_reference_files WHERE tenant_id = ? AND pack_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&pack_id)
    .execute(&state.db)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_reference_packs WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&pack_id)
    .execute(&state.db)
    .await?;
    let _ =
        tokio::fs::remove_dir_all(reference_pack_dir(&state, &claims.tenant_id, &pack_id)).await;
    Ok(Json(DeleteReferenceResponse { deleted: true }))
}

pub(crate) async fn upload_reference_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pack_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<ReferenceFileDto>> {
    let (datasource_id, _) = require_pack_write_access(&state, &claims, &pack_id).await?;

    let uploads = collect_multipart_reference_uploads(&mut multipart).await?;
    let first = uploads
        .into_iter()
        .next()
        .ok_or_else(|| AppError::ValidationError("missing file field".into()))?;
    let file = index_reference_upload(
        &state,
        &claims,
        &pack_id,
        &datasource_id,
        first.filename,
        first.media_type,
        first.bytes,
    )
    .await?;
    Ok(Json(file))
}

pub(crate) async fn delete_reference_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(file_id): Path<String>,
) -> Result<Json<DeleteReferenceResponse>> {
    let file = load_reference_file_record(&state.db, &claims.tenant_id, &file_id).await?;
    require_pack_write_access(&state, &claims, &file.pack_id).await?;
    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_reference_chunks WHERE tenant_id = ? AND file_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&file_id)
    .execute(&state.db)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM nl2sql_reference_files WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&file_id)
    .execute(&state.db)
    .await?;
    if !file.storage_path.trim().is_empty() {
        let _ = tokio::fs::remove_file(file.storage_path).await;
    }
    Ok(Json(DeleteReferenceResponse { deleted: true }))
}

pub(crate) async fn search_references(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ReferenceSearchRequest>,
) -> Result<Json<ReferenceSearchResponse>> {
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &req.datasource_id,
    )
    .await?;
    let snippets = resolve_query_references(
        &state,
        &claims.tenant_id,
        &req.datasource_id,
        &req.question,
        Some(&req.reference_bindings),
        req.limit
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(MAX_SEARCH_LIMIT)
            .min(MAX_SEARCH_LIMIT),
    )
    .await?;
    Ok(Json(ReferenceSearchResponse {
        references: snippets
            .iter()
            .map(ReferencePromptSnippet::to_usage_dto)
            .collect(),
    }))
}

pub(crate) async fn list_sql_knowledge_spaces(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ListSqlKnowledgeQuery>,
) -> Result<Json<Vec<ReferencePackDto>>> {
    if let Some(datasource_id) = params.datasource_id.as_deref() {
        validate_data_source_access(
            &state,
            &claims.tenant_id,
            &claims.sub,
            &claims.role,
            datasource_id,
        )
        .await?;
    }
    let spaces = load_reference_packs(
        &state.db,
        &claims.tenant_id,
        params.datasource_id.as_deref(),
        params.include_global.unwrap_or(true),
        &claims.sub,
        claims.role == "admin" || claims.role == "superadmin",
    )
    .await?;
    Ok(Json(spaces))
}

pub(crate) async fn create_sql_knowledge_space(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateSqlKnowledgeSpaceRequest>,
) -> Result<Json<ReferencePackDto>> {
    super::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;
    let bindings = normalize_ids(&req.datasource_ids);
    if !req.global && bindings.is_empty() {
        return Err(AppError::ValidationError(
            "SQL 知识库需要至少绑定一个数据源，或选择全局知识空间".into(),
        ));
    }
    if req.global {
        let is_admin = claims.role == "admin" || claims.role == "superadmin";
        if !is_admin {
            return Err(AppError::Forbidden);
        }
        if !bindings.is_empty() {
            return Err(AppError::ValidationError(
                "tenant-global knowledge spaces apply to the whole tenant and cannot bind a specific data source".into(),
            ));
        }
    }
    let primary_ds = bindings
        .first()
        .cloned()
        .unwrap_or_else(|| "global".to_string());
    let scope = if req.global { "tenant" } else { "datasource" };
    let space = create_pack_record(
        &state,
        &claims,
        &primary_ds,
        &bindings,
        scope,
        &req.name,
        req.description.as_deref(),
        req.tags.unwrap_or_default(),
        req.verified.unwrap_or(false),
    )
    .await?;
    Ok(Json(space))
}

pub(crate) async fn update_sql_knowledge_space(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(space_id): Path<String>,
    Json(req): Json<UpdateSqlKnowledgeSpaceRequest>,
) -> Result<Json<ReferencePackDto>> {
    require_pack_write_access(&state, &claims, &space_id).await?;
    let update = UpdateReferencePackRequest {
        name: req.name,
        description: req.description,
        enabled: req.enabled,
        tags: req.tags,
        datasource_bindings: req.datasource_ids,
        verified: req.verified,
        stale: req.stale,
    };
    update_reference_pack(
        State(state),
        Extension(claims),
        Path(space_id),
        Json(update),
    )
    .await
}

pub(crate) async fn upload_sql_knowledge_files(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(space_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<UploadReferenceFilesResponse>> {
    super::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;
    let (datasource_id, _) = require_pack_write_access(&state, &claims, &space_id).await?;
    let uploads = collect_multipart_reference_uploads(&mut multipart).await?;
    if uploads.is_empty() {
        return Err(AppError::ValidationError("missing file field".into()));
    }
    let mut files = Vec::new();
    for upload in uploads {
        files.push(
            index_reference_upload(
                &state,
                &claims,
                &space_id,
                &datasource_id,
                upload.filename,
                upload.media_type,
                upload.bytes,
            )
            .await?,
        );
    }
    Ok(Json(UploadReferenceFilesResponse { files }))
}

pub(crate) async fn create_sql_knowledge_import_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(space_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<ReferenceImportTaskDto>> {
    super::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;
    let (datasource_id, _) = require_pack_write_access(&state, &claims, &space_id).await?;
    let task_id = format!("nlref-import-{}", uuid::Uuid::new_v4());
    let staging_dir = reference_import_staging_dir(&state, &claims.tenant_id, &task_id);
    tokio::fs::create_dir_all(&staging_dir).await?;

    let mut manifest = Vec::new();
    let mut total_bytes = 0usize;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::ValidationError(format!("invalid multipart/form-data: {e}")))?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name != "file" && field_name != "files" {
            continue;
        }
        let filename = field
            .file_name()
            .map(safe_reference_upload_name)
            .unwrap_or_else(|| "reference.txt".to_string());
        let media_type = field.content_type().map(ToString::to_string);
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::ValidationError(format!("failed to read upload: {e}")))?;
        if data.len() > MAX_REFERENCE_UPLOAD_BYTES {
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
            return Err(AppError::PayloadTooLarge(format!(
                "reference file exceeds {} bytes",
                MAX_REFERENCE_UPLOAD_BYTES
            )));
        }
        total_bytes = total_bytes.saturating_add(data.len());
        if total_bytes > MAX_REFERENCE_BATCH_UPLOAD_BYTES {
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
            return Err(AppError::PayloadTooLarge(format!(
                "SQL knowledge import exceeds {} bytes",
                MAX_REFERENCE_BATCH_UPLOAD_BYTES
            )));
        }

        let uploads = if is_zip_filename(&filename) {
            extract_zip_reference_uploads(&data)?
        } else {
            vec![ReferenceUpload {
                filename,
                media_type,
                bytes: data.to_vec(),
            }]
        };
        for upload in uploads {
            if manifest.len() >= MAX_ARCHIVE_FILES {
                let _ = tokio::fs::remove_dir_all(&staging_dir).await;
                return Err(AppError::PayloadTooLarge(format!(
                    "too many files in SQL knowledge upload; max {}",
                    MAX_ARCHIVE_FILES
                )));
            }
            let staged_filename = format!("{:04}.upload", manifest.len());
            tokio::fs::write(staging_dir.join(&staged_filename), upload.bytes).await?;
            manifest.push(ReferenceImportManifestItem {
                filename: upload.filename,
                media_type: upload.media_type,
                staged_filename,
            });
        }
    }
    if manifest.is_empty() {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(AppError::ValidationError("missing file field".into()));
    }

    let mut tx = state.db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let active_task_id = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT id FROM nl2sql_reference_import_tasks \
         WHERE tenant_id = ? AND user_id = ? AND pack_id = ? \
           AND status IN ('pending', 'running') \
         ORDER BY created_at ASC LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&space_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(active_task_id) = active_task_id {
        tx.rollback().await?;
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            pack_id = %space_id,
            active_task_id,
            "duplicate SQL knowledge import rejected while another batch is active"
        );
        return Err(AppError::Conflict(
            "another SQL knowledge import is already running for this space".into(),
        ));
    }
    let insert_result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_reference_import_tasks \
         (id, tenant_id, user_id, pack_id, datasource_id, status, total_files, manifest_json, staging_dir) \
         VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?)",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&space_id)
    .bind(&datasource_id)
    .bind(i64::try_from(manifest.len()).unwrap_or(i64::MAX))
    .bind(serde_json::to_string(&manifest)?)
    .bind(staging_dir.to_string_lossy().as_ref())
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert_result {
        let _ = tx.rollback().await;
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(error.into());
    }
    tx.commit().await?;
    tracing::info!(
        tenant_id = %claims.tenant_id,
        pack_id = %space_id,
        task_id = %task_id,
        files = manifest.len(),
        "SQL knowledge import queued"
    );
    Ok(Json(
        load_reference_import_task(&state.db, &claims.tenant_id, &claims.sub, &task_id).await?,
    ))
}

pub(crate) async fn list_sql_knowledge_import_tasks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(space_id): Path<String>,
) -> Result<Json<Vec<ReferenceImportTaskDto>>> {
    require_pack_write_access(&state, &claims, &space_id).await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, pack_id, datasource_id, status, total_files, processed_files, failed_files, \
                current_filename, error_message, failure_details_json, \
                CAST(created_at AS TEXT) created_at, CAST(started_at AS TEXT) started_at, \
                CAST(completed_at AS TEXT) completed_at, CAST(updated_at AS TEXT) updated_at \
         FROM nl2sql_reference_import_tasks \
         WHERE tenant_id = ? AND user_id = ? AND pack_id = ? \
         ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END, \
                  created_at DESC LIMIT 20",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&space_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(reference_import_task_from_row)
            .collect(),
    ))
}

pub(crate) async fn update_sql_knowledge_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(file_id): Path<String>,
    Json(req): Json<UpdateSqlKnowledgeFileRequest>,
) -> Result<Json<ReferenceFileDto>> {
    super::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;
    let file = load_reference_file_record(&state.db, &claims.tenant_id, &file_id).await?;
    require_pack_write_access(&state, &claims, &file.pack_id).await?;

    let text = req.content.replace("\r\n", "\n").replace('\r', "\n");
    let bytes = text.as_bytes().to_vec();
    if text.trim().is_empty() {
        return Err(AppError::ValidationError(
            "SQL knowledge file content cannot be empty".into(),
        ));
    }
    if bytes.len() > MAX_REFERENCE_UPLOAD_BYTES {
        return Err(AppError::PayloadTooLarge(format!(
            "reference file exceeds {} bytes",
            MAX_REFERENCE_UPLOAD_BYTES
        )));
    }
    if text.chars().count() > MAX_REFERENCE_CHARS {
        return Err(AppError::PayloadTooLarge(format!(
            "reference text exceeds {} characters",
            MAX_REFERENCE_CHARS
        )));
    }

    let content_hash = sha256_hex_bytes(&bytes);
    if content_hash == file.content_hash {
        return Ok(Json(
            load_reference_file(&state.db, &claims.tenant_id, &file.id).await?,
        ));
    }
    if let Some(existing_file_id) = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT id FROM nl2sql_reference_files \
         WHERE tenant_id = ? AND pack_id = ? AND content_hash = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&file.pack_id)
    .bind(&content_hash)
    .fetch_optional(&state.db)
    .await?
    {
        if existing_file_id != file.id {
            return Err(AppError::ValidationError(
                "same SQL knowledge content already exists in this space".into(),
            ));
        }
    }

    let storage_path = if file.storage_path.trim().is_empty() {
        reference_pack_dir(&state, &claims.tenant_id, &file.pack_id)
            .join(&file.id)
            .join(safe_filename(&file.filename))
    } else {
        PathBuf::from(&file.storage_path)
    };
    assert_storage_path_safe(&state, &claims.tenant_id, &file.pack_id, &storage_path).await?;
    tokio::fs::write(&storage_path, &bytes).await?;

    let language = infer_language(&file.filename);
    let summary = summarize_reference_text(&text);
    let chunks = chunk_reference_text(&text, language.as_deref());
    let embedding_inputs: Vec<String> = chunks
        .iter()
        .map(|chunk| build_embedding_input(&file.filename, chunk))
        .collect();
    let embedding_batch = embed_reference_batch(
        &state,
        &claims.tenant_id,
        &file.datasource_id,
        &embedding_inputs,
    )
    .await?;

    let file_metadata = serde_json::json!({
        "source": "sql_knowledge",
        "tables": extract_sql_table_names(&text),
        "metrics": extract_metric_terms(&text),
        "chunkCount": chunks.len(),
        "editedFromUi": true,
    });
    let storage_path_text = storage_path.to_string_lossy().to_string();
    let mut last_write_error: Option<AppError> = None;
    for attempt in 1..=3 {
        let write_result: Result<()> = async {
            let mut tx = state.db.begin().await?;
            sqlx::query::<sqlx::Sqlite>("DELETE FROM nl2sql_reference_chunks WHERE tenant_id = ? AND file_id = ?")
                .bind(&claims.tenant_id)
                .bind(&file.id)
                .execute(&mut *tx)
                .await?;

            for (index, chunk) in chunks.iter().enumerate() {
                let embedding = &embedding_batch.local.vectors[index];
                let chunk_id = format!("nlref-chunk-{}", uuid::Uuid::new_v4());
                let metadata = serde_json::json!({
                    "tables": chunk.tables,
                    "columns": chunk.columns,
                    "metrics": chunk.metrics,
                    "joins": extract_join_hints(&chunk.content),
                    "codexLikeTool": "knowledge_read",
                });
                sqlx::query::<sqlx::Sqlite>(
                    "INSERT INTO nl2sql_reference_chunks \
                     (id, tenant_id, datasource_id, pack_id, file_id, chunk_index, language, chunk_type, start_line, end_line, content_text, content_hash, token_count, keywords_text, summary_text, extracted_tables_json, extracted_columns_json, extracted_metrics_json, metadata_json, embedding_model, embedding_dimensions, embedding_json) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&chunk_id)
                .bind(&claims.tenant_id)
                .bind(&file.datasource_id)
                .bind(&file.pack_id)
                .bind(&file.id)
                .bind(u32::try_from(chunk.index).unwrap_or(u32::MAX))
                .bind(language.as_deref())
                .bind(&chunk.chunk_type)
                .bind(u32::try_from(chunk.start_line).unwrap_or(u32::MAX))
                .bind(u32::try_from(chunk.end_line).unwrap_or(u32::MAX))
                .bind(&chunk.content)
                .bind(sha256_hex(chunk.content.as_str()))
                .bind(u32::try_from(estimate_token_count(&chunk.content)).unwrap_or(u32::MAX))
                .bind(chunk.keywords.as_deref())
                .bind(chunk.summary.as_deref())
                .bind(serde_json::to_value(&chunk.tables)?)
                .bind(serde_json::to_value(&chunk.columns)?)
                .bind(serde_json::to_value(&chunk.metrics)?)
                .bind(metadata)
                .bind(&embedding_batch.local.profile.config.model)
                .bind(i32::try_from(embedding.len()).unwrap_or(i32::MAX))
                .bind(serde_json::to_string(embedding)?)
                .execute(&mut *tx)
                .await?;

                insert_reference_profile_embedding(
                    &mut tx,
                    &claims.tenant_id,
                    &chunk_id,
                    &embedding_batch.local,
                    index,
                )
                .await?;
                if let Some(api) = &embedding_batch.api {
                    insert_reference_profile_embedding(
                        &mut tx,
                        &claims.tenant_id,
                        &chunk_id,
                        api,
                        index,
                    )
                    .await?;
                }
            }

            sqlx::query::<sqlx::Sqlite>(
                "UPDATE nl2sql_reference_files \
                 SET language = ?, size_bytes = ?, content_hash = ?, storage_path = ?, \
                     status = 'indexed', error = NULL, summary = ?, metadata_json = ?, version_no = version_no + 1 \
                 WHERE tenant_id = ? AND id = ?",
            )
            .bind(language.as_deref())
            .bind(i64::try_from(bytes.len()).unwrap_or(i64::MAX))
            .bind(&content_hash)
            .bind(&storage_path_text)
            .bind(summary.as_deref())
            .bind(file_metadata.clone())
            .bind(&claims.tenant_id)
            .bind(&file.id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;

        match write_result {
            Ok(()) => {
                last_write_error = None;
                break;
            }
            Err(err) if attempt < 3 && is_sqlite_transient_write_error(&err) => {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    file_id = %file.id,
                    attempt,
                    error = %err,
                    "SQL knowledge file edit hit transient database lock; retrying"
                );
                last_write_error = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(150 * attempt as u64)).await;
            }
            Err(err) => return Err(err),
        }
    }
    if let Some(err) = last_write_error {
        return Err(err);
    }
    Ok(Json(
        load_reference_file(&state.db, &claims.tenant_id, &file.id).await?,
    ))
}

pub(crate) async fn search_sql_knowledge(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SqlKnowledgeSearchRequest>,
) -> Result<Json<SqlKnowledgeSearchResponse>> {
    super::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;
    if let Some(datasource_id) = req.datasource_id.as_deref() {
        validate_data_source_access(
            &state,
            &claims.tenant_id,
            &claims.sub,
            &claims.role,
            datasource_id,
        )
        .await?;
    }
    let snippets = resolve_auto_query_references(
        &state,
        &claims.tenant_id,
        req.datasource_id.as_deref().unwrap_or("global"),
        &req.question,
        req.limit
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(MAX_SEARCH_LIMIT)
            .min(MAX_SEARCH_LIMIT),
    )
    .await?;
    persist_sql_knowledge_usage_events(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        req.datasource_id.as_deref(),
        "test_search",
        Some(&req.question),
        None,
        &snippets,
    )
    .await;
    Ok(Json(SqlKnowledgeSearchResponse {
        references: snippets
            .iter()
            .map(ReferencePromptSnippet::to_usage_dto)
            .collect(),
    }))
}

pub(crate) async fn read_sql_knowledge_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(file_id): Path<String>,
    Query(params): Query<KnowledgeReadQuery>,
) -> Result<Json<KnowledgeReadResponse>> {
    let file = load_reference_file_record(&state.db, &claims.tenant_id, &file_id).await?;
    if file.datasource_id != "global" {
        validate_data_source_access_allow_missing(
            &state,
            &claims.tenant_id,
            &claims.sub,
            &claims.role,
            &file.datasource_id,
        )
        .await?;
    }
    let bytes = tokio::fs::read(&file.storage_path).await?;
    let text = decode_reference_text(&bytes)?;
    let lines: Vec<&str> = text.lines().collect();
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX).max(1);
    let full_file = params.start_line.is_none() && params.end_line.is_none();
    let start = if full_file {
        1
    } else {
        params.start_line.unwrap_or(1).max(1).min(total)
    };
    let end = if full_file {
        total
    } else {
        params
            .end_line
            .unwrap_or((start + 120).min(total))
            .max(start)
            .min(total)
    };
    let content = lines[(start - 1) as usize..end as usize].join("\n");
    Ok(Json(KnowledgeReadResponse {
        file_id,
        filename: file.filename,
        start_line: start,
        end_line: end,
        content,
    }))
}

async fn create_pack_record(
    state: &AppState,
    claims: &Claims,
    datasource_id: &str,
    datasource_bindings: &[String],
    scope: &str,
    name: &str,
    description: Option<&str>,
    tags: Vec<String>,
    verified: bool,
) -> Result<ReferencePackDto> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::ValidationError("SQL 知识空间名称不能为空".into()));
    }
    if name.chars().count() > 120 {
        return Err(AppError::ValidationError("SQL 知识空间名称过长".into()));
    }
    let normalized_scope = match scope.trim() {
        "tenant" | "global" => "tenant",
        _ => "datasource",
    };
    let mut bindings = normalize_ids(datasource_bindings);
    if normalized_scope == "tenant" {
        let is_admin = claims.role == "admin" || claims.role == "superadmin";
        if !is_admin {
            return Err(AppError::Forbidden);
        }
        if datasource_id != "global" || bindings.iter().any(|id| id != "global") {
            return Err(AppError::ValidationError(
                "tenant-global knowledge spaces cannot bind a specific data source".into(),
            ));
        }
        bindings.clear();
    }
    let resolved_datasource_id = if normalized_scope == "tenant" {
        "global".to_string()
    } else {
        validate_reference_datasource_bindings(state, claims, &bindings).await?;
        bindings.first().cloned().ok_or_else(|| {
            AppError::ValidationError("knowledge spaces must bind at least one data source".into())
        })?
    };
    if normalized_scope == "datasource" {
        if !bindings.iter().any(|id| id == datasource_id) {
            return Err(AppError::ValidationError(
                "the primary data source must be included in knowledge-space bindings".into(),
            ));
        }
    }
    let tags = normalize_tags(tags);
    let pack_id = format!("nlref-pack-{}", uuid::Uuid::new_v4());
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_reference_packs \
         (id, tenant_id, user_id, datasource_id, datasource_bindings_json, name, description, scope, tags_json, enabled, verified, stale, knowledge_kind, metadata_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, 0, 'sql_knowledge', ?)",
    )
    .bind(&pack_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&resolved_datasource_id)
    .bind(serde_json::to_value(&bindings)?)
    .bind(name)
    .bind(description.map(str::trim).filter(|s| !s.is_empty()))
    .bind(normalized_scope)
    .bind(serde_json::to_value(&tags)?)
    .bind(if verified { 1i32 } else { 0i32 })
    .bind(serde_json::json!({
        "createdBySurface": "sql_knowledge",
        "codexLikeTools": [
            "knowledge_tree",
            "knowledge_rg",
            "knowledge_read",
            "sql_example_open",
            "knowledge_outline",
            "knowledge_related",
            "knowledge_list",
            "knowledge_search",
            "schema_search"
        ]
    }))
    .execute(&state.db)
    .await?;

    let packs = load_reference_packs(
        &state.db,
        &claims.tenant_id,
        Some(&resolved_datasource_id),
        normalized_scope == "tenant",
        &claims.sub,
        claims.role == "admin" || claims.role == "superadmin",
    )
    .await?;
    packs
        .into_iter()
        .find(|p| p.id == pack_id)
        .ok_or_else(|| AppError::Internal("created SQL knowledge space not found".into()))
}

pub(crate) async fn resolve_query_references(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    bindings: Option<&ReferenceBindingRequest>,
    limit: usize,
) -> Result<Vec<ReferencePromptSnippet>> {
    // Every query is bound to the enabled tenant/datasource knowledge by
    // default.  An empty or omitted client override must not silently disable
    // first-party SQL references; the prompt/result window remains bounded by
    // `limit` and the existing relevance scorer.
    let default_bindings = ReferenceBindingRequest {
        include_all: true,
        ..ReferenceBindingRequest::default()
    };
    let effective_bindings = bindings
        .filter(|value| value.is_active())
        .unwrap_or(&default_bindings);
    let mut references = resolve_bound_query_references(
        &state.db,
        tenant_id,
        datasource_id,
        question,
        effective_bindings,
        limit,
    )
    .await?;
    append_approved_feedback_references(
        &state.db,
        tenant_id,
        datasource_id,
        question,
        &mut references,
        limit,
    )
    .await?;
    Ok(references)
}

/// Approved corrections are scoped exemplars, never global prompt text.  They
/// are appended only after normal lexical/embedding/rg retrieval and remain
/// bounded by the same prompt snippet limit.
async fn append_approved_feedback_references(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    references: &mut Vec<ReferencePromptSnippet>,
    limit: usize,
) -> Result<()> {
    if references.len() >= limit.max(1).min(MAX_PROMPT_SNIPPETS) {
        return Ok(());
    }
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT e.id, e.correction_json
         FROM feedback_learning_events e
         JOIN feedback_regression_cases r
           ON r.tenant_id = e.tenant_id AND r.feedback_event_id = e.id
         WHERE e.tenant_id = ? AND e.scope = ? AND e.approved = 1
           AND r.status = 'verified'
         ORDER BY e.created_at DESC LIMIT 20",
    )
    .bind(tenant_id)
    .bind(format!("datasource:{datasource_id}"))
    .fetch_all(db)
    .await?;
    let query_tokens = tokenize_for_reference(question);
    let max_items = limit.max(1).min(MAX_PROMPT_SNIPPETS);
    for row in rows {
        if references.len() >= max_items {
            break;
        }
        let event_id: String = row.try_get("id")?;
        let raw: String = row.try_get("correction_json")?;
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let source_question = value.get("question").and_then(|v| v.as_str()).unwrap_or("");
        let corrected_sql = value
            .get("correctedSql")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if corrected_sql.trim().is_empty() {
            continue;
        }
        let overlap = tokenize_for_reference(source_question)
            .iter()
            .filter(|token| query_tokens.contains(*token))
            .count();
        if !query_tokens.is_empty() && overlap == 0 {
            continue;
        }
        references.push(ReferencePromptSnippet {
            pack_id: "feedback-learning".into(),
            pack_name: "Approved NL2SQL corrections".into(),
            file_id: event_id.clone(),
            filename: format!("approved-correction-{event_id}.sql"),
            chunk_id: event_id,
            language: Some("sql".into()),
            start_line: 1,
            end_line: corrected_sql.lines().count().max(1) as u32,
            score: 1.0 + overlap as f64 * 0.01,
            reason: "approved tenant/datasource-scoped correction exemplar".into(),
            chunk_type: "approved_correction".into(),
            verified: true,
            stale: false,
            content: format!(
                "-- Approved correction for a similar question: {source_question}\n{corrected_sql}"
            ),
        });
    }
    Ok(())
}

pub(crate) async fn has_indexed_sql_knowledge_for_datasource(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> Result<bool> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT 1 \
         FROM nl2sql_reference_packs p \
         JOIN nl2sql_reference_files f ON f.tenant_id = p.tenant_id AND f.pack_id = p.id \
         WHERE p.tenant_id = ? \
           AND p.enabled = 1 \
           AND f.status = 'indexed' \
           AND (f.datasource_id = ? OR p.scope = 'tenant' OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ?)) \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(datasource_id)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

async fn resolve_bound_query_references(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    bindings: &ReferenceBindingRequest,
    limit: usize,
) -> Result<Vec<ReferencePromptSnippet>> {
    let mut pack_ids = normalize_ids(&bindings.pack_ids);
    let file_ids = normalize_ids(&bindings.file_ids);
    let include_all = bindings.include_all;
    if !include_all {
        validate_reference_bindings(db, tenant_id, datasource_id, &pack_ids, &file_ids).await?;
    }

    if include_all {
        pack_ids.clear();
    }

    let candidates = load_candidate_chunks(
        db,
        tenant_id,
        datasource_id,
        include_all,
        &pack_ids,
        &file_ids,
        Some(question),
        None,
    )
    .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let chunks_by_file = group_candidate_chunks_by_file(&candidates);
    let query_tokens = tokenize_for_reference(question);
    let selected_pack_ids: HashSet<String> = pack_ids.into_iter().collect();
    let selected_file_ids: HashSet<String> = file_ids.into_iter().collect();
    let mut scored: Vec<(f64, String, CandidateChunk)> = candidates
        .into_iter()
        .map(|chunk| {
            let (score, reason) = score_chunk(
                &chunk,
                &query_tokens,
                &selected_pack_ids,
                &selected_file_ids,
                None,
            );
            (score, reason, chunk)
        })
        .filter(|(score, _, _)| *score > 0.0)
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
    let take = limit.max(1).min(MAX_PROMPT_SNIPPETS);
    Ok(scored
        .into_iter()
        .take(take)
        .map(|(score, reason, chunk)| {
            snippet_from_scored_chunk(chunk, score, reason, &chunks_by_file)
        })
        .collect())
}

pub(crate) async fn resolve_auto_query_references(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    limit: usize,
) -> Result<Vec<ReferencePromptSnippet>> {
    let query_embedding = embed_reference_query(state, tenant_id, datasource_id, question)
        .await
        .ok();
    // Retrieval is deliberately split into independent lexical and semantic
    // lanes. Requiring a lexical match on the semantic query used to miss
    // useful SQL such as ROI questions whose files only mention revenue,
    // spend, or ROAS. Conversely, exact rg-like matches must remain available
    // when embedding generation or profile backfill is unavailable.
    let lexical_candidates = load_candidate_chunks(
        &state.db,
        tenant_id,
        datasource_id,
        true,
        &[],
        &[],
        Some(question),
        None,
    )
    .await?;
    let mut candidates_by_id = HashMap::new();
    for candidate in lexical_candidates {
        merge_hybrid_candidate(&mut candidates_by_id, candidate);
    }
    match load_rg_like_candidate_chunks(
        &state.db,
        tenant_id,
        datasource_id,
        question,
        None,
        rg_like_scan_limit(),
    )
    .await
    {
        Ok(candidates) => {
            for candidate in candidates {
                merge_hybrid_candidate(&mut candidates_by_id, candidate);
            }
        }
        Err(error) => tracing::warn!(
            tenant_id,
            datasource_id,
            error = %error,
            "SQL knowledge deterministic retrieval lane failed; keeping other candidates"
        ),
    }
    if let Some(embedding) = query_embedding.as_ref() {
        match load_candidate_chunks(
            &state.db,
            tenant_id,
            datasource_id,
            true,
            &[],
            &[],
            None,
            Some(embedding.profile_id.as_str()),
        )
        .await
        {
            Ok(candidates) => {
                for candidate in candidates {
                    merge_hybrid_candidate(&mut candidates_by_id, candidate);
                }
            }
            Err(error) => tracing::warn!(
                tenant_id,
                datasource_id,
                profile_id = %embedding.profile_id,
                error = %error,
                "SQL knowledge semantic retrieval lane failed; keeping deterministic candidates"
            ),
        }
    }
    let candidates = candidates_by_id.into_values().collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let chunks_by_file = group_candidate_chunks_by_file(&candidates);
    let schema_tables = load_schema_table_names(&state.db, datasource_id)
        .await
        .unwrap_or_default();
    let query_tokens = tokenize_for_reference(question);
    let selected_pack_ids = HashSet::new();
    let selected_file_ids = HashSet::new();
    let mut scored: Vec<(f64, String, CandidateChunk)> = candidates
        .into_iter()
        .map(|chunk| {
            score_auto_query_chunk(
                chunk,
                question,
                &query_tokens,
                &selected_pack_ids,
                &selected_file_ids,
                query_embedding
                    .as_ref()
                    .map(|embedding| embedding.vector.as_slice()),
                &schema_tables,
            )
        })
        .filter(|(score, _, _)| *score >= 0.20)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
    let take = limit
        .max(1)
        .min(MAX_PROMPT_SNIPPETS.max(AUTO_REFERENCE_LIMIT));
    Ok(scored
        .into_iter()
        .take(take)
        .map(|(score, reason, chunk)| {
            snippet_from_scored_chunk(chunk, score, reason, &chunks_by_file)
        })
        .collect())
}

fn merge_hybrid_candidate(
    candidates: &mut HashMap<String, CandidateChunk>,
    candidate: CandidateChunk,
) {
    match candidates.entry(candidate.chunk_id.clone()) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(candidate);
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            let existing = entry.get_mut();
            existing.fulltext_score = existing.fulltext_score.max(candidate.fulltext_score);
            if candidate.embedding.is_some() {
                existing.embedding_model = candidate.embedding_model;
                existing.embedding = candidate.embedding;
            }
        }
    }
}

fn score_auto_query_chunk(
    chunk: CandidateChunk,
    question: &str,
    query_tokens: &HashSet<String>,
    selected_pack_ids: &HashSet<String>,
    selected_file_ids: &HashSet<String>,
    query_embedding: Option<&[f32]>,
    schema_tables: &HashSet<String>,
) -> (f64, String, CandidateChunk) {
    let (mut score, mut reason) = score_chunk(
        &chunk,
        query_tokens,
        selected_pack_ids,
        selected_file_ids,
        query_embedding,
    );
    // Deterministic exact/term scoring is additive and intentionally stronger
    // than a weak semantic similarity. This keeps the Codex-like rg path in
    // control for exact business terms while still allowing semantic-only
    // candidates when the wording differs.
    if let Some((lexical_score, lexical_reason)) = rg_score_chunk(&chunk, question) {
        score += lexical_score;
        reason = append_reason(
            reason,
            format!("deterministic {lexical_score:.2}: {lexical_reason}"),
        );
    }
    let schema_overlap = schema_overlap_score(&chunk.extracted_tables, schema_tables);
    if schema_overlap > 0.0 {
        score += schema_overlap;
        reason = append_reason(reason, format!("schema overlap {:.2}", schema_overlap));
    } else if !chunk.extracted_tables.is_empty() && !schema_tables.is_empty() {
        // Cached discovery is often partial (permissions, catalog limits, or
        // transient failures). Treat mismatch as a weak confidence signal,
        // not proof that a SQL file is stale.
        score *= 0.85;
        reason = append_reason(reason, "not present in cached schema".to_string());
    }
    if chunk.verified {
        score += 1.2;
        reason = append_reason(reason, "verified".to_string());
    }
    if chunk.stale {
        score *= 0.55;
        let stale_reason = chunk
            .file_age_days
            .filter(|days| *days >= sql_knowledge_stale_after_days())
            .map(|days| format!("stale downgraded: file age {days}d"))
            .unwrap_or_else(|| "stale downgraded".to_string());
        reason = append_reason(reason, stale_reason);
    }
    if chunk.pack_scope == "tenant" {
        score += 0.25;
        reason = append_reason(reason, "tenant knowledge".to_string());
    }
    (score, reason, chunk)
}

pub(crate) async fn resolve_recent_sql_examples_for_datasource(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    limit: usize,
) -> Result<Vec<ReferencePromptSnippet>> {
    let candidates = load_candidate_chunks(
        &state.db,
        tenant_id,
        datasource_id,
        true,
        &[],
        &[],
        None,
        None,
    )
    .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let chunks_by_file = group_candidate_chunks_by_file(&candidates);
    let mut sql_candidates = candidates
        .into_iter()
        .filter(|chunk| {
            !chunk.stale
                && (chunk.chunk_type == "sql_example"
                    || matches!(chunk.language.as_deref(), Some("sql"))
                    || chunk.filename.to_ascii_lowercase().ends_with(".sql"))
        })
        .map(|chunk| {
            let mut score = if chunk.verified { 1.8 } else { 0.8 };
            if chunk.chunk_type == "sql_example" {
                score += 0.6;
            }
            if matches!(chunk.language.as_deref(), Some("sql"))
                || chunk.filename.to_ascii_lowercase().ends_with(".sql")
            {
                score += 0.4;
            }
            let age_bonus = chunk
                .file_age_days
                .map(|days| 0.3_f64 / (1.0 + f64::from(days.min(365)) / 30.0))
                .unwrap_or(0.0);
            score += age_bonus;
            let reason = if chunk.verified {
                "schema empty fallback: verified recent SQL example"
            } else {
                "schema empty fallback: recent SQL example"
            };
            (score, reason.to_string(), chunk)
        })
        .collect::<Vec<_>>();
    sql_candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));

    Ok(sql_candidates
        .into_iter()
        .take(
            limit
                .max(1)
                .min(MAX_PROMPT_SNIPPETS.max(AUTO_REFERENCE_LIMIT)),
        )
        .map(|(score, reason, chunk)| {
            snippet_from_scored_chunk(chunk, score, reason, &chunks_by_file)
        })
        .collect())
}

pub(crate) async fn sql_knowledge_tree_for_tool(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    query: Option<&str>,
    limit: usize,
) -> Result<serde_json::Value> {
    // The caller has already authorized `datasource_id`; this tool view is
    // scoped to that datasource and should include tenant-global references.
    let files = load_reference_files_for_datasource(
        &state.db,
        tenant_id,
        Some(datasource_id),
        true,
        "",
        true,
    )
    .await?;
    let terms = search_terms_for_tool(query.unwrap_or_default());
    let query_lower = query.unwrap_or_default().trim().to_lowercase();
    let mut scored = files
        .into_iter()
        .map(|file| {
            let mut score = 0.0_f64;
            let haystack = format!(
                "{}\n{}\n{}",
                file.filename,
                file.summary.as_deref().unwrap_or_default(),
                file.metadata
                    .as_ref()
                    .map(serde_json::Value::to_string)
                    .unwrap_or_default()
            )
            .to_lowercase();
            if !query_lower.is_empty() && haystack.contains(&query_lower) {
                score += 8.0;
            }
            for term in &terms {
                if haystack.contains(term) {
                    score += if file.filename.to_lowercase().contains(term) {
                        3.0
                    } else {
                        1.0
                    };
                }
            }
            if matches!(file.language.as_deref(), Some("sql")) || file.filename.ends_with(".sql") {
                score += 0.6;
            }
            if file.status == "indexed" {
                score += 0.3;
            }
            (score, file)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.1.updated_at.cmp(&a.1.updated_at))
    });

    let items = scored
        .into_iter()
        .take(limit.max(1).min(80))
        .map(|(score, file)| {
            serde_json::json!({
                "fileId": file.id,
                "packId": file.pack_id,
                "datasourceId": file.datasource_id,
                "filename": file.filename,
                "language": file.language,
                "status": file.status,
                "summary": file.summary,
                "sizeBytes": file.size_bytes,
                "chunkCount": file.chunk_count,
                "versionNo": file.version_no,
                "updatedAt": file.updated_at,
                "score": score,
                "metadata": file.metadata,
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "tool": "knowledge_tree",
        "query": query.unwrap_or_default(),
        "count": items.len(),
        "items": items,
    }))
}

pub(crate) async fn sql_knowledge_rg_for_tool(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    query: &str,
    filename: Option<&str>,
    limit: usize,
) -> Result<(Vec<ReferencePromptSnippet>, serde_json::Value)> {
    let mut by_chunk: HashMap<String, CandidateChunk> = HashMap::new();
    for chunk in load_candidate_chunks(
        &state.db,
        tenant_id,
        datasource_id,
        true,
        &[],
        &[],
        Some(query),
        None,
    )
    .await?
    {
        by_chunk.insert(chunk.chunk_id.clone(), chunk);
    }
    for chunk in load_rg_like_candidate_chunks(
        &state.db,
        tenant_id,
        datasource_id,
        query,
        filename,
        rg_like_scan_limit(),
    )
    .await?
    {
        by_chunk.entry(chunk.chunk_id.clone()).or_insert(chunk);
    }
    for chunk in load_candidate_chunks(
        &state.db,
        tenant_id,
        datasource_id,
        true,
        &[],
        &[],
        None,
        None,
    )
    .await?
    {
        by_chunk.entry(chunk.chunk_id.clone()).or_insert(chunk);
    }

    let filename_filter = filename
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let candidates = by_chunk.into_values().collect::<Vec<_>>();
    let chunks_by_file = group_candidate_chunks_by_file(&candidates);
    let mut scored = candidates
        .into_iter()
        .filter(|chunk| {
            filename_filter.as_ref().map_or(true, |needle| {
                chunk.file_id.to_lowercase().contains(needle)
                    || chunk.filename.to_lowercase().contains(needle)
            })
        })
        .filter_map(|chunk| rg_score_chunk(&chunk, query).map(|(s, r)| (s, r, chunk)))
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));

    let snippets = scored
        .into_iter()
        .take(limit.max(1).min(MAX_SEARCH_LIMIT))
        .map(|(score, reason, chunk)| {
            snippet_from_scored_chunk(chunk, score, reason, &chunks_by_file)
        })
        .collect::<Vec<_>>();
    let items = snippets
        .iter()
        .map(|snippet| {
            serde_json::json!({
                "fileId": snippet.file_id,
                "filename": snippet.filename,
                "chunkId": snippet.chunk_id,
                "chunkType": snippet.chunk_type,
                "language": snippet.language,
                "lines": [snippet.start_line, snippet.end_line],
                "score": snippet.score,
                "reason": snippet.reason,
                "verified": snippet.verified,
                "stale": snippet.stale,
                "preview": preview_chars(&snippet.content, 2400),
            })
        })
        .collect::<Vec<_>>();

    Ok((
        snippets,
        serde_json::json!({
            "tool": "knowledge_rg",
            "query": query,
            "filename": filename,
            "count": items.len(),
            "items": items,
        }),
    ))
}

pub(crate) async fn sql_knowledge_read_for_tool(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    file_id: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    max_chars: usize,
) -> Result<(Vec<ReferencePromptSnippet>, serde_json::Value)> {
    let (file, pack_name, verified, stale) =
        load_reference_file_record_for_datasource(&state.db, tenant_id, datasource_id, file_id)
            .await?;
    let bytes = tokio::fs::read(&file.storage_path).await?;
    let text = decode_reference_text(&bytes)?;
    let (start, end, content, truncated) =
        slice_reference_lines(&text, start_line, end_line, max_chars);
    let chunk_type = if matches!(file.language.as_deref(), Some("sql"))
        || file.filename.to_ascii_lowercase().ends_with(".sql")
        || looks_like_sql(&content)
    {
        "sql_example"
    } else {
        "text"
    }
    .to_string();
    let snippet = ReferencePromptSnippet {
        pack_id: file.pack_id.clone(),
        pack_name,
        file_id: file.id.clone(),
        filename: file.filename.clone(),
        chunk_id: format!("file-read-{}-{}-{}", file.id, start, end),
        language: file.language.clone(),
        start_line: start,
        end_line: end,
        score: 3.0,
        reason: if truncated {
            "knowledge_read exact file lines truncated by tool budget".to_string()
        } else {
            "knowledge_read exact file lines".to_string()
        },
        chunk_type,
        verified,
        stale,
        content: content.clone(),
    };
    let payload = serde_json::json!({
        "tool": "knowledge_read",
        "fileId": file.id,
        "filename": file.filename,
        "language": file.language,
        "lines": [start, end],
        "truncated": truncated,
        "content": content,
    });
    Ok((vec![snippet], payload))
}

pub(crate) async fn sql_knowledge_outline_for_tool(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    file_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<(Vec<ReferencePromptSnippet>, serde_json::Value)> {
    if let Some(file_id) = file_id.filter(|s| !s.trim().is_empty()) {
        let (snippets, read_payload) = sql_knowledge_read_for_tool(
            state,
            tenant_id,
            datasource_id,
            file_id,
            Some(1),
            None,
            sql_knowledge_full_example_max_chars(),
        )
        .await?;
        let content = snippets
            .first()
            .map(|s| s.content.as_str())
            .unwrap_or_default();
        let filename = snippets
            .first()
            .map(|s| s.filename.as_str())
            .unwrap_or_default();
        let outline = build_reference_outline(filename, content);
        return Ok((
            snippets,
            serde_json::json!({
                "tool": "knowledge_outline",
                "file": read_payload,
                "outline": outline,
            }),
        ));
    }

    let (snippets, rg_payload) =
        sql_knowledge_rg_for_tool(state, tenant_id, datasource_id, query, None, limit).await?;
    let outlines = snippets
        .iter()
        .map(|snippet| {
            serde_json::json!({
                "fileId": snippet.file_id,
                "filename": snippet.filename,
                "lines": [snippet.start_line, snippet.end_line],
                "reason": snippet.reason,
                "outline": build_reference_outline(&snippet.filename, &snippet.content),
            })
        })
        .collect::<Vec<_>>();
    Ok((
        snippets,
        serde_json::json!({
            "tool": "knowledge_outline",
            "query": query,
            "search": rg_payload,
            "outlines": outlines,
        }),
    ))
}

pub(crate) async fn sql_knowledge_related_for_tool(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    file_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<(Vec<ReferencePromptSnippet>, serde_json::Value)> {
    let related_query = if let Some(file_id) = file_id.filter(|s| !s.trim().is_empty()) {
        let file_chunks = load_candidate_chunks(
            &state.db,
            tenant_id,
            datasource_id,
            false,
            &[],
            &[file_id.to_string()],
            None,
            None,
        )
        .await?;
        let mut terms = Vec::new();
        let mut seen = HashSet::new();
        for chunk in &file_chunks {
            for item in chunk
                .extracted_tables
                .iter()
                .chain(chunk.extracted_metrics.iter())
                .chain(chunk.extracted_columns.iter())
            {
                let term = item.trim();
                if !term.is_empty() && seen.insert(term.to_lowercase()) {
                    terms.push(term.to_string());
                }
                if terms.len() >= 16 {
                    break;
                }
            }
            if terms.len() >= 16 {
                break;
            }
        }
        if terms.is_empty() {
            query.to_string()
        } else {
            terms.join(" ")
        }
    } else {
        query.to_string()
    };
    let (snippets, payload) =
        sql_knowledge_rg_for_tool(state, tenant_id, datasource_id, &related_query, None, limit)
            .await?;
    Ok((
        snippets,
        serde_json::json!({
            "tool": "knowledge_related",
            "fileId": file_id,
            "query": query,
            "expandedQuery": related_query,
            "results": payload,
        }),
    ))
}

async fn load_reference_file_record_for_datasource(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    file_id: &str,
) -> Result<(ReferenceFileRecord, String, bool, bool)> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT f.id, f.pack_id, f.datasource_id, f.filename, f.media_type, f.language, \
         CAST(f.size_bytes AS INTEGER), f.content_hash, f.storage_path, f.status, f.error, f.summary, \
         CAST(f.version_no AS INTEGER), f.metadata_json, \
         strftime('%Y-%m-%d %H:%M:%S', f.created_at), \
         strftime('%Y-%m-%d %H:%M:%S', f.updated_at), \
         p.name AS pack_name, p.verified, p.stale \
         FROM nl2sql_reference_files f \
         JOIN nl2sql_reference_packs p ON p.tenant_id = f.tenant_id AND p.id = f.pack_id \
         WHERE f.tenant_id = ? AND (f.id = ? OR f.filename = ?) \
           AND (f.datasource_id = ? OR p.scope = 'tenant' OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ?)) \
         ORDER BY CASE WHEN f.id = ? THEN 0 ELSE 1 END,
                  p.verified DESC, p.stale ASC, f.version_no DESC, f.updated_at DESC, f.id ASC \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(file_id)
    .bind(file_id)
    .bind(datasource_id)
    .bind(datasource_id)
    .bind(file_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("reference file not found".into()))?;

    let file = ReferenceFileRecord {
        id: row.get::<String, _>(0),
        pack_id: row.get::<String, _>(1),
        datasource_id: row.get::<String, _>(2),
        filename: row.get::<String, _>(3),
        media_type: row.get::<Option<String>, _>(4),
        language: row.get::<Option<String>, _>(5),
        size_bytes: i64_to_u64(row.get::<i64, _>(6)),
        content_hash: row.get::<String, _>(7),
        storage_path: row.get::<String, _>(8),
        status: row.get::<String, _>(9),
        error: row.get::<Option<String>, _>(10),
        summary: row.get::<Option<String>, _>(11),
        version_no: i64_to_u64(row.get::<i64, _>(12)),
        metadata: row.get::<Option<serde_json::Value>, _>(13),
        created_at: row.get::<String, _>(14),
        updated_at: row.get::<String, _>(15),
    };
    Ok((
        file,
        row.get::<String, _>("pack_name"),
        row.get::<i8, _>("verified") != 0,
        row.get::<i8, _>("stale") != 0,
    ))
}

fn rg_score_chunk(chunk: &CandidateChunk, query: &str) -> Option<(f64, String)> {
    let terms = search_terms_for_tool(query);
    if terms.is_empty() {
        return None;
    }
    let query_lower = query.trim().to_lowercase();
    let haystack = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        chunk.filename,
        chunk.keywords.as_deref().unwrap_or_default(),
        chunk.summary.as_deref().unwrap_or_default(),
        chunk.content,
        chunk.extracted_tables.join(" "),
        chunk.extracted_metrics.join(" ")
    )
    .to_lowercase();
    let filename_lower = chunk.filename.to_lowercase();
    let mut score = 0.0_f64;
    let mut matched = Vec::new();

    if !query_lower.is_empty() && haystack.contains(&query_lower) {
        score += 10.0;
        matched.push("exact phrase".to_string());
    }
    for term in &terms {
        if term.len() < 2 {
            continue;
        }
        if filename_lower.contains(term) {
            score += 4.0;
            if matched.len() < 8 {
                matched.push(format!("filename:{term}"));
            }
        }
        if chunk
            .extracted_tables
            .iter()
            .any(|t| t.to_lowercase().contains(term))
        {
            score += 3.0;
            if matched.len() < 8 {
                matched.push(format!("table:{term}"));
            }
        }
        if chunk
            .extracted_metrics
            .iter()
            .any(|m| m.to_lowercase().contains(term))
        {
            score += 3.0;
            if matched.len() < 8 {
                matched.push(format!("metric:{term}"));
            }
        }
        if haystack.contains(term) {
            score += if term.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                1.2
            } else {
                1.8
            };
            if matched.len() < 8 {
                matched.push(term.clone());
            }
        }
    }
    if chunk.fulltext_score > 0.0 {
        score += (chunk.fulltext_score * 2.0).min(3.0);
    }
    if chunk.verified {
        score += 1.0;
    }
    if chunk.chunk_type == "sql_example" {
        score += 0.6;
    }
    if matches!(chunk.language.as_deref(), Some("sql")) || filename_lower.ends_with(".sql") {
        score += 0.6;
    }
    if chunk.stale {
        score *= 0.65;
    }
    (score > 0.0).then(|| {
        (
            score,
            if matched.is_empty() {
                "rg candidate".to_string()
            } else {
                format!("rg matched {}", matched.join(", "))
            },
        )
    })
}

fn search_terms_for_tool(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    let trimmed = query.trim();
    if !trimmed.is_empty() && trimmed.chars().count() <= 120 {
        let lower = trimmed.to_lowercase();
        if seen.insert(lower.clone()) {
            terms.push(lower);
        }
    }

    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else {
            push_search_term(&mut terms, &mut seen, &current);
            current.clear();
            if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
                let s = ch.to_string();
                push_search_term(&mut terms, &mut seen, &s);
            }
        }
    }
    push_search_term(&mut terms, &mut seen, &current);

    for token in tokenize_for_reference(query) {
        push_search_term(&mut terms, &mut seen, &token);
    }
    let cjk_chars = query
        .chars()
        .filter(|ch| ('\u{4e00}'..='\u{9fff}').contains(ch))
        .collect::<Vec<_>>();
    for window in 2..=4 {
        if cjk_chars.len() < window {
            continue;
        }
        for slice in cjk_chars.windows(window) {
            let term = slice.iter().collect::<String>();
            push_search_term(&mut terms, &mut seen, &term);
        }
    }
    terms.truncate(32);
    terms
}

fn rg_like_terms(query: &str) -> Vec<String> {
    let mut terms = search_terms_for_tool(query)
        .into_iter()
        .filter(|term| term.chars().count() >= 2)
        .collect::<Vec<_>>();
    terms.sort_by(|a, b| {
        b.chars()
            .count()
            .cmp(&a.chars().count())
            .then_with(|| a.cmp(b))
    });
    terms.truncate(24);
    terms
}

fn rg_like_scan_limit() -> usize {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_RG_LIKE_SCAN_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 500)
        .unwrap_or(8_000)
        .min(20_000)
}

fn push_search_term(terms: &mut Vec<String>, seen: &mut HashSet<String>, value: &str) {
    let value = value.trim().to_lowercase();
    if value.chars().count() >= 2 && value.chars().count() <= 80 && seen.insert(value.clone()) {
        terms.push(value);
    }
}

fn slice_reference_lines(
    text: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    max_chars: usize,
) -> (u32, u32, String, bool) {
    let lines = text.lines().collect::<Vec<_>>();
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX).max(1);
    let start = start_line.unwrap_or(1).max(1).min(total);
    let requested_end = end_line.unwrap_or(total).max(start).min(total);
    let max_chars = max_chars.max(1_000);
    let mut out = String::new();
    let mut actual_end = start;
    let mut truncated = false;
    for line_no in start..=requested_end {
        let Some(line) = lines.get((line_no - 1) as usize) else {
            break;
        };
        let add_len = line.chars().count() + usize::from(!out.is_empty());
        if !out.is_empty() && out.chars().count().saturating_add(add_len) > max_chars {
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        actual_end = line_no;
    }
    if truncated {
        out.push_str("\n...");
    }
    (start, actual_end, out, truncated)
}

fn build_reference_outline(filename: &str, content: &str) -> serde_json::Value {
    let mut ctes = Vec::new();
    let mut headings = Vec::new();
    let mut params = Vec::new();
    let mut seen = HashSet::new();
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') && headings.len() < 24 {
            headings.push(serde_json::json!({
                "line": idx + 1,
                "text": trimmed.trim_start_matches('#').trim(),
            }));
        }
        if ctes.len() < 48 {
            if let Some(name) = extract_cte_name(trimmed) {
                if seen.insert(format!("cte:{name}")) {
                    ctes.push(serde_json::json!({ "line": idx + 1, "name": name }));
                }
            }
        }
        if params.len() < 48 {
            for param in extract_parameter_terms(trimmed) {
                if seen.insert(format!("param:{param}")) {
                    params.push(serde_json::json!({ "line": idx + 1, "name": param }));
                }
            }
        }
    }
    serde_json::json!({
        "filename": filename,
        "lineCount": content.lines().count(),
        "tables": extract_sql_table_names(content),
        "metrics": extract_metric_terms(content),
        "columns": extract_column_like_terms(content).into_iter().take(80).collect::<Vec<_>>(),
        "ctes": ctes,
        "headings": headings,
        "parameters": params,
    })
}

fn extract_cte_name(line: &str) -> Option<String> {
    static CTE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = CTE_RE.get_or_init(|| {
        Regex::new(r#"(?i)^\s*(?:WITH\s+)?([a-zA-Z_][a-zA-Z0-9_]*)\s+AS\s*\("#)
            .expect("valid CTE regex")
    });
    re.captures(line)
        .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
}

fn extract_parameter_terms(line: &str) -> Vec<String> {
    static PARAM_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = PARAM_RE.get_or_init(|| {
        Regex::new(r#"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|:([A-Za-z_][A-Za-z0-9_]*)|\{\{([A-Za-z_][A-Za-z0-9_]*)\}\}"#)
            .expect("valid parameter regex")
    });
    re.captures_iter(line)
        .filter_map(|cap| {
            cap.get(1)
                .or_else(|| cap.get(2))
                .or_else(|| cap.get(3))
                .map(|m| m.as_str().to_string())
        })
        .collect()
}

struct ExpandedCandidateContext {
    start_line: u32,
    end_line: u32,
    content: String,
    reason_suffix: String,
}

fn group_candidate_chunks_by_file(
    candidates: &[CandidateChunk],
) -> HashMap<String, Vec<CandidateChunk>> {
    let mut grouped: HashMap<String, Vec<CandidateChunk>> = HashMap::new();
    for chunk in candidates {
        grouped
            .entry(chunk.file_id.clone())
            .or_default()
            .push(chunk.clone());
    }
    for chunks in grouped.values_mut() {
        chunks.sort_by_key(|chunk| chunk.chunk_index);
    }
    grouped
}

fn expand_candidate_context(
    chunk: &CandidateChunk,
    chunks_by_file: &HashMap<String, Vec<CandidateChunk>>,
) -> ExpandedCandidateContext {
    let should_expand = chunk.chunk_type == "sql_example"
        || matches!(chunk.language.as_deref(), Some("sql" | "markdown"));
    if !should_expand {
        return ExpandedCandidateContext {
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            content: chunk.content.clone(),
            reason_suffix: "knowledge_read chunk".to_string(),
        };
    }

    let Some(file_chunks) = chunks_by_file.get(&chunk.file_id) else {
        return ExpandedCandidateContext {
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            content: chunk.content.clone(),
            reason_suffix: "sql_example_open chunk".to_string(),
        };
    };
    let Some(pos) = file_chunks
        .iter()
        .position(|candidate| candidate.chunk_id == chunk.chunk_id)
    else {
        return ExpandedCandidateContext {
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            content: chunk.content.clone(),
            reason_suffix: "sql_example_open chunk".to_string(),
        };
    };

    let should_open_full_sql_context = chunk.chunk_type == "sql_example"
        || matches!(chunk.language.as_deref(), Some("sql"))
        || chunk.filename.to_ascii_lowercase().ends_with(".sql");
    let (start_pos, end_pos, reason_suffix) = if should_open_full_sql_context {
        let max_chars = sql_knowledge_full_example_max_chars();
        let mut start_pos = pos;
        let mut end_pos = pos;
        let mut char_count = file_chunks[pos].content.chars().count();
        loop {
            let before = start_pos.checked_sub(1);
            let after = (end_pos + 1 < file_chunks.len()).then_some(end_pos + 1);
            let before_len = before
                .map(|idx| file_chunks[idx].content.chars().count())
                .unwrap_or(usize::MAX);
            let after_len = after
                .map(|idx| file_chunks[idx].content.chars().count())
                .unwrap_or(usize::MAX);
            let next = match (before, after) {
                (Some(b), Some(a)) => {
                    if before_len <= after_len {
                        Some((b, true, before_len))
                    } else {
                        Some((a, false, after_len))
                    }
                }
                (Some(b), None) => Some((b, true, before_len)),
                (None, Some(a)) => Some((a, false, after_len)),
                (None, None) => None,
            };
            let Some((idx, is_before, len)) = next else {
                break;
            };
            if char_count.saturating_add(len) > max_chars && end_pos > start_pos {
                break;
            }
            char_count = char_count.saturating_add(len);
            if is_before {
                start_pos = idx;
            } else {
                end_pos = idx;
            }
            if start_pos == 0 && end_pos + 1 == file_chunks.len() {
                break;
            }
            if char_count >= max_chars {
                break;
            }
        }
        let reason = if start_pos == 0 && end_pos + 1 == file_chunks.len() {
            "sql_example_open full file"
        } else {
            "sql_example_open wide context"
        };
        (start_pos, end_pos, reason.to_string())
    } else {
        (
            pos.saturating_sub(1),
            (pos + 1).min(file_chunks.len().saturating_sub(1)),
            "sql_example_open adjacent context".to_string(),
        )
    };
    let selected = &file_chunks[start_pos..=end_pos];
    let start_line = selected
        .first()
        .map(|c| c.start_line)
        .unwrap_or(chunk.start_line);
    let end_line = selected
        .last()
        .map(|c| c.end_line)
        .unwrap_or(chunk.end_line);
    let content = selected
        .iter()
        .map(|c| c.content.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    ExpandedCandidateContext {
        start_line,
        end_line,
        content,
        reason_suffix: if selected.len() > 1 {
            reason_suffix
        } else {
            "sql_example_open chunk".to_string()
        },
    }
}

fn trim_prompt_snippet_for_chunk(content: &str, chunk: &CandidateChunk) -> String {
    let max_chars = if chunk.chunk_type == "sql_example"
        || matches!(chunk.language.as_deref(), Some("sql"))
        || chunk.filename.to_ascii_lowercase().ends_with(".sql")
    {
        sql_knowledge_full_example_max_chars()
    } else if matches!(chunk.language.as_deref(), Some("markdown" | "md")) {
        12_000
    } else {
        6_000
    };
    preview_chars(content, max_chars)
}

fn snippet_from_scored_chunk(
    chunk: CandidateChunk,
    score: f64,
    reason: String,
    chunks_by_file: &HashMap<String, Vec<CandidateChunk>>,
) -> ReferencePromptSnippet {
    let expanded = expand_candidate_context(&chunk, chunks_by_file);
    let content = trim_prompt_snippet_for_chunk(&expanded.content, &chunk);
    ReferencePromptSnippet {
        pack_id: chunk.pack_id,
        pack_name: chunk.pack_name,
        file_id: chunk.file_id,
        filename: chunk.filename,
        chunk_id: chunk.chunk_id,
        language: chunk.language,
        start_line: expanded.start_line,
        end_line: expanded.end_line,
        score,
        reason: append_reason(reason, expanded.reason_suffix),
        chunk_type: chunk.chunk_type.clone(),
        verified: chunk.verified,
        stale: chunk.stale,
        content,
    }
}

pub(crate) async fn persist_query_reference_usages(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    query_id: &str,
    datasource_id: &str,
    references: &[ReferencePromptSnippet],
) {
    for item in references {
        if let Err(e) = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO nl2sql_query_reference_usages \
             (tenant_id, user_id, query_id, datasource_id, pack_id, pack_name, file_id, filename, \
              chunk_id, language, start_line, end_line, preview_text, reason, score) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(query_id)
        .bind(datasource_id)
        .bind(&item.pack_id)
        .bind(&item.pack_name)
        .bind(&item.file_id)
        .bind(&item.filename)
        .bind(&item.chunk_id)
        .bind(item.language.as_deref())
        .bind(item.start_line)
        .bind(item.end_line)
        .bind(preview_chars(&item.content, 520))
        .bind(&item.reason)
        .bind(item.score)
        .execute(db)
        .await
        {
            tracing::warn!(
                error = %e,
                tenant_id,
                query_id,
                chunk_id = %item.chunk_id,
                "failed to persist nl2sql reference usage"
            );
        }
    }
}

pub(crate) async fn persist_sql_knowledge_usage_events(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    datasource_id: Option<&str>,
    event_type: &str,
    question: Option<&str>,
    query_id: Option<&str>,
    references: &[ReferencePromptSnippet],
) {
    for item in references {
        if let Err(e) = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO sql_knowledge_usage_events \
             (tenant_id, user_id, datasource_id, event_type, question, query_id, pack_id, pack_name, \
              file_id, filename, chunk_id, chunk_type, start_line, end_line, score, reason, verified, stale) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(datasource_id)
        .bind(event_type)
        .bind(question)
        .bind(query_id)
        .bind(&item.pack_id)
        .bind(&item.pack_name)
        .bind(&item.file_id)
        .bind(&item.filename)
        .bind(&item.chunk_id)
        .bind(&item.chunk_type)
        .bind(item.start_line)
        .bind(item.end_line)
        .bind(item.score)
        .bind(&item.reason)
        .bind(if item.verified { 1i32 } else { 0i32 })
        .bind(if item.stale { 1i32 } else { 0i32 })
        .execute(db)
        .await
        {
            tracing::warn!(
                error = %e,
                tenant_id,
                event_type,
                chunk_id = %item.chunk_id,
                "failed to persist sql knowledge usage event"
            );
        }
    }
}

pub(crate) async fn load_query_reference_usages(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    query_ids: &[String],
) -> Result<HashMap<String, Vec<ReferenceUsageDto>>> {
    let query_ids = normalize_ids(query_ids);
    if query_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "SELECT u.query_id, u.pack_id, COALESCE(u.pack_name, p.name, '') AS pack_name, \
         u.file_id, COALESCE(u.filename, f.filename, '') AS filename, u.chunk_id, \
         COALESCE(u.language, c.language) AS language, \
         CAST(COALESCE(u.start_line, c.start_line, 1) AS INTEGER) AS start_line, \
         CAST(COALESCE(u.end_line, c.end_line, 1) AS INTEGER) AS end_line, \
         COALESCE(u.score, 0) AS score, COALESCE(u.reason, '') AS reason, \
         COALESCE(c.chunk_type, 'text') AS chunk_type, COALESCE(p.verified, 0) AS verified, \
         COALESCE(p.stale, 0) AS stale, COALESCE(u.preview_text, c.content_text, '') AS preview_text \
         FROM nl2sql_query_reference_usages u \
         LEFT JOIN nl2sql_reference_packs p \
           ON p.tenant_id = u.tenant_id AND p.id = u.pack_id \
         LEFT JOIN nl2sql_reference_files f \
           ON f.tenant_id = u.tenant_id AND f.id = u.file_id \
         LEFT JOIN nl2sql_reference_chunks c \
           ON c.tenant_id = u.tenant_id AND c.id = u.chunk_id \
         WHERE u.tenant_id = ",
    );
    qb.push_bind(tenant_id).push(" AND u.query_id IN (");
    {
        let mut separated = qb.separated(", ");
        for id in &query_ids {
            separated.push_bind(id);
        }
    }
    qb.push(") ORDER BY u.query_id, u.score DESC, u.id ASC");

    let rows = qb.build().fetch_all(db).await?;
    let mut grouped: HashMap<String, Vec<ReferenceUsageDto>> = HashMap::new();
    for row in rows {
        let query_id = row.try_get::<String, _>("query_id")?;
        let pack_id = row
            .try_get::<Option<String>, _>("pack_id")
            .ok()
            .flatten()
            .unwrap_or_default();
        let file_id = row
            .try_get::<Option<String>, _>("file_id")
            .ok()
            .flatten()
            .unwrap_or_default();
        let chunk_id = row
            .try_get::<Option<String>, _>("chunk_id")
            .ok()
            .flatten()
            .unwrap_or_default();
        let start_line = row.try_get::<i64, _>("start_line").unwrap_or(1).max(1);
        let end_line = row
            .try_get::<i64, _>("end_line")
            .unwrap_or(start_line)
            .max(start_line);
        let preview = row.try_get::<String, _>("preview_text").unwrap_or_default();
        grouped
            .entry(query_id)
            .or_default()
            .push(ReferenceUsageDto {
                pack_id,
                pack_name: row.try_get::<String, _>("pack_name").unwrap_or_default(),
                file_id,
                filename: row.try_get::<String, _>("filename").unwrap_or_default(),
                chunk_id,
                language: row.try_get::<Option<String>, _>("language").ok().flatten(),
                start_line: u32::try_from(start_line).unwrap_or(u32::MAX),
                end_line: u32::try_from(end_line).unwrap_or(u32::MAX),
                score: row.try_get::<f64, _>("score").unwrap_or(0.0),
                reason: row.try_get::<String, _>("reason").unwrap_or_default(),
                chunk_type: row
                    .try_get::<String, _>("chunk_type")
                    .unwrap_or_else(|_| "text".to_string()),
                verified: row.try_get::<i8, _>("verified").unwrap_or(0) != 0,
                stale: row.try_get::<i8, _>("stale").unwrap_or(0) != 0,
                preview: preview_chars(&preview, 520),
            });
    }

    Ok(grouped)
}

async fn validate_reference_bindings(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    pack_ids: &[String],
    file_ids: &[String],
) -> Result<()> {
    if !pack_ids.is_empty() {
        validate_pack_ids_with_builder(db, tenant_id, datasource_id, pack_ids).await?;
    }
    if !file_ids.is_empty() {
        validate_file_ids_with_builder(db, tenant_id, datasource_id, file_ids).await?;
    }
    Ok(())
}

async fn validate_pack_ids_with_builder(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    pack_ids: &[String],
) -> Result<()> {
    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM nl2sql_reference_packs \
         WHERE tenant_id = ",
    );
    qb.push_bind(tenant_id)
        .push(" AND enabled = 1 AND (datasource_id = ")
        .push_bind(datasource_id)
        .push(" OR scope = 'tenant' OR EXISTS (SELECT 1 FROM json_each(datasource_bindings_json) WHERE json_each.value = ")
        .push_bind(datasource_id)
        .push(")) AND id IN (");
    {
        let mut separated = qb.separated(", ");
        for id in pack_ids {
            separated.push_bind(id);
        }
    }
    qb.push(")");
    let count: i64 = qb.build_query_scalar().fetch_one(db).await?;
    if count != i64::try_from(pack_ids.len()).unwrap_or(i64::MAX) {
        return Err(AppError::ValidationError(
            "one or more reference packs are not available for this data source".into(),
        ));
    }
    Ok(())
}

async fn validate_file_ids_with_builder(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    file_ids: &[String],
) -> Result<()> {
    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM nl2sql_reference_files f \
         JOIN nl2sql_reference_packs p ON p.tenant_id = f.tenant_id AND p.id = f.pack_id \
         WHERE f.tenant_id = ",
    );
    qb.push_bind(tenant_id)
        .push(" AND (f.datasource_id = ")
        .push_bind(datasource_id)
        .push(" OR p.scope = 'tenant' OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ")
        .push_bind(datasource_id)
        .push(")) AND p.enabled = 1 AND f.status = 'indexed' AND f.id IN (");
    {
        let mut separated = qb.separated(", ");
        for id in file_ids {
            separated.push_bind(id);
        }
    }
    qb.push(")");
    let count: i64 = qb.build_query_scalar().fetch_one(db).await?;
    if count != i64::try_from(file_ids.len()).unwrap_or(i64::MAX) {
        return Err(AppError::ValidationError(
            "one or more reference files are not available for this data source".into(),
        ));
    }
    Ok(())
}

async fn load_candidate_chunks(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    include_all: bool,
    pack_ids: &[String],
    file_ids: &[String],
    fulltext_query: Option<&str>,
    profile_id: Option<&str>,
) -> Result<Vec<CandidateChunk>> {
    let trimmed_fulltext_query = fulltext_query
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string);
    let fulltext_terms = trimmed_fulltext_query
        .as_deref()
        .map(rg_like_terms)
        .unwrap_or_default();
    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "SELECT c.id AS chunk_id, c.pack_id, p.name AS pack_name, c.file_id, f.filename, \
         CAST(c.chunk_index AS INTEGER) AS chunk_index, c.language, \
         CAST(c.start_line AS INTEGER) AS start_line, CAST(c.end_line AS INTEGER) AS end_line, \
         c.content_text, c.keywords_text, COALESCE(c.chunk_type, 'text') AS chunk_type, \
         c.summary_text, c.metadata_json, c.extracted_tables_json, c.extracted_columns_json, \
         c.extracted_metrics_json, ",
    );
    if profile_id.is_some() {
        qb.push("e.model AS embedding_model, e.embedding_json, ");
    } else {
        qb.push("c.embedding_model, c.embedding_json, ");
    }
    qb.push(
        "p.verified, p.stale, p.scope AS pack_scope, \
         MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(f.updated_at)) AS INTEGER), 0) AS file_age_days, ",
    );
    if fulltext_terms.is_empty() {
        qb.push("0.0 AS fulltext_score ");
    } else {
        qb.push("CAST((");
        for (idx, term) in fulltext_terms.iter().take(12).enumerate() {
            if idx > 0 {
                qb.push(" + ");
            }
            let like = format!("%{}%", term.to_lowercase());
            qb.push("CASE WHEN LOWER(f.filename) LIKE ")
                .push_bind(like.clone())
                .push(" THEN 3 ELSE 0 END + CASE WHEN LOWER(c.content_text) LIKE ")
                .push_bind(like.clone())
                .push(" THEN 2 ELSE 0 END + CASE WHEN LOWER(COALESCE(c.keywords_text, '')) LIKE ")
                .push_bind(like)
                .push(" THEN 1 ELSE 0 END");
        }
        qb.push(") AS REAL) AS fulltext_score ");
    }
    qb.push(
        "FROM nl2sql_reference_chunks c \
         JOIN nl2sql_reference_packs p ON p.tenant_id = c.tenant_id AND p.id = c.pack_id \
         JOIN nl2sql_reference_files f ON f.tenant_id = c.tenant_id AND f.id = c.file_id ",
    );
    if let Some(profile_id) = profile_id {
        qb.push(
            "JOIN nl2sql_reference_chunk_embeddings e \
             ON e.tenant_id = c.tenant_id AND e.chunk_id = c.id AND e.profile_id = ",
        )
        .push_bind(profile_id)
        .push(" ");
    }
    qb.push("WHERE c.tenant_id = ");
    qb.push_bind(tenant_id)
        .push(" AND (c.datasource_id = ")
        .push_bind(datasource_id)
        .push(" OR p.scope = 'tenant' OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ")
        .push_bind(datasource_id)
        .push(")) AND p.enabled = 1 AND f.status = 'indexed'");
    if !include_all {
        if !pack_ids.is_empty() || !file_ids.is_empty() {
            qb.push(" AND (");
            let mut needs_or = false;
            if !pack_ids.is_empty() {
                qb.push("c.pack_id IN (");
                {
                    let mut ids = qb.separated(", ");
                    for id in pack_ids {
                        ids.push_bind(id);
                    }
                }
                qb.push(")");
                needs_or = true;
            }
            if !file_ids.is_empty() {
                if needs_or {
                    qb.push(" OR ");
                }
                qb.push("c.file_id IN (");
                {
                    let mut ids = qb.separated(", ");
                    for id in file_ids {
                        ids.push_bind(id);
                    }
                }
                qb.push(")");
            }
            qb.push(")");
        }
    }
    if !fulltext_terms.is_empty() {
        qb.push(" AND (");
        for (idx, term) in fulltext_terms.iter().take(12).enumerate() {
            if idx > 0 {
                qb.push(" OR ");
            }
            let like = format!("%{}%", term.to_lowercase());
            qb.push("LOWER(f.filename) LIKE ")
                .push_bind(like.clone())
                .push(" OR LOWER(c.content_text) LIKE ")
                .push_bind(like.clone())
                .push(" OR LOWER(COALESCE(c.keywords_text, '')) LIKE ")
                .push_bind(like);
        }
        qb.push(")");
        qb.push(" ORDER BY fulltext_score DESC, f.updated_at DESC, c.chunk_index ASC LIMIT 2000");
    } else {
        let limit = if profile_id.is_some() {
            semantic_candidate_scan_limit()
        } else {
            1_500
        };
        qb.push(" ORDER BY f.updated_at DESC, c.chunk_index ASC LIMIT ")
            .push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
    }

    let rows = qb.build().fetch_all(db).await?;
    Ok(rows.into_iter().map(candidate_chunk_from_row).collect())
}

fn semantic_candidate_scan_limit() -> usize {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_SEMANTIC_SCAN_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 1_000)
        .unwrap_or(12_000)
        .min(50_000)
}

async fn load_rg_like_candidate_chunks(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    query: &str,
    filename: Option<&str>,
    limit: usize,
) -> Result<Vec<CandidateChunk>> {
    let terms = rg_like_terms(query);
    let filename_filter = filename
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    if terms.is_empty() && filename_filter.is_none() {
        return Ok(Vec::new());
    }

    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "SELECT c.id AS chunk_id, c.pack_id, p.name AS pack_name, c.file_id, f.filename, \
         CAST(c.chunk_index AS INTEGER) AS chunk_index, c.language, \
         CAST(c.start_line AS INTEGER) AS start_line, CAST(c.end_line AS INTEGER) AS end_line, \
         c.content_text, c.keywords_text, COALESCE(c.chunk_type, 'text') AS chunk_type, \
         c.summary_text, c.metadata_json, c.extracted_tables_json, c.extracted_columns_json, \
         c.extracted_metrics_json, c.embedding_model, c.embedding_json, \
         p.verified, p.stale, p.scope AS pack_scope, \
         MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(f.updated_at)) AS INTEGER), 0) AS file_age_days, \
         0.0 AS fulltext_score \
         FROM nl2sql_reference_chunks c \
         JOIN nl2sql_reference_packs p ON p.tenant_id = c.tenant_id AND p.id = c.pack_id \
         JOIN nl2sql_reference_files f ON f.tenant_id = c.tenant_id AND f.id = c.file_id \
         WHERE c.tenant_id = ",
    );
    qb.push_bind(tenant_id)
        .push(" AND (c.datasource_id = ")
        .push_bind(datasource_id)
        .push(" OR p.scope = 'tenant' OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ")
        .push_bind(datasource_id)
        .push(")) AND p.enabled = 1 AND f.status = 'indexed'");

    if let Some(filename_filter) = filename_filter.as_deref() {
        let like = format!("%{}%", filename_filter);
        qb.push(" AND (LOWER(f.id) LIKE ")
            .push_bind(like.clone())
            .push(" OR LOWER(f.filename) LIKE ")
            .push_bind(like)
            .push(")");
    }

    if !terms.is_empty() {
        qb.push(" AND (");
        let mut first_clause = true;
        for term in &terms {
            let like = format!("%{}%", term);
            if !first_clause {
                qb.push(" OR ");
            }
            first_clause = false;
            qb.push("LOWER(f.filename) LIKE ").push_bind(like.clone());
            qb.push(" OR LOWER(c.content_text) LIKE ")
                .push_bind(like.clone());
            qb.push(" OR LOWER(COALESCE(c.keywords_text, '')) LIKE ")
                .push_bind(like.clone());
            qb.push(" OR LOWER(COALESCE(c.summary_text, '')) LIKE ")
                .push_bind(like);
        }
        qb.push(")");
    }

    qb.push(" ORDER BY (");
    let mut first_score = true;
    for (idx, term) in terms.iter().take(12).enumerate() {
        let like = format!("%{}%", term);
        let base_score = 120_i32.saturating_sub(i32::try_from(idx).unwrap_or(0) * 5);
        if !first_score {
            qb.push(" + ");
        }
        first_score = false;
        qb.push("CASE WHEN LOWER(f.filename) LIKE ")
            .push_bind(like.clone())
            .push(" THEN ")
            .push_bind(base_score + 20)
            .push(" ELSE 0 END + CASE WHEN LOWER(c.content_text) LIKE ")
            .push_bind(like.clone())
            .push(" THEN ")
            .push_bind(base_score)
            .push(" ELSE 0 END + CASE WHEN LOWER(COALESCE(c.keywords_text, '')) LIKE ")
            .push_bind(like.clone())
            .push(" THEN ")
            .push_bind(base_score + 10)
            .push(" ELSE 0 END + CASE WHEN LOWER(COALESCE(c.summary_text, '')) LIKE ")
            .push_bind(like)
            .push(" THEN ")
            .push_bind(base_score + 5)
            .push(" ELSE 0 END");
    }
    if first_score {
        qb.push("0");
    }
    qb.push(") DESC, p.verified DESC, f.updated_at DESC, c.chunk_index ASC LIMIT ");
    qb.push_bind(i64::try_from(limit.max(1).min(20_000)).unwrap_or(8_000));

    let rows = qb.build().fetch_all(db).await?;
    Ok(rows.into_iter().map(candidate_chunk_from_row).collect())
}

fn candidate_chunk_from_row(row: SqliteRow) -> CandidateChunk {
    CandidateChunk {
        chunk_id: row.get::<String, _>("chunk_id"),
        pack_id: row.get::<String, _>("pack_id"),
        pack_name: row.get::<String, _>("pack_name"),
        file_id: row.get::<String, _>("file_id"),
        filename: row.get::<String, _>("filename"),
        chunk_index: i64_to_u32(row.get::<i64, _>("chunk_index")),
        language: row.get::<Option<String>, _>("language"),
        start_line: i64_to_u32(row.get::<i64, _>("start_line")),
        end_line: i64_to_u32(row.get::<i64, _>("end_line")),
        content: row.get::<String, _>("content_text"),
        keywords: row.get::<Option<String>, _>("keywords_text"),
        chunk_type: row.get::<String, _>("chunk_type"),
        summary: row.get::<Option<String>, _>("summary_text"),
        metadata: row.get::<Option<serde_json::Value>, _>("metadata_json"),
        extracted_tables: parse_json_string_vec(
            row.get::<Option<serde_json::Value>, _>("extracted_tables_json"),
        ),
        extracted_columns: parse_json_string_vec(
            row.get::<Option<serde_json::Value>, _>("extracted_columns_json"),
        ),
        extracted_metrics: parse_json_string_vec(
            row.get::<Option<serde_json::Value>, _>("extracted_metrics_json"),
        ),
        embedding_model: row.get::<Option<String>, _>("embedding_model"),
        embedding: parse_embedding_json(row.get::<Option<String>, _>("embedding_json").as_deref()),
        fulltext_score: row.try_get::<f64, _>("fulltext_score").unwrap_or(0.0),
        verified: row.get::<i8, _>("verified") != 0,
        file_age_days: row.try_get::<i64, _>("file_age_days").ok().map(i64_to_u32),
        stale: row.get::<i8, _>("stale") != 0
            || row
                .try_get::<i64, _>("file_age_days")
                .ok()
                .map(i64_to_u32)
                .map(|days| days >= sql_knowledge_stale_after_days())
                .unwrap_or(false),
        pack_scope: row.get::<String, _>("pack_scope"),
    }
}

fn sql_knowledge_stale_after_days() -> u32 {
    std::env::var("AOS_SQL_KNOWLEDGE_STALE_AFTER_DAYS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_SQL_KNOWLEDGE_STALE_AFTER_DAYS)
}

fn score_chunk(
    chunk: &CandidateChunk,
    query_tokens: &HashSet<String>,
    selected_pack_ids: &HashSet<String>,
    selected_file_ids: &HashSet<String>,
    query_embedding: Option<&[f32]>,
) -> (f64, String) {
    let mut score = 0.0;
    let mut reasons: Vec<String> = Vec::new();
    if selected_file_ids.contains(&chunk.file_id) {
        score += 3.0;
        reasons.push("selected file".to_string());
    } else if selected_pack_ids.contains(&chunk.pack_id) {
        score += 1.25;
        reasons.push("selected pack".to_string());
    }

    let haystack = format!(
        "{}\n{}\n{}",
        chunk.filename,
        chunk.keywords.as_deref().unwrap_or_default(),
        chunk.content
    )
    .to_lowercase();
    let mut matched: Vec<String> = Vec::new();
    for token in query_tokens {
        if token.len() < 2 {
            continue;
        }
        if haystack.contains(token) {
            score += if token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                0.85
            } else {
                1.15
            };
            if matched.len() < 6 {
                matched.push(token.clone());
            }
        }
    }
    if !matched.is_empty() {
        reasons.push(format!("matched: {}", matched.join(", ")));
    }
    if let (Some(q), Some(e)) = (query_embedding, chunk.embedding.as_deref()) {
        let sim = cosine_similarity(q, e) as f64;
        if sim > 0.0 {
            score += sim * 4.0;
            reasons.push(format!("semantic {:.2}", sim));
        }
    }
    if chunk.fulltext_score > 0.0 {
        score += (chunk.fulltext_score * 2.0).min(3.0);
        reasons.push(format!("fulltext {:.2}", chunk.fulltext_score));
    }
    if !chunk.extracted_metrics.is_empty() {
        let metric_hits = chunk
            .extracted_metrics
            .iter()
            .filter(|m| query_tokens.contains(&m.to_lowercase()))
            .count();
        if metric_hits > 0 {
            score += metric_hits as f64 * 0.9;
            reasons.push(format!("metric hits {metric_hits}"));
        }
    }
    if !chunk.extracted_columns.is_empty() {
        let column_hits = chunk
            .extracted_columns
            .iter()
            .filter(|c| query_tokens.contains(&c.to_lowercase()))
            .count();
        if column_hits > 0 {
            score += (column_hits as f64 * 0.25).min(1.5);
            reasons.push(format!("column hits {column_hits}"));
        }
    }

    let filename_lower = chunk.filename.to_lowercase();
    for token in query_tokens {
        if filename_lower.contains(token) {
            score += 1.5;
            break;
        }
    }
    if score <= 0.0 {
        score = 0.1;
        reasons.push("selected reference candidate".to_string());
    }
    (score, reasons.join("; "))
}

async fn load_reference_packs(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: Option<&str>,
    include_global: bool,
    viewer_user_id: &str,
    is_admin: bool,
) -> Result<Vec<ReferencePackDto>> {
    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "SELECT p.id, p.user_id, p.datasource_id, CAST(p.datasource_bindings_json AS TEXT) AS datasource_bindings_json, \
         p.name, p.description, p.scope, CAST(p.tags_json AS TEXT) AS tags_json, p.enabled, \
         p.verified, p.stale, p.knowledge_kind, p.metadata_json, \
         strftime('%Y-%m-%d %H:%M:%S', p.created_at) AS created_at, \
         strftime('%Y-%m-%d %H:%M:%S', p.updated_at) AS updated_at, \
         CAST(COUNT(DISTINCT f.id) AS INTEGER) AS file_count, \
         CAST(COUNT(DISTINCT c.id) AS INTEGER) AS chunk_count \
         FROM nl2sql_reference_packs p \
         LEFT JOIN nl2sql_reference_files f ON f.tenant_id = p.tenant_id AND f.pack_id = p.id \
         LEFT JOIN nl2sql_reference_chunks c ON c.tenant_id = p.tenant_id AND c.pack_id = p.id \
         WHERE p.tenant_id = ",
    );
    qb.push_bind(tenant_id);
    if !is_admin {
        qb.push(" AND (p.scope = 'tenant' OR p.user_id = ")
            .push_bind(viewer_user_id)
            .push(" OR (EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json)) AND NOT EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) binding LEFT JOIN data_sources d ON d.tenant_id = p.tenant_id AND d.id = binding.value WHERE d.id IS NULL OR d.visibility <> 'tenant')))");
    }
    if let Some(ds) = datasource_id {
        qb.push(" AND (p.datasource_id = ")
            .push_bind(ds)
            .push(" OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ")
            .push_bind(ds)
            .push(")");
        if include_global {
            qb.push(" OR p.scope = 'tenant'");
        }
        qb.push(")");
    }
    qb.push(" GROUP BY p.id, p.user_id, p.datasource_id, p.datasource_bindings_json, p.name, p.description, p.scope, p.tags_json, p.enabled, p.verified, p.stale, p.knowledge_kind, p.metadata_json, p.created_at, p.updated_at ORDER BY p.updated_at DESC");
    let rows = qb.build().fetch_all(db).await?;

    let files = load_reference_files_for_datasource(
        db,
        tenant_id,
        datasource_id,
        include_global,
        viewer_user_id,
        is_admin,
    )
    .await?;
    let mut files_by_pack: HashMap<String, Vec<ReferenceFileDto>> = HashMap::new();
    for file in files {
        files_by_pack
            .entry(file.pack_id.clone())
            .or_default()
            .push(file);
    }

    Ok(rows
        .into_iter()
        .map(|row| {
            let id = row.get::<String, _>("id");
            let tags_json = row.get::<Option<String>, _>("tags_json");
            let bindings_json = row.get::<Option<String>, _>("datasource_bindings_json");
            let owner_user_id = row.get::<String, _>("user_id");
            let scope = row.get::<String, _>("scope");
            ReferencePackDto {
                id: id.clone(),
                datasource_id: row.get::<String, _>("datasource_id"),
                datasource_bindings: parse_tags_json(bindings_json.as_deref()),
                name: row.get::<String, _>("name"),
                description: row.get::<Option<String>, _>("description"),
                scope: scope.clone(),
                tags: parse_tags_json(tags_json.as_deref()),
                enabled: row.get::<i8, _>("enabled") != 0,
                verified: row.get::<i8, _>("verified") != 0,
                stale: row.get::<i8, _>("stale") != 0,
                knowledge_kind: row.get::<String, _>("knowledge_kind"),
                metadata: row.get::<Option<serde_json::Value>, _>("metadata_json"),
                created_at: row.get::<String, _>("created_at"),
                updated_at: row.get::<String, _>("updated_at"),
                file_count: i64_to_u64(row.get::<i64, _>("file_count")),
                chunk_count: i64_to_u64(row.get::<i64, _>("chunk_count")),
                writable: is_admin || (scope != "tenant" && owner_user_id == viewer_user_id),
                files: files_by_pack.remove(&id).unwrap_or_default(),
            }
        })
        .collect())
}

async fn load_reference_files_for_datasource(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: Option<&str>,
    include_global: bool,
    viewer_user_id: &str,
    is_admin: bool,
) -> Result<Vec<ReferenceFileDto>> {
    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "SELECT f.id, f.pack_id, f.datasource_id, f.filename, f.media_type, f.language, \
         CAST(f.size_bytes AS INTEGER) AS size_bytes, f.content_hash, f.status, f.error, f.summary, \
         CAST(f.version_no AS INTEGER) AS version_no, f.metadata_json, \
         strftime('%Y-%m-%d %H:%M:%S', f.created_at) AS created_at, \
         strftime('%Y-%m-%d %H:%M:%S', f.updated_at) AS updated_at, \
         CAST(COUNT(c.id) AS INTEGER) AS chunk_count \
         FROM nl2sql_reference_files f \
         JOIN nl2sql_reference_packs p ON p.tenant_id = f.tenant_id AND p.id = f.pack_id \
         LEFT JOIN nl2sql_reference_chunks c ON c.tenant_id = f.tenant_id AND c.file_id = f.id \
         WHERE f.tenant_id = ",
    );
    qb.push_bind(tenant_id);
    if !is_admin {
        qb.push(" AND (p.scope = 'tenant' OR p.user_id = ")
            .push_bind(viewer_user_id)
            .push(" OR (EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json)) AND NOT EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) binding LEFT JOIN data_sources d ON d.tenant_id = p.tenant_id AND d.id = binding.value WHERE d.id IS NULL OR d.visibility <> 'tenant')))");
    }
    if let Some(ds) = datasource_id {
        qb.push(" AND (f.datasource_id = ")
            .push_bind(ds)
            .push(" OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ")
            .push_bind(ds)
            .push(")");
        if include_global {
            qb.push(" OR p.scope = 'tenant'");
        }
        qb.push(")");
    }
    qb.push(" GROUP BY f.id, f.pack_id, f.datasource_id, f.filename, f.media_type, f.language, f.size_bytes, f.content_hash, f.status, f.error, f.summary, f.version_no, f.metadata_json, f.created_at, f.updated_at ORDER BY f.updated_at DESC");
    let rows = qb.build().fetch_all(db).await?;
    Ok(rows.into_iter().map(file_dto_from_row).collect())
}

async fn load_reference_file(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    file_id: &str,
) -> Result<ReferenceFileDto> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT f.id, f.pack_id, f.datasource_id, f.filename, f.media_type, f.language, \
         CAST(f.size_bytes AS INTEGER) AS size_bytes, f.content_hash, f.status, f.error, f.summary, \
         CAST(f.version_no AS INTEGER) AS version_no, f.metadata_json, \
         strftime('%Y-%m-%d %H:%M:%S', f.created_at) AS created_at, \
         strftime('%Y-%m-%d %H:%M:%S', f.updated_at) AS updated_at, \
         CAST(COUNT(c.id) AS INTEGER) AS chunk_count \
         FROM nl2sql_reference_files f \
         LEFT JOIN nl2sql_reference_chunks c ON c.tenant_id = f.tenant_id AND c.file_id = f.id \
         WHERE f.tenant_id = ? AND f.id = ? \
         GROUP BY f.id, f.pack_id, f.datasource_id, f.filename, f.media_type, f.language, f.size_bytes, f.content_hash, f.status, f.error, f.summary, f.version_no, f.metadata_json, f.created_at, f.updated_at",
    )
    .bind(tenant_id)
    .bind(file_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("reference file not found".into()))?;
    Ok(file_dto_from_row(row))
}

fn file_dto_from_row(row: sqlx::sqlite::SqliteRow) -> ReferenceFileDto {
    ReferenceFileDto {
        id: row.get::<String, _>("id"),
        pack_id: row.get::<String, _>("pack_id"),
        datasource_id: row.get::<String, _>("datasource_id"),
        filename: row.get::<String, _>("filename"),
        media_type: row.get::<Option<String>, _>("media_type"),
        language: row.get::<Option<String>, _>("language"),
        size_bytes: i64_to_u64(row.get::<i64, _>("size_bytes")),
        content_hash: row.get::<String, _>("content_hash"),
        status: row.get::<String, _>("status"),
        error: row.get::<Option<String>, _>("error"),
        summary: row.get::<Option<String>, _>("summary"),
        version_no: i64_to_u64(row.get::<i64, _>("version_no")),
        metadata: row.get::<Option<serde_json::Value>, _>("metadata_json"),
        created_at: row.get::<String, _>("created_at"),
        updated_at: row.get::<String, _>("updated_at"),
        chunk_count: i64_to_u64(row.get::<i64, _>("chunk_count")),
    }
}

fn is_sqlite_transient_write_error(err: &AppError) -> bool {
    let AppError::Database(sqlx::Error::Database(db_err)) = err else {
        return false;
    };
    let code_matches = db_err
        .code()
        .as_deref()
        .is_some_and(|code| matches!(code, "5" | "6" | "SQLITE_BUSY" | "SQLITE_LOCKED"));
    let message = db_err.message().to_ascii_lowercase();
    code_matches
        || message.contains("database is locked")
        || message.contains("database table is locked")
        || message.contains("database schema is locked")
        || message.contains("database is busy")
}

async fn load_pack_datasource_id(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    pack_id: &str,
) -> Result<String> {
    sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT datasource_id FROM nl2sql_reference_packs WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(pack_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("reference pack not found".into()))
}

async fn require_pack_write_access(
    state: &AppState,
    claims: &Claims,
    pack_id: &str,
) -> Result<(String, String)> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT datasource_id, scope, user_id \
         FROM nl2sql_reference_packs WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(pack_id)
    .fetch_optional(&state.db)
    .await?;
    let (datasource_id, scope, owner_user_id) =
        row.ok_or_else(|| AppError::NotFound("reference pack not found".into()))?;
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if scope == "tenant" {
        if !is_admin {
            return Err(AppError::Forbidden);
        }
    } else {
        if owner_user_id != claims.sub && !is_admin {
            return Err(AppError::Forbidden);
        }
        if datasource_id != "global" {
            validate_data_source_access_allow_missing(
                state,
                &claims.tenant_id,
                &claims.sub,
                &claims.role,
                &datasource_id,
            )
            .await?;
        }
    }
    Ok((datasource_id, scope))
}

async fn validate_data_source_access_allow_missing(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    role: &str,
    datasource_id: &str,
) -> Result<()> {
    match validate_data_source_access(state, tenant_id, user_id, role, datasource_id).await {
        Ok(_) => Ok(()),
        Err(AppError::NotFound(message)) if message == "data source not found" => {
            tracing::warn!(
                tenant_id,
                datasource_id,
                "SQL knowledge object references a deleted data source; allowing cleanup/read"
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}

async fn load_reference_file_record(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    file_id: &str,
) -> Result<ReferenceFileRecord> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, pack_id, datasource_id, filename, media_type, language, \
         CAST(size_bytes AS INTEGER), content_hash, storage_path, status, error, summary, \
         CAST(version_no AS INTEGER), metadata_json, \
         strftime('%Y-%m-%d %H:%M:%S', created_at), \
         strftime('%Y-%m-%d %H:%M:%S', updated_at) \
         FROM nl2sql_reference_files WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(file_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("reference file not found".into()))?;
    Ok(ReferenceFileRecord {
        id: row.get::<String, _>(0),
        pack_id: row.get::<String, _>(1),
        datasource_id: row.get::<String, _>(2),
        filename: row.get::<String, _>(3),
        media_type: row.get::<Option<String>, _>(4),
        language: row.get::<Option<String>, _>(5),
        size_bytes: i64_to_u64(row.get::<i64, _>(6)),
        content_hash: row.get::<String, _>(7),
        storage_path: row.get::<String, _>(8),
        status: row.get::<String, _>(9),
        error: row.get::<Option<String>, _>(10),
        summary: row.get::<Option<String>, _>(11),
        version_no: i64_to_u64(row.get::<i64, _>(12)),
        metadata: row.get::<Option<serde_json::Value>, _>(13),
        created_at: row.get::<String, _>(14),
        updated_at: row.get::<String, _>(15),
    })
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.chars().count() <= 48)
        .filter(|s| seen.insert(s.to_lowercase()))
        .take(20)
        .collect()
}

fn parse_tags_json(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default()
}

fn normalize_ids(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.chars().count() <= 96)
        .filter(|s| seen.insert(s.clone()))
        .take(100)
        .collect()
}

fn i64_to_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(u64::MAX)
}

fn i64_to_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn reference_pack_dir(state: &AppState, tenant_id: &str, pack_id: &str) -> PathBuf {
    state
        .data_dir
        .join(".aos")
        .join("nl2sql-reference")
        .join(safe_path_segment(tenant_id))
        .join(safe_path_segment(pack_id))
}

fn reference_import_staging_dir(state: &AppState, tenant_id: &str, task_id: &str) -> PathBuf {
    state
        .data_dir
        .join(".aos")
        .join("nl2sql-reference-imports")
        .join(safe_path_segment(tenant_id))
        .join(safe_path_segment(task_id))
}

fn reference_import_task_from_row(row: SqliteRow) -> ReferenceImportTaskDto {
    let failure_details_json: Option<String> = row.get("failure_details_json");
    ReferenceImportTaskDto {
        id: row.get("id"),
        pack_id: row.get("pack_id"),
        datasource_id: row.get("datasource_id"),
        status: row.get("status"),
        total_files: i64_to_u64(row.get("total_files")),
        processed_files: i64_to_u64(row.get("processed_files")),
        failed_files: i64_to_u64(row.get("failed_files")),
        current_filename: row.get("current_filename"),
        error_message: row.get("error_message"),
        failure_details: failure_details_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default(),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        updated_at: row.get("updated_at"),
    }
}

async fn load_reference_import_task(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
) -> Result<ReferenceImportTaskDto> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, pack_id, datasource_id, status, total_files, processed_files, failed_files, \
                current_filename, error_message, failure_details_json, \
                CAST(created_at AS TEXT) created_at, CAST(started_at AS TEXT) started_at, \
                CAST(completed_at AS TEXT) completed_at, CAST(updated_at AS TEXT) updated_at \
         FROM nl2sql_reference_import_tasks WHERE tenant_id = ? AND user_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(task_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("SQL knowledge import task not found".into()))?;
    Ok(reference_import_task_from_row(row))
}

pub(crate) fn start_sql_knowledge_import_worker(mut state: AppState) {
    // Import parsing, indexing, and progress persistence are all background
    // work. Rebind the worker's default handle so every downstream helper uses
    // the control pool, including helpers that only receive `&AppState`.
    state.db = state.control_db().clone();
    tokio::spawn(async move {
        let _ = sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_reference_import_tasks SET status = 'pending', current_filename = NULL, \
             error_message = 'server restarted; import resumed', updated_at = CURRENT_TIMESTAMP \
             WHERE status = 'running'",
        )
        .execute(&state.db)
        .await;
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match claim_reference_import_task(&state.db).await {
                Ok(Some(task)) => {
                    if let Err(error) = process_reference_import_task(&state, &task).await {
                        let safe_error = runtime::protect_sensitive_text(
                            &error.to_string(),
                            runtime::configured_data_protection_mode(),
                        )
                        .value;
                        let safe_error = safe_error.chars().take(2000).collect::<String>();
                        tracing::error!(
                            task_id = %task.id,
                            pack_id = %task.pack_id,
                            error = %safe_error,
                            "SQL knowledge import task failed"
                        );
                        let _ = sqlx::query::<sqlx::Sqlite>(
                            "UPDATE nl2sql_reference_import_tasks SET status = 'failed', \
                             error_message = ?, current_filename = NULL, completed_at = CURRENT_TIMESTAMP, \
                             updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                        )
                        .bind(safe_error)
                        .bind(&task.id)
                        .execute(&state.db)
                        .await;
                    }
                }
                Ok(None) => {}
                Err(error) if is_transient_sqlite_lock(&error) => {
                    tracing::debug!(error = %error, "SQL knowledge import claim deferred by SQLite contention");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(error) => {
                    tracing::warn!(error = %error, "failed to claim SQL knowledge import task")
                }
            }
        }
    });
}

fn is_transient_sqlite_lock(error: &AppError) -> bool {
    let AppError::Database(database_error) = error else {
        return false;
    };
    if matches!(database_error, sqlx::Error::PoolTimedOut) {
        return true;
    }
    let sqlx::Error::Database(database_error) = database_error else {
        return false;
    };
    let code = database_error.code();
    let message = database_error.message().to_ascii_lowercase();
    code.as_deref()
        .is_some_and(|value| matches!(value, "5" | "6" | "SQLITE_BUSY" | "SQLITE_LOCKED"))
        || message.contains("database is locked")
        || message.contains("database table is locked")
        || message.contains("database is busy")
}

#[derive(Debug)]
struct ClaimedReferenceImportTask {
    id: String,
    tenant_id: String,
    user_id: String,
    pack_id: String,
    datasource_id: String,
    manifest: Vec<ReferenceImportManifestItem>,
    staging_dir: PathBuf,
    processed_files: usize,
    failure_details: Vec<serde_json::Value>,
}

async fn claim_reference_import_task(
    db: &sqlx::SqlitePool,
) -> Result<Option<ClaimedReferenceImportTask>> {
    let mut tx = db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, tenant_id, user_id, pack_id, datasource_id, manifest_json, staging_dir, \
                processed_files, failure_details_json \
         FROM nl2sql_reference_import_tasks WHERE status = 'pending' \
         ORDER BY created_at ASC LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let id: String = row.get("id");
    let claimed = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_reference_import_tasks SET status = 'running', \
         started_at = COALESCE(started_at, CURRENT_TIMESTAMP), error_message = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ? AND status = 'pending'",
    )
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() != 1 {
        tx.commit().await?;
        return Ok(None);
    }
    let manifest_json: String = row.get("manifest_json");
    let failure_details_json: Option<String> = row.get("failure_details_json");
    let task = ClaimedReferenceImportTask {
        id,
        tenant_id: row.get("tenant_id"),
        user_id: row.get("user_id"),
        pack_id: row.get("pack_id"),
        datasource_id: row.get("datasource_id"),
        manifest: serde_json::from_str(&manifest_json)?,
        staging_dir: PathBuf::from(row.get::<String, _>("staging_dir")),
        processed_files: usize::try_from(row.get::<i64, _>("processed_files").max(0))
            .unwrap_or(usize::MAX),
        failure_details: failure_details_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default(),
    };
    tx.commit().await?;
    Ok(Some(task))
}

async fn process_reference_import_task(
    state: &AppState,
    task: &ClaimedReferenceImportTask,
) -> Result<()> {
    let claims = Claims::new(&task.user_id, "", "member", &task.tenant_id);
    let mut failures = task.failure_details.clone();
    let mut failed_files = failures.len();
    for (index, item) in task.manifest.iter().enumerate().skip(task.processed_files) {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_reference_import_tasks SET current_filename = ?, \
             updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(&item.filename)
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        let file_path = task.staging_dir.join(&item.staged_filename);
        let result = match tokio::fs::read(&file_path).await {
            Ok(bytes) => index_reference_upload(
                state,
                &claims,
                &task.pack_id,
                &task.datasource_id,
                item.filename.clone(),
                item.media_type.clone(),
                bytes,
            )
            .await
            .map(|_| ()),
            Err(error) => Err(AppError::Io(error)),
        };
        if let Err(error) = result {
            failed_files = failed_files.saturating_add(1);
            let safe_error = runtime::protect_sensitive_text(
                &error.to_string(),
                runtime::configured_data_protection_mode(),
            )
            .value;
            failures.push(json!({
                "filename": item.filename,
                "error": safe_error.chars().take(1000).collect::<String>(),
            }));
        }
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_reference_import_tasks SET processed_files = ?, failed_files = ?, \
             failure_details_json = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(i64::try_from(index + 1).unwrap_or(i64::MAX))
        .bind(i64::try_from(failed_files).unwrap_or(i64::MAX))
        .bind(serde_json::to_string(&failures)?)
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        // Persist the resume offset before deleting staged input. If AOS
        // restarts between these operations, the worker skips this completed
        // item instead of reporting a false missing-file failure.
        let _ = tokio::fs::remove_file(file_path).await;
    }
    let status = if failed_files == 0 {
        "completed"
    } else if failed_files >= task.manifest.len() {
        "failed"
    } else {
        "partial"
    };
    let error_message = match status {
        "failed" => Some("all files failed to import"),
        "partial" => Some("some files failed to import"),
        _ => None,
    };
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_reference_import_tasks SET status = ?, current_filename = NULL, \
         error_message = ?, completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?",
    )
    .bind(status)
    .bind(error_message)
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    let _ = tokio::fs::remove_dir_all(&task.staging_dir).await;
    tracing::info!(
        task_id = %task.id,
        pack_id = %task.pack_id,
        total_files = task.manifest.len(),
        failed_files,
        status,
        "SQL knowledge import finished"
    );
    Ok(())
}

async fn assert_storage_path_safe(
    state: &AppState,
    tenant_id: &str,
    pack_id: &str,
    path: &FsPath,
) -> Result<()> {
    let root = reference_pack_dir(state, tenant_id, pack_id);
    tokio::fs::create_dir_all(&root).await?;
    let canonical_root = tokio::fs::canonicalize(&root).await?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::ValidationError("invalid reference storage path".into()))?;
    tokio::fs::create_dir_all(parent).await?;
    let canonical_parent = tokio::fs::canonicalize(parent).await?;
    if !canonical_parent.starts_with(canonical_root) {
        return Err(AppError::ValidationError(
            "reference storage path escapes data directory".into(),
        ));
    }
    Ok(())
}

fn safe_path_segment(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn safe_filename(input: &str) -> String {
    let leaf = input
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("reference.txt")
        .trim();
    let sanitized: String = leaf
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('.').trim();
    if sanitized.is_empty() {
        "reference.txt".to_string()
    } else {
        sanitized.chars().take(180).collect()
    }
}

fn safe_reference_upload_name(input: &str) -> String {
    let normalized = input.replace('\\', "/");
    if normalized.contains('/') {
        let path = safe_zip_path(&normalized);
        if path.is_empty() {
            "reference.txt".to_string()
        } else {
            path
        }
    } else {
        safe_filename(input)
    }
}

fn decode_reference_text(bytes: &[u8]) -> Result<String> {
    if bytes.iter().take(2048).any(|b| *b == 0) {
        return Err(AppError::ValidationError(
            "binary reference files are not supported yet".into(),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| AppError::ValidationError("reference file must be valid UTF-8 text".into()))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn infer_language(filename: &str) -> Option<String> {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let lang = match ext.as_str() {
        "sql" | "hql" | "ddl" => "sql",
        "md" | "markdown" => "markdown",
        "txt" => "text",
        "py" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "csv" => "csv",
        "java" => "java",
        "go" => "go",
        "rs" => "rust",
        "sh" | "bash" | "zsh" => "shell",
        "log" => "log",
        _ => return None,
    };
    Some(lang.to_string())
}

#[derive(Debug)]
struct ReferenceChunkDraft {
    index: usize,
    start_line: usize,
    end_line: usize,
    content: String,
    keywords: Option<String>,
    chunk_type: String,
    summary: Option<String>,
    tables: Vec<String>,
    columns: Vec<String>,
    metrics: Vec<String>,
}

struct ReferenceUpload {
    filename: String,
    media_type: Option<String>,
    bytes: Vec<u8>,
}

async fn collect_multipart_reference_uploads(
    multipart: &mut Multipart,
) -> Result<Vec<ReferenceUpload>> {
    let mut uploads = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::ValidationError(format!("invalid multipart/form-data: {e}")))?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name != "file" && field_name != "files" {
            continue;
        }
        let filename = field
            .file_name()
            .map(safe_reference_upload_name)
            .unwrap_or_else(|| "reference.txt".to_string());
        let media_type = field.content_type().map(|s| s.to_string());
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::ValidationError(format!("failed to read upload: {e}")))?;
        if data.len() > MAX_REFERENCE_UPLOAD_BYTES {
            return Err(AppError::PayloadTooLarge(format!(
                "reference file exceeds {} bytes",
                MAX_REFERENCE_UPLOAD_BYTES
            )));
        }
        if is_zip_filename(&filename) {
            uploads.extend(extract_zip_reference_uploads(&data)?);
        } else {
            uploads.push(ReferenceUpload {
                filename,
                media_type,
                bytes: data.to_vec(),
            });
        }
    }
    if uploads.len() > MAX_ARCHIVE_FILES {
        return Err(AppError::PayloadTooLarge(format!(
            "too many files in SQL knowledge upload; max {}",
            MAX_ARCHIVE_FILES
        )));
    }
    Ok(uploads)
}

fn extract_zip_reference_uploads(bytes: &[u8]) -> Result<Vec<ReferenceUpload>> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| AppError::ValidationError(format!("invalid zip archive: {e}")))?;
    let mut uploads = Vec::new();
    for i in 0..archive.len() {
        if uploads.len() >= MAX_ARCHIVE_FILES {
            break;
        }
        let mut file = archive
            .by_index(i)
            .map_err(|e| AppError::ValidationError(format!("failed to read zip entry: {e}")))?;
        if file.is_dir() {
            continue;
        }
        let raw_name = file.name().to_string();
        let filename = safe_zip_path(&raw_name);
        if filename.is_empty() || infer_language(&filename).is_none() {
            continue;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .map_err(|e| AppError::ValidationError(format!("failed to read zip file: {e}")))?;
        if data.len() > MAX_REFERENCE_UPLOAD_BYTES {
            return Err(AppError::PayloadTooLarge(format!(
                "zip entry {filename} exceeds {} bytes",
                MAX_REFERENCE_UPLOAD_BYTES
            )));
        }
        uploads.push(ReferenceUpload {
            filename,
            media_type: None,
            bytes: data,
        });
    }
    Ok(uploads)
}

async fn index_reference_upload(
    state: &AppState,
    claims: &Claims,
    pack_id: &str,
    datasource_id: &str,
    filename: String,
    media_type: Option<String>,
    bytes: Vec<u8>,
) -> Result<ReferenceFileDto> {
    let text = decode_reference_text(&bytes)?;
    if text.trim().is_empty() {
        return Err(AppError::ValidationError(
            "reference file is empty after decoding".into(),
        ));
    }
    if text.chars().count() > MAX_REFERENCE_CHARS {
        return Err(AppError::PayloadTooLarge(format!(
            "reference text exceeds {} characters",
            MAX_REFERENCE_CHARS
        )));
    }

    let language = infer_language(&filename);
    let content_hash = sha256_hex_bytes(&bytes);
    if let Some(existing_file_id) = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT id FROM nl2sql_reference_files \
         WHERE tenant_id = ? AND pack_id = ? AND content_hash = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(pack_id)
    .bind(&content_hash)
    .fetch_optional(&state.db)
    .await?
    {
        tracing::info!(
            tenant_id = %claims.tenant_id,
            pack_id,
            file_id = %existing_file_id,
            filename = %filename,
            "SQL knowledge duplicate content hash found; skipping re-index"
        );
        return load_reference_file(&state.db, &claims.tenant_id, &existing_file_id).await;
    }

    let file_id = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT id FROM nl2sql_reference_files \
         WHERE tenant_id = ? AND pack_id = ? AND filename = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(pack_id)
    .bind(&filename)
    .fetch_optional(&state.db)
    .await?
    .unwrap_or_else(|| format!("nlref-file-{}", uuid::Uuid::new_v4()));
    let file_dir = reference_pack_dir(state, &claims.tenant_id, pack_id).join(&file_id);
    tokio::fs::create_dir_all(&file_dir).await?;
    let storage_path = file_dir.join(safe_filename(&filename));
    assert_storage_path_safe(state, &claims.tenant_id, pack_id, &storage_path).await?;
    tokio::fs::write(&storage_path, &bytes).await?;

    let summary = summarize_reference_text(&text);
    let chunks = chunk_reference_text(&text, language.as_deref());
    let embedding_inputs: Vec<String> = chunks
        .iter()
        .map(|chunk| build_embedding_input(&filename, chunk))
        .collect();
    let embedding_batch =
        embed_reference_batch(state, &claims.tenant_id, datasource_id, &embedding_inputs).await?;

    let file_metadata = serde_json::json!({
        "source": "sql_knowledge",
        "tables": extract_sql_table_names(&text),
        "metrics": extract_metric_terms(&text),
        "chunkCount": chunks.len(),
    });
    let storage_path_text = storage_path.to_string_lossy().to_string();
    let mut last_write_error: Option<AppError> = None;
    let mut persisted_file_id = file_id.clone();
    for attempt in 1..=3 {
        let write_result: Result<String> = async {
            let mut tx = state.db.begin().await?;
            // Lock/upsert the file row first. Concurrent uploads of the same
            // content hash now serialize on this row before replacing chunks.
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO nl2sql_reference_files \
                 (id, tenant_id, user_id, pack_id, datasource_id, filename, media_type, language, size_bytes, content_hash, version_no, storage_path, status, summary, metadata_json) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, 'indexed', ?, ?) \
                 ON CONFLICT DO UPDATE SET \
                   filename = excluded.filename, media_type = excluded.media_type, language = excluded.language, \
                   size_bytes = excluded.size_bytes, content_hash = excluded.content_hash, \
                   storage_path = excluded.storage_path, status = 'indexing', \
                   version_no = version_no + 1, error = NULL, summary = excluded.summary, metadata_json = excluded.metadata_json",
            )
            .bind(&file_id)
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .bind(pack_id)
            .bind(datasource_id)
            .bind(&filename)
            .bind(media_type.as_deref())
            .bind(language.as_deref())
            .bind(i64::try_from(bytes.len()).unwrap_or(i64::MAX))
            .bind(&content_hash)
            .bind(&storage_path_text)
            .bind(summary.as_deref())
            .bind(file_metadata.clone())
            .execute(&mut *tx)
            .await?;

            // ON CONFLICT can resolve to an existing row when another
            // upload concurrently inserted the same content hash. Use the
            // persisted row id for chunk replacement and final loading instead
            // of the optimistic id generated for this attempt.
            let actual_file_id = sqlx::query_scalar::<sqlx::Sqlite, String>(
                "SELECT id FROM nl2sql_reference_files \
                 WHERE tenant_id = ? AND pack_id = ? AND content_hash = ? LIMIT 1",
            )
            .bind(&claims.tenant_id)
            .bind(pack_id)
            .bind(&content_hash)
            .fetch_one(&mut *tx)
            .await?;
            if actual_file_id != file_id {
                tracing::info!(
                    tenant_id = %claims.tenant_id,
                    pack_id,
                    attempted_file_id = %file_id,
                    actual_file_id = %actual_file_id,
                    filename = %filename,
                    "SQL knowledge upload reused an existing content-hash row"
                );
            }

            sqlx::query::<sqlx::Sqlite>("DELETE FROM nl2sql_reference_chunks WHERE tenant_id = ? AND file_id = ?")
                .bind(&claims.tenant_id)
                .bind(&actual_file_id)
                .execute(&mut *tx)
                .await?;

            for (index, chunk) in chunks.iter().enumerate() {
                let embedding = &embedding_batch.local.vectors[index];
                let chunk_id = format!("nlref-chunk-{}", uuid::Uuid::new_v4());
                let metadata = serde_json::json!({
                    "tables": chunk.tables,
                    "columns": chunk.columns,
                    "metrics": chunk.metrics,
                    "joins": extract_join_hints(&chunk.content),
                    "codexLikeTool": "knowledge_read",
                });
                sqlx::query::<sqlx::Sqlite>(
                    "INSERT INTO nl2sql_reference_chunks \
                     (id, tenant_id, datasource_id, pack_id, file_id, chunk_index, language, chunk_type, start_line, end_line, content_text, content_hash, token_count, keywords_text, summary_text, extracted_tables_json, extracted_columns_json, extracted_metrics_json, metadata_json, embedding_model, embedding_dimensions, embedding_json) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&chunk_id)
                .bind(&claims.tenant_id)
                .bind(datasource_id)
                .bind(pack_id)
                .bind(&actual_file_id)
                .bind(u32::try_from(chunk.index).unwrap_or(u32::MAX))
                .bind(language.as_deref())
                .bind(&chunk.chunk_type)
                .bind(u32::try_from(chunk.start_line).unwrap_or(u32::MAX))
                .bind(u32::try_from(chunk.end_line).unwrap_or(u32::MAX))
                .bind(&chunk.content)
                .bind(sha256_hex(chunk.content.as_str()))
                .bind(u32::try_from(estimate_token_count(&chunk.content)).unwrap_or(u32::MAX))
                .bind(chunk.keywords.as_deref())
                .bind(chunk.summary.as_deref())
                .bind(serde_json::to_value(&chunk.tables)?)
                .bind(serde_json::to_value(&chunk.columns)?)
                .bind(serde_json::to_value(&chunk.metrics)?)
                .bind(metadata)
                .bind(&embedding_batch.local.profile.config.model)
                .bind(i32::try_from(embedding.len()).unwrap_or(i32::MAX))
                .bind(serde_json::to_string(embedding)?)
                .execute(&mut *tx)
                .await?;

                insert_reference_profile_embedding(
                    &mut tx,
                    &claims.tenant_id,
                    &chunk_id,
                    &embedding_batch.local,
                    index,
                )
                .await?;
                if let Some(api) = &embedding_batch.api {
                    insert_reference_profile_embedding(
                        &mut tx,
                        &claims.tenant_id,
                        &chunk_id,
                        api,
                        index,
                    )
                    .await?;
                }
            }

            sqlx::query::<sqlx::Sqlite>(
                "UPDATE nl2sql_reference_files \
                 SET status = 'indexed', error = NULL \
                 WHERE tenant_id = ? AND id = ?",
            )
            .bind(&claims.tenant_id)
            .bind(&actual_file_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(actual_file_id)
        }
        .await;

        match write_result {
            Ok(actual_file_id) => {
                persisted_file_id = actual_file_id;
                last_write_error = None;
                break;
            }
            Err(err) if attempt < 3 && is_sqlite_transient_write_error(&err) => {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    pack_id,
                    file_id = %file_id,
                    attempt,
                    error = %err,
                    "SQL knowledge file index write hit transient database lock; retrying"
                );
                last_write_error = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(150 * attempt as u64)).await;
            }
            Err(err) => return Err(err),
        }
    }
    if let Some(err) = last_write_error {
        return Err(err);
    }
    load_reference_file(&state.db, &claims.tenant_id, &persisted_file_id).await
}

fn chunk_reference_text(text: &str, language: Option<&str>) -> Vec<ReferenceChunkDraft> {
    if matches!(language, Some("markdown")) {
        let md_chunks = chunk_markdown_reference_text(text, language);
        if !md_chunks.is_empty() {
            return md_chunks;
        }
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + CHUNK_LINE_TARGET).min(lines.len());
        let content = lines[start..end].join("\n");
        let tables = extract_sql_table_names(&content);
        let columns = extract_column_like_terms(&content);
        let metrics = extract_metric_terms(&content);
        chunks.push(ReferenceChunkDraft {
            index: chunks.len(),
            start_line: start + 1,
            end_line: end,
            keywords: Some(build_keywords_text(&content, language)),
            chunk_type: infer_chunk_type(&content, language),
            summary: summarize_reference_text(&content),
            tables,
            columns,
            metrics,
            content,
        });
        if end >= lines.len() {
            break;
        }
        start = end.saturating_sub(CHUNK_LINE_OVERLAP);
    }
    chunks
}

fn chunk_markdown_reference_text(text: &str, language: Option<&str>) -> Vec<ReferenceChunkDraft> {
    let lines: Vec<&str> = text.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut in_code = false;
    let mut current_heading = String::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
        }
        let is_heading = !in_code && trimmed.starts_with('#');
        let should_cut =
            is_heading && idx > start || idx.saturating_sub(start) >= CHUNK_LINE_TARGET;
        if should_cut {
            push_markdown_chunk(&mut chunks, &lines, start, idx, &current_heading, language);
            start = idx;
        }
        if is_heading {
            current_heading = trimmed.trim_start_matches('#').trim().to_string();
        }
    }
    if start < lines.len() {
        push_markdown_chunk(
            &mut chunks,
            &lines,
            start,
            lines.len(),
            &current_heading,
            language,
        );
    }
    chunks
}

fn push_markdown_chunk(
    chunks: &mut Vec<ReferenceChunkDraft>,
    lines: &[&str],
    start: usize,
    end: usize,
    heading: &str,
    language: Option<&str>,
) {
    let content = lines[start..end].join("\n");
    if content.trim().is_empty() {
        return;
    }
    let content = if heading.is_empty() || content.trim_start().starts_with('#') {
        content
    } else {
        format!("# {heading}\n{content}")
    };
    let tables = extract_sql_table_names(&content);
    let columns = extract_column_like_terms(&content);
    let metrics = extract_metric_terms(&content);
    chunks.push(ReferenceChunkDraft {
        index: chunks.len(),
        start_line: start + 1,
        end_line: end,
        keywords: Some(build_keywords_text(&content, language)),
        chunk_type: infer_chunk_type(&content, language),
        summary: summarize_reference_text(&content),
        tables,
        columns,
        metrics,
        content,
    });
}

fn build_keywords_text(content: &str, language: Option<&str>) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for token in tokenize_for_reference(content) {
        if seen.insert(token.clone()) {
            terms.push(token);
        }
        if terms.len() >= 120 {
            break;
        }
    }
    if matches!(language, Some("sql")) {
        for table in extract_sql_table_names(content) {
            if seen.insert(table.to_lowercase()) {
                terms.push(table);
            }
        }
    }
    for metric in extract_metric_terms(content) {
        if seen.insert(metric.to_lowercase()) {
            terms.push(metric);
        }
    }
    terms.join(" ")
}

fn tokenize_for_reference(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' || ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            current.push(ch.to_ascii_lowercase());
        } else {
            flush_token(&mut out, &mut current);
        }
    }
    flush_token(&mut out, &mut current);
    out
}

pub(crate) fn tokenize_for_sql_knowledge_tool(text: &str) -> HashSet<String> {
    tokenize_for_reference(text)
}

fn flush_token(out: &mut HashSet<String>, current: &mut String) {
    let token = current.trim();
    if token.chars().count() >= 2 && token.chars().count() <= 64 {
        out.insert(token.to_string());
    }
    current.clear();
}

fn raw_looks_like_sql(content: &str) -> bool {
    static SQL_CUE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = SQL_CUE_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(select|from|join|with|insert|into|update|delete|set|where)\b")
            .expect("valid SQL cue regex")
    });
    let cues = re
        .captures_iter(content)
        .filter_map(|capture| {
            capture
                .get(1)
                .map(|value| value.as_str().to_ascii_lowercase())
        })
        .collect::<HashSet<_>>();
    (cues.contains("select") && (cues.contains("from") || cues.contains("join")))
        || (cues.contains("insert") && cues.contains("into"))
        || (cues.contains("update") && (cues.contains("set") || cues.contains("where")))
        || (cues.contains("delete") && cues.contains("from"))
        || (cues.contains("with") && cues.contains("select"))
}

fn sql_metadata_content(content: &str) -> Cow<'_, str> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    let mut fence_language = String::new();
    let mut in_fence = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(marker) = trimmed.strip_prefix("```") {
            if in_fence {
                let block = current.join("\n");
                let language = fence_language.split_whitespace().next().unwrap_or_default();
                let explicitly_sql = matches!(
                    language,
                    "sql"
                        | "trino"
                        | "presto"
                        | "mysql"
                        | "tidb"
                        | "hive"
                        | "spark"
                        | "sparksql"
                        | "postgres"
                        | "postgresql"
                        | "sqlite"
                        | "clickhouse"
                        | "duckdb"
                        | "bigquery"
                        | "snowflake"
                );
                if explicitly_sql || (language.is_empty() && raw_looks_like_sql(&block)) {
                    blocks.push(block);
                }
                current.clear();
                fence_language.clear();
                in_fence = false;
            } else {
                fence_language = marker.trim().to_ascii_lowercase();
                in_fence = true;
            }
            continue;
        }
        if in_fence {
            current.push(line);
        }
    }

    if blocks.is_empty() {
        Cow::Borrowed(content)
    } else {
        Cow::Owned(blocks.join("\n\n"))
    }
}

fn extract_sql_table_names(content: &str) -> Vec<String> {
    static SQL_TABLE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = SQL_TABLE_RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:from|join|into|update)\s+((?:"[^"]+"|`[^`]+`|[a-zA-Z_][a-zA-Z0-9_$]*)(?:\s*\.\s*(?:"[^"]+"|`[^`]+`|[a-zA-Z_][a-zA-Z0-9_$]*))*)"#,
        )
            .expect("valid SQL table regex")
    });
    let content = sql_metadata_content(content);
    let mut seen = HashSet::new();
    re.captures_iter(content.as_ref())
        .filter_map(|cap| {
            cap.get(1)
                .map(|m| super::normalize_table_identifier(m.as_str()))
        })
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_lowercase()))
        .take(50)
        .collect()
}

fn extract_column_like_terms(content: &str) -> Vec<String> {
    static COL_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = COL_RE.get_or_init(|| {
        Regex::new(r"(?i)\b([a-zA-Z_][a-zA-Z0-9_]*\.[a-zA-Z_][a-zA-Z0-9_]*|[a-zA-Z_][a-zA-Z0-9_]*(?:_id|_dt|_date|_time|_uv|_pv|_cnt|_cost|_rev|_rate|_ratio|_roi|_roas|_amount|_count))\b")
            .expect("valid column regex")
    });
    let content = sql_metadata_content(content);
    let table_identifiers = extract_sql_table_names(content.as_ref())
        .into_iter()
        .flat_map(|table| {
            let leaf = table.rsplit('.').next().unwrap_or(&table).to_string();
            [table.to_ascii_lowercase(), leaf.to_ascii_lowercase()]
        })
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut columns: Vec<String> = re
        .captures_iter(content.as_ref())
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim_matches('`').to_string()))
        .filter(|s| seen.insert(s.to_lowercase()))
        .take(80)
        .collect();

    if looks_like_sql(content.as_ref()) {
        for ident in extract_sql_identifiers(content.as_ref()) {
            if columns.len() >= 120 {
                break;
            }
            let lower = ident.to_lowercase();
            if !table_identifiers.contains(&lower) && seen.insert(lower) {
                columns.push(ident);
            }
        }
    }
    columns
}

fn looks_like_sql(content: &str) -> bool {
    raw_looks_like_sql(content)
}

fn extract_sql_identifiers(content: &str) -> Vec<String> {
    static IDENT_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = IDENT_RE.get_or_init(|| {
        Regex::new(r"`([A-Za-z_][A-Za-z0-9_]*)`|\b([A-Za-z_][A-Za-z0-9_]*)\b")
            .expect("valid SQL identifier regex")
    });
    let content = sql_metadata_content(content);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for cap in re.captures_iter(content.as_ref()) {
        let Some(raw) = cap.get(1).or_else(|| cap.get(2)).map(|m| m.as_str()) else {
            continue;
        };
        let ident = raw.trim_matches('`');
        if ident.len() < 2 || ident.len() > 64 {
            continue;
        }
        let lower = ident.to_ascii_lowercase();
        if is_sql_keyword(&lower) || !seen.insert(lower) {
            continue;
        }
        out.push(ident.to_string());
        if out.len() >= 120 {
            break;
        }
    }
    out
}

fn is_sql_keyword(lower: &str) -> bool {
    matches!(
        lower,
        "select"
            | "from"
            | "where"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "outer"
            | "full"
            | "cross"
            | "on"
            | "and"
            | "or"
            | "not"
            | "null"
            | "is"
            | "as"
            | "by"
            | "group"
            | "order"
            | "having"
            | "limit"
            | "offset"
            | "union"
            | "all"
            | "distinct"
            | "case"
            | "when"
            | "then"
            | "else"
            | "end"
            | "with"
            | "insert"
            | "into"
            | "update"
            | "delete"
            | "create"
            | "table"
            | "cast"
            | "sum"
            | "avg"
            | "count"
            | "min"
            | "max"
            | "coalesce"
            | "nullif"
            | "round"
            | "floor"
            | "ceil"
            | "date_format"
            | "date_add"
            | "date_sub"
            | "current_date"
            | "current_timestamp"
            | "interval"
            | "regexp"
            | "over"
            | "partition"
            | "rows"
            | "range"
            | "unbounded"
            | "preceding"
            | "following"
            | "lag"
            | "lead"
            | "first_value"
            | "last_value"
            | "if"
            | "date"
            | "datetime"
            | "timestamp"
            | "true"
            | "false"
    )
}

fn extract_metric_terms(content: &str) -> Vec<String> {
    static METRIC_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = METRIC_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(roi|roas|gmv|dau|mau|uv|pv|ctr|cvr|ltv|arpu|arppu|retention|revenue|cost|profit|margin|rate|ratio|amount|count|users|orders|buyers|sessions)\b|(?:留存|收入|成本|转化|客单价|复购|活跃|利润|毛利|人均|占比)")
            .expect("valid metric regex")
    });
    let mut seen = HashSet::new();
    let mut metrics: Vec<String> = re
        .captures_iter(content)
        .filter_map(|cap| {
            cap.get(1)
                .or_else(|| cap.get(0))
                .map(|m| m.as_str().to_string())
        })
        .filter(|s| seen.insert(s.to_lowercase()))
        .take(80)
        .collect();
    if looks_like_sql(content) {
        for ident in extract_sql_identifiers(content) {
            if metrics.len() >= 120 {
                break;
            }
            if looks_like_metric_identifier(&ident) && seen.insert(ident.to_lowercase()) {
                metrics.push(ident);
            }
        }
    }
    metrics
}

fn looks_like_metric_identifier(ident: &str) -> bool {
    let lower = ident.to_ascii_lowercase();
    let parts = lower.split('_').collect::<HashSet<_>>();
    [
        "roi",
        "roas",
        "gmv",
        "dau",
        "mau",
        "uv",
        "pv",
        "ctr",
        "cvr",
        "ltv",
        "arpu",
        "arppu",
        "retention",
        "revenue",
        "rev",
        "cost",
        "profit",
        "margin",
        "rate",
        "ratio",
        "amount",
        "count",
        "cnt",
        "users",
        "orders",
        "buyers",
        "sessions",
        "duration",
        "score",
    ]
    .iter()
    .any(|needle| {
        lower == *needle || lower.ends_with(&format!("_{needle}")) || parts.contains(needle)
    })
}

fn infer_chunk_type(content: &str, language: Option<&str>) -> String {
    if matches!(language, Some("sql")) || content.to_ascii_lowercase().contains("select ") {
        "sql_example".to_string()
    } else if matches!(language, Some("markdown")) && content.contains("```sql") {
        "sql_example".to_string()
    } else if !extract_metric_terms(content).is_empty() {
        "metric_definition".to_string()
    } else if matches!(language, Some("csv" | "json" | "yaml")) {
        "structured_reference".to_string()
    } else {
        "text".to_string()
    }
}

fn build_embedding_input(filename: &str, chunk: &ReferenceChunkDraft) -> String {
    format!(
        "file: {filename}\nchunk_type: {}\ntables: {}\ncolumns: {}\nmetrics: {}\nsummary: {}\n\n{}",
        chunk.chunk_type,
        chunk.tables.join(", "),
        chunk.columns.join(", "),
        chunk.metrics.join(", "),
        chunk.summary.clone().unwrap_or_default(),
        preview_chars(&chunk.content, 1800)
    )
}

async fn insert_reference_profile_embedding(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    chunk_id: &str,
    profile_vectors: &ReferenceProfileVectors,
    vector_index: usize,
) -> Result<()> {
    let vector = profile_vectors.vectors.get(vector_index).ok_or_else(|| {
        AppError::Internal("embedding provider returned fewer vectors than inputs".to_string())
    })?;
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_reference_chunk_embeddings \
         (tenant_id, chunk_id, profile_id, model, dimensions, embedding_json) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(tenant_id, chunk_id, profile_id) DO UPDATE SET \
           model = excluded.model, dimensions = excluded.dimensions, \
           embedding_json = excluded.embedding_json, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(chunk_id)
    .bind(&profile_vectors.profile.id)
    .bind(&profile_vectors.profile.config.model)
    .bind(i64::try_from(vector.len()).unwrap_or(i64::MAX))
    .bind(serde_json::to_string(vector)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn enqueue_reference_scope_reindex(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    profile: &crate::nl2sql::embedding_profiles::ResolvedProfile,
) {
    let datasource_ids = if datasource_id == "global" {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM data_sources WHERE tenant_id = ? AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .fetch_all(db)
        .await
        .unwrap_or_default()
    } else {
        vec![datasource_id.to_string()]
    };
    for datasource_id in datasource_ids {
        if let Err(error) = crate::nl2sql::embedding_profiles::enqueue_reindex(
            db,
            tenant_id,
            &datasource_id,
            profile,
        )
        .await
        {
            tracing::warn!(
                tenant_id,
                datasource_id,
                profile_id = %profile.id,
                error = %error,
                "failed to queue SQL knowledge profile backfill"
            );
        }
    }
}

async fn embed_reference_batch(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    inputs: &[String],
) -> Result<ReferenceEmbeddingBatch> {
    let profiles =
        crate::nl2sql::embedding_profiles::resolve_profiles(&state.db, tenant_id, Some("nl2sql"))
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to resolve embedding profiles: {error}"))
            })?;
    if datasource_id != "global" {
        crate::nl2sql::embedding_profiles::ensure_datasource_profiles(
            &state.db,
            tenant_id,
            datasource_id,
            &profiles,
        )
        .await
        .map_err(|error| {
            AppError::Internal(format!("failed to sync embedding profiles: {error}"))
        })?;
    }

    let local_model = EmbeddingModel::new_with_dimensions(
        &profiles.local.config.model,
        profiles.local.config.base_url.clone(),
        None,
        profiles.local.config.dimensions,
    );
    let local_vectors = local_model.embed_batch(inputs).await.map_err(|error| {
        AppError::Internal(format!("local SQL knowledge embedding failed: {error}"))
    })?;
    if local_vectors.len() != inputs.len() {
        return Err(AppError::Internal(format!(
            "local embedding returned {} vectors for {} SQL knowledge chunks",
            local_vectors.len(),
            inputs.len()
        )));
    }
    if let Some(vector) = local_vectors
        .iter()
        .find(|vector| vector.len() != profiles.local.config.effective_dimensions())
    {
        return Err(AppError::Internal(format!(
            "local embedding returned {} dimensions; expected {}",
            vector.len(),
            profiles.local.config.effective_dimensions()
        )));
    }
    let _ =
        crate::nl2sql::embedding_profiles::record_profile_success(&state.db, &profiles.local.id)
            .await;

    let mut api_vectors = None;
    if let Some(api_profile) = &profiles.api {
        let allowed =
            crate::nl2sql::embedding_profiles::circuit_allows_request(&state.db, &api_profile.id)
                .await
                .unwrap_or(true);
        if allowed {
            let api_model = EmbeddingModel::new_with_dimensions(
                &api_profile.config.model,
                api_profile.config.base_url.clone(),
                Some(api_profile.config.api_key.clone()),
                api_profile.config.dimensions,
            );
            match api_model.embed_batch(inputs).await {
                Ok(vectors) => {
                    if vectors.len() != inputs.len() {
                        let error = format!(
                            "API embedding returned {} vectors for {} SQL knowledge chunks",
                            vectors.len(),
                            inputs.len()
                        );
                        let _ = crate::nl2sql::embedding_profiles::record_profile_failure(
                            &state.db,
                            &api_profile.id,
                            &error,
                        )
                        .await;
                        enqueue_reference_scope_reindex(
                            &state.db,
                            tenant_id,
                            datasource_id,
                            api_profile,
                        )
                        .await;
                        tracing::warn!(
                            tenant_id,
                            datasource_id,
                            profile_id = %api_profile.id,
                            error = %error,
                            "SQL knowledge API embedding returned incomplete batch"
                        );
                        let _ = crate::nl2sql::embedding_failover::record_embedding_fallback_alert(
                            &state.db,
                            tenant_id,
                            "nl2sql",
                            &api_profile.config,
                            &error,
                        )
                        .await;
                        return Ok(ReferenceEmbeddingBatch {
                            local: ReferenceProfileVectors {
                                profile: profiles.local,
                                vectors: local_vectors,
                            },
                            api: None,
                        });
                    }
                    if let Some(vector) = vectors
                        .iter()
                        .find(|vector| vector.len() != api_profile.config.effective_dimensions())
                    {
                        let error = format!(
                            "API embedding returned {} dimensions; expected {}",
                            vector.len(),
                            api_profile.config.effective_dimensions()
                        );
                        let _ = crate::nl2sql::embedding_profiles::record_profile_failure(
                            &state.db,
                            &api_profile.id,
                            &error,
                        )
                        .await;
                        enqueue_reference_scope_reindex(
                            &state.db,
                            tenant_id,
                            datasource_id,
                            api_profile,
                        )
                        .await;
                        tracing::warn!(
                            tenant_id,
                            datasource_id,
                            profile_id = %api_profile.id,
                            error = %error,
                            "SQL knowledge API embedding returned incompatible dimensions"
                        );
                        let _ = crate::nl2sql::embedding_failover::record_embedding_fallback_alert(
                            &state.db,
                            tenant_id,
                            "nl2sql",
                            &api_profile.config,
                            &error,
                        )
                        .await;
                        return Ok(ReferenceEmbeddingBatch {
                            local: ReferenceProfileVectors {
                                profile: profiles.local,
                                vectors: local_vectors,
                            },
                            api: None,
                        });
                    }
                    let _ = crate::nl2sql::embedding_profiles::record_profile_success(
                        &state.db,
                        &api_profile.id,
                    )
                    .await;
                    let _ = crate::nl2sql::embedding_failover::resolve_embedding_fallback_alert(
                        &state.db,
                        tenant_id,
                        "nl2sql",
                        &api_profile.config,
                    )
                    .await;
                    api_vectors = Some(ReferenceProfileVectors {
                        profile: api_profile.clone(),
                        vectors,
                    });
                }
                Err(error) => {
                    let _ = crate::nl2sql::embedding_profiles::record_profile_failure(
                        &state.db,
                        &api_profile.id,
                        &error.to_string(),
                    )
                    .await;
                    enqueue_reference_scope_reindex(
                        &state.db,
                        tenant_id,
                        datasource_id,
                        api_profile,
                    )
                    .await;
                    let _ = crate::nl2sql::embedding_failover::record_embedding_fallback_alert(
                        &state.db,
                        tenant_id,
                        "nl2sql",
                        &api_profile.config,
                        &error.to_string(),
                    )
                    .await;
                    tracing::warn!(
                        tenant_id,
                        datasource_id,
                        profile_id = %api_profile.id,
                        error = %error,
                        "SQL knowledge API embedding failed; local vectors stored"
                    );
                }
            }
        } else {
            enqueue_reference_scope_reindex(&state.db, tenant_id, datasource_id, api_profile).await;
        }
    }

    Ok(ReferenceEmbeddingBatch {
        local: ReferenceProfileVectors {
            profile: profiles.local,
            vectors: local_vectors,
        },
        api: api_vectors,
    })
}

pub(crate) async fn rebuild_reference_profile(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    profile: &crate::nl2sql::embedding_profiles::ResolvedProfile,
) -> anyhow::Result<(usize, usize)> {
    let rows = sqlx::query(
        "SELECT c.id AS chunk_id, f.filename, COALESCE(c.chunk_type, 'text') AS chunk_type, \
                c.content_text, c.summary_text, c.extracted_tables_json, \
                c.extracted_columns_json, c.extracted_metrics_json \
         FROM nl2sql_reference_chunks c \
         JOIN nl2sql_reference_packs p \
           ON p.tenant_id = c.tenant_id AND p.id = c.pack_id \
         JOIN nl2sql_reference_files f \
           ON f.tenant_id = c.tenant_id AND f.id = c.file_id \
         WHERE c.tenant_id = ? AND p.enabled = 1 AND f.status = 'indexed' \
           AND (c.datasource_id = ? OR p.scope = 'tenant' \
             OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) \
                        WHERE json_each.value = ?)) \
         ORDER BY c.id",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await?;

    let mut chunk_ids = Vec::with_capacity(rows.len());
    let mut inputs = Vec::with_capacity(rows.len());
    for row in rows {
        let draft = ReferenceChunkDraft {
            index: 0,
            start_line: 0,
            end_line: 0,
            content: row.get::<String, _>("content_text"),
            keywords: None,
            chunk_type: row.get::<String, _>("chunk_type"),
            summary: row.get::<Option<String>, _>("summary_text"),
            tables: parse_json_string_vec(
                row.get::<Option<serde_json::Value>, _>("extracted_tables_json"),
            ),
            columns: parse_json_string_vec(
                row.get::<Option<serde_json::Value>, _>("extracted_columns_json"),
            ),
            metrics: parse_json_string_vec(
                row.get::<Option<serde_json::Value>, _>("extracted_metrics_json"),
            ),
        };
        chunk_ids.push(row.get::<String, _>("chunk_id"));
        inputs.push(build_embedding_input(
            &row.get::<String, _>("filename"),
            &draft,
        ));
    }

    let model = EmbeddingModel::new_with_dimensions(
        &profile.config.model,
        profile.config.base_url.clone(),
        (profile.config.profile_kind == crate::nl2sql::EmbeddingProfileKind::Api)
            .then(|| profile.config.api_key.clone()),
        profile.config.dimensions,
    );
    let expected_dimensions = profile.config.effective_dimensions();
    let mut vectors = Vec::with_capacity(inputs.len());
    for input_batch in inputs.chunks(64) {
        let batch = model.embed_batch(input_batch).await?;
        if batch.len() != input_batch.len() {
            anyhow::bail!(
                "embedding profile {} returned {} vectors for {} SQL knowledge inputs",
                profile.id,
                batch.len(),
                input_batch.len()
            );
        }
        for vector in &batch {
            if vector.len() != expected_dimensions {
                anyhow::bail!(
                    "embedding profile {} returned {} dimensions; expected {}",
                    profile.id,
                    vector.len(),
                    expected_dimensions
                );
            }
        }
        vectors.extend(batch);
    }

    let mut tx = db.begin().await?;
    for (chunk_id, vector) in chunk_ids.iter().zip(&vectors) {
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO nl2sql_reference_chunk_embeddings \
             (tenant_id, chunk_id, profile_id, model, dimensions, embedding_json) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(tenant_id, chunk_id, profile_id) DO UPDATE SET \
               model = excluded.model, dimensions = excluded.dimensions, \
               embedding_json = excluded.embedding_json, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(tenant_id)
        .bind(chunk_id)
        .bind(&profile.id)
        .bind(&profile.config.model)
        .bind(i64::try_from(vector.len()).unwrap_or(i64::MAX))
        .bind(serde_json::to_string(vector)?)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok((vectors.len(), inputs.len()))
}

async fn reference_profile_has_full_coverage(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    profile_id: &str,
) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT CASE WHEN COUNT(*) = 0 THEN 1
          WHEN COUNT(*) = SUM(CASE WHEN e.chunk_id IS NULL THEN 0 ELSE 1 END) THEN 1
          ELSE 0 END
         FROM nl2sql_reference_chunks c
         JOIN nl2sql_reference_packs p
           ON p.tenant_id = c.tenant_id AND p.id = c.pack_id
         JOIN nl2sql_reference_files f
           ON f.tenant_id = c.tenant_id AND f.id = c.file_id
         LEFT JOIN nl2sql_reference_chunk_embeddings e
           ON e.tenant_id = c.tenant_id AND e.chunk_id = c.id AND e.profile_id = ?
         WHERE c.tenant_id = ? AND p.enabled = 1 AND f.status = 'indexed'
           AND (c.datasource_id = ? OR p.scope = 'tenant'
             OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json)
                        WHERE json_each.value = ?))",
    )
    .bind(profile_id)
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(datasource_id)
    .fetch_one(db)
    .await
    .map(|value| value == 1)
    .unwrap_or(false)
}

async fn embed_reference_query(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
) -> Result<ReferenceQueryEmbedding> {
    let profiles =
        crate::nl2sql::embedding_profiles::resolve_profiles(&state.db, tenant_id, Some("nl2sql"))
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to resolve embedding profiles: {error}"))
            })?;

    let mut api_fallback_error = None;
    if let Some(api_profile) = &profiles.api {
        let circuit_allows =
            crate::nl2sql::embedding_profiles::circuit_allows_request(&state.db, &api_profile.id)
                .await
                .unwrap_or(false);
        let scoped_datasources = if datasource_id == "global" {
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM data_sources WHERE tenant_id = ? AND deleted_at IS NULL",
            )
            .bind(tenant_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
        } else {
            vec![datasource_id.to_string()]
        };
        let profile_ready =
            crate::nl2sql::embedding_profiles::active_profile_ready_for_datasources(
                &state.db,
                tenant_id,
                api_profile,
                &scoped_datasources,
            )
            .await
            .unwrap_or(false);
        if circuit_allows
            && profile_ready
            && reference_profile_has_full_coverage(
                &state.db,
                tenant_id,
                datasource_id,
                &api_profile.id,
            )
            .await
        {
            let model = EmbeddingModel::new_with_dimensions(
                &api_profile.config.model,
                api_profile.config.base_url.clone(),
                Some(api_profile.config.api_key.clone()),
                api_profile.config.dimensions,
            );
            match model.embed_batch(&[question.to_string()]).await {
                Ok(mut vectors) => {
                    let vector = vectors.pop().unwrap_or_default();
                    if vectors.is_empty()
                        && vector.len() == api_profile.config.effective_dimensions()
                    {
                        let _ = crate::nl2sql::embedding_profiles::record_profile_success(
                            &state.db,
                            &api_profile.id,
                        )
                        .await;
                        let _ =
                            crate::nl2sql::embedding_failover::resolve_embedding_fallback_alert(
                                &state.db,
                                tenant_id,
                                "nl2sql",
                                &api_profile.config,
                            )
                            .await;
                        return Ok(ReferenceQueryEmbedding {
                            profile_id: api_profile.id.clone(),
                            vector,
                        });
                    }
                    let error = format!(
                        "API query embedding returned an incompatible batch/dimension (remaining={}, dimensions={}, expected={})",
                        vectors.len(),
                        vector.len(),
                        api_profile.config.effective_dimensions()
                    );
                    let _ = crate::nl2sql::embedding_profiles::record_profile_failure(
                        &state.db,
                        &api_profile.id,
                        &error,
                    )
                    .await;
                    api_fallback_error = Some(error);
                }
                Err(error) => {
                    let _ = crate::nl2sql::embedding_profiles::record_profile_failure(
                        &state.db,
                        &api_profile.id,
                        &error.to_string(),
                    )
                    .await;
                    api_fallback_error = Some(error.to_string());
                }
            }
        }
    }

    let model = EmbeddingModel::new_with_dimensions(
        &profiles.local.config.model,
        profiles.local.config.base_url.clone(),
        None,
        profiles.local.config.dimensions,
    );
    let mut vectors = model
        .embed_batch(&[question.to_string()])
        .await
        .map_err(|error| {
            AppError::Internal(format!("local SQL knowledge embedding failed: {error}"))
        })?;
    let vector = vectors.pop().unwrap_or_default();
    if !vectors.is_empty() || vector.len() != profiles.local.config.effective_dimensions() {
        return Err(AppError::Internal(format!(
            "local SQL knowledge embedding returned an incompatible batch/dimension (remaining={}, dimensions={}, expected={})",
            vectors.len(),
            vector.len(),
            profiles.local.config.effective_dimensions()
        )));
    }
    if let (Some(api_profile), Some(error)) = (profiles.api.as_ref(), api_fallback_error.as_deref())
    {
        let _ = crate::nl2sql::embedding_failover::record_embedding_fallback_alert(
            &state.db,
            tenant_id,
            "nl2sql",
            &api_profile.config,
            error,
        )
        .await;
    }
    Ok(ReferenceQueryEmbedding {
        profile_id: profiles.local.id,
        vector,
    })
}

fn parse_embedding_json(raw: Option<&str>) -> Option<Vec<f32>> {
    let raw = raw?;
    serde_json::from_str::<Vec<f32>>(raw).ok()
}

fn parse_json_string_vec(value: Option<serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
        .unwrap_or_default()
}

fn append_reason(current: String, next: String) -> String {
    if current.is_empty() {
        next
    } else {
        format!("{current}; {next}")
    }
}

fn schema_overlap_score(tables: &[String], schema_tables: &HashSet<String>) -> f64 {
    if tables.is_empty() || schema_tables.is_empty() {
        return 0.0;
    }
    let matched = tables
        .iter()
        .filter(|t| super::table_ref_matches_set(t, schema_tables))
        .count();
    if matched == 0 {
        0.0
    } else {
        (matched as f64 / tables.len().max(1) as f64).min(1.0) * 1.4
    }
}

async fn load_schema_table_names(
    db: &sqlx::SqlitePool,
    datasource_id: &str,
) -> Result<HashSet<String>> {
    let row = sqlx::query::<sqlx::Sqlite>("SELECT schema_info FROM data_sources WHERE id = ?")
        .bind(datasource_id)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        return Ok(HashSet::new());
    };
    let schema = row
        .get::<Option<serde_json::Value>, _>("schema_info")
        .unwrap_or(serde_json::json!({"tables": []}));
    let tables = schema
        .get("tables")
        .and_then(|v| v.as_array())
        .or_else(|| schema.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = HashSet::new();
    for table in &tables {
        super::insert_schema_table_aliases(&mut out, table);
    }
    Ok(out)
}

fn is_zip_filename(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".zip")
}

fn safe_zip_path(input: &str) -> String {
    input
        .replace('\\', "/")
        .split('/')
        .filter(|part| {
            let p = part.trim();
            !p.is_empty() && p != "." && p != ".." && !p.starts_with("__MACOSX")
        })
        .map(safe_filename)
        .collect::<Vec<_>>()
        .join("/")
}

fn extract_join_hints(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains(" join "))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| preview_chars(line, 220))
        .take(20)
        .collect()
}

fn summarize_reference_text(text: &str) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let summary = lines
        .by_ref()
        .take(6)
        .map(|l| preview_chars(l, 180))
        .collect::<Vec<_>>()
        .join("\n");
    (!summary.is_empty()).then_some(summary)
}

fn preview_chars(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("\n...");
    }
    out
}

fn estimate_token_count(content: &str) -> usize {
    let char_count = content.chars().count();
    (char_count / 4).max(1)
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn sha256_hex_bytes(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_import_pool_timeout_is_transient_contention() {
        assert!(is_transient_sqlite_lock(&AppError::Database(
            sqlx::Error::PoolTimedOut,
        )));
    }

    async fn import_task_test_pool() -> sqlx::SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite");
        sqlx::query("CREATE TABLE aos_setup_lock (lock_id INTEGER PRIMARY KEY)")
            .execute(&db)
            .await
            .expect("SQLite writer lock schema");
        sqlx::query("INSERT INTO aos_setup_lock (lock_id) VALUES (1)")
            .execute(&db)
            .await
            .expect("SQLite writer lock fixture");
        sqlx::query("CREATE TABLE nl2sql_reference_packs (id TEXT PRIMARY KEY)")
            .execute(&db)
            .await
            .expect("reference pack schema");
        sqlx::query(include_str!(
            "../../../sqlite-migrations/0015_sql_knowledge_async_imports.sql"
        ))
        .execute(&db)
        .await
        .expect("import task schema");
        sqlx::query("INSERT INTO nl2sql_reference_packs (id) VALUES ('pack-1')")
            .execute(&db)
            .await
            .expect("reference pack fixture");
        db
    }

    #[tokio::test]
    async fn reference_import_task_claim_is_exclusive_and_preserves_resume_offset() {
        let db = import_task_test_pool().await;
        let manifest = serde_json::to_string(&vec![
            ReferenceImportManifestItem {
                filename: "one.sql".to_string(),
                media_type: None,
                staged_filename: "0000.upload".to_string(),
            },
            ReferenceImportManifestItem {
                filename: "two.sql".to_string(),
                media_type: None,
                staged_filename: "0001.upload".to_string(),
            },
        ])
        .expect("manifest");
        sqlx::query(
            "INSERT INTO nl2sql_reference_import_tasks \
             (id, tenant_id, user_id, pack_id, datasource_id, status, total_files, processed_files, manifest_json, staging_dir) \
             VALUES ('task-1', 'tenant-1', 'user-1', 'pack-1', 'ds-1', 'pending', 2, 1, ?, '/tmp/staged')",
        )
        .bind(manifest)
        .execute(&db)
        .await
        .expect("insert task");

        let claimed = claim_reference_import_task(&db)
            .await
            .expect("claim task")
            .expect("pending task");
        assert_eq!(claimed.id, "task-1");
        assert_eq!(claimed.processed_files, 1);
        assert_eq!(claimed.manifest.len(), 2);
        assert!(claim_reference_import_task(&db)
            .await
            .expect("second claim")
            .is_none());
    }

    #[test]
    fn knowledge_binding_policy_follows_datasource_visibility() {
        let tenant_policies = vec![
            ReferenceDatasourceBindingPolicy {
                visibility: "tenant".to_string(),
                owner_user_id: None,
            },
            ReferenceDatasourceBindingPolicy {
                visibility: "tenant".to_string(),
                owner_user_id: None,
            },
        ];
        assert_eq!(
            validate_reference_binding_policy_set(&tenant_policies).expect("tenant bindings"),
            "tenant"
        );

        let mixed_policies = vec![
            tenant_policies[0].clone(),
            ReferenceDatasourceBindingPolicy {
                visibility: "private".to_string(),
                owner_user_id: Some("user-1".to_string()),
            },
        ];
        assert!(validate_reference_binding_policy_set(&mixed_policies).is_err());

        let different_private_owners = vec![
            ReferenceDatasourceBindingPolicy {
                visibility: "private".to_string(),
                owner_user_id: Some("user-1".to_string()),
            },
            ReferenceDatasourceBindingPolicy {
                visibility: "private".to_string(),
                owner_user_id: Some("user-2".to_string()),
            },
        ];
        assert!(validate_reference_binding_policy_set(&different_private_owners).is_err());
    }

    #[tokio::test]
    async fn knowledge_space_listing_derives_visibility_from_bound_datasources() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite");
        sqlx::query(
            "CREATE TABLE data_sources (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT, visibility TEXT NOT NULL)",
        )
        .execute(&db)
        .await
        .expect("data source schema");
        sqlx::query(
            "CREATE TABLE nl2sql_reference_packs (\
             id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT NOT NULL, \
             datasource_id TEXT NOT NULL, datasource_bindings_json TEXT, name TEXT NOT NULL, \
             description TEXT, scope TEXT NOT NULL, tags_json TEXT, enabled INTEGER NOT NULL, \
             verified INTEGER NOT NULL, stale INTEGER NOT NULL, knowledge_kind TEXT NOT NULL, \
             metadata_json TEXT, created_at TEXT, updated_at TEXT)",
        )
        .execute(&db)
        .await
        .expect("pack schema");
        sqlx::query(
            "CREATE TABLE nl2sql_reference_files (\
             id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, pack_id TEXT NOT NULL, datasource_id TEXT NOT NULL, \
             filename TEXT NOT NULL, media_type TEXT, language TEXT, size_bytes INTEGER, content_hash TEXT, \
             status TEXT, error TEXT, summary TEXT, version_no INTEGER, metadata_json TEXT, created_at TEXT, updated_at TEXT)",
        )
        .execute(&db)
        .await
        .expect("file schema");
        sqlx::query(
            "CREATE TABLE nl2sql_reference_chunks (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, pack_id TEXT NOT NULL, file_id TEXT NOT NULL)",
        )
        .execute(&db)
        .await
        .expect("chunk schema");

        sqlx::query(
            "INSERT INTO data_sources VALUES \
             ('tenant-ds', 'tenant-1', NULL, 'tenant'), \
             ('private-ds', 'tenant-1', 'owner-1', 'private')",
        )
        .execute(&db)
        .await
        .expect("data sources");
        sqlx::query(
            "INSERT INTO nl2sql_reference_packs VALUES \
             ('global-pack', 'tenant-1', 'admin-1', 'global', '[]', 'Global', NULL, 'tenant', '[]', 1, 1, 0, 'sql_knowledge', NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP), \
             ('shared-pack', 'tenant-1', 'owner-1', 'tenant-ds', '[\"tenant-ds\"]', 'Shared', NULL, 'datasource', '[]', 1, 1, 0, 'sql_knowledge', NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP), \
             ('private-pack', 'tenant-1', 'owner-1', 'private-ds', '[\"private-ds\"]', 'Private', NULL, 'datasource', '[]', 1, 1, 0, 'sql_knowledge', NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("knowledge spaces");

        let visible = load_reference_packs(&db, "tenant-1", None, true, "member-2", false)
            .await
            .expect("visible knowledge spaces");
        let visible_ids = visible
            .iter()
            .map(|pack| pack.id.as_str())
            .collect::<HashSet<_>>();
        assert!(visible_ids.contains("global-pack"));
        assert!(visible_ids.contains("shared-pack"));
        assert!(!visible_ids.contains("private-pack"));
    }

    #[test]
    fn reference_binding_request_accepts_camel_case_payload() {
        let parsed: ReferenceBindingRequest = serde_json::from_value(serde_json::json!({
            "packIds": ["pack-1"],
            "fileIds": ["file-1"],
            "includeAll": false
        }))
        .expect("camelCase reference bindings should deserialize");

        assert_eq!(parsed.pack_ids, vec!["pack-1"]);
        assert_eq!(parsed.file_ids, vec!["file-1"]);
        assert!(!parsed.include_all);
        assert!(parsed.is_active());
    }

    #[test]
    fn reference_search_request_accepts_frontend_and_legacy_payloads() {
        let frontend: ReferenceSearchRequest = serde_json::from_value(serde_json::json!({
            "datasourceId": "ds-1",
            "question": "按渠道统计收入",
            "referenceBindings": {
                "packIds": ["pack-1"],
                "fileIds": [],
                "includeAll": false
            },
            "limit": 5
        }))
        .expect("frontend reference search payload should deserialize");
        assert_eq!(frontend.datasource_id, "ds-1");
        assert_eq!(frontend.reference_bindings.pack_ids, vec!["pack-1"]);

        let legacy: ReferenceSearchRequest = serde_json::from_value(serde_json::json!({
            "data_source_id": "ds-2",
            "question": "按渠道统计收入",
            "reference_bindings": {
                "packIds": [],
                "fileIds": ["file-1"],
                "includeAll": false
            }
        }))
        .expect("legacy snake_case reference search payload should deserialize");
        assert_eq!(legacy.datasource_id, "ds-2");
        assert_eq!(legacy.reference_bindings.file_ids, vec!["file-1"]);
    }

    #[test]
    fn sql_knowledge_search_request_accepts_camel_case_payload() {
        let parsed: SqlKnowledgeSearchRequest = serde_json::from_value(serde_json::json!({
            "datasourceId": "ds-1",
            "question": "老板想看 ROI 口径",
            "limit": 10
        }))
        .expect("SQL knowledge search payload should deserialize");

        assert_eq!(parsed.datasource_id.as_deref(), Some("ds-1"));
        assert_eq!(parsed.limit, Some(10));
    }

    #[test]
    fn markdown_sql_blocks_are_chunked_as_reusable_examples() {
        let markdown = r#"# Revenue Playbook

Use this demo when the business asks for channel level payback.

```sql
SELECT
  channel,
  SUM(ad_revenue) AS revenue,
  SUM(ua_cost + incentive_cost) AS total_cost,
  SUM(ad_revenue) / NULLIF(SUM(ua_cost + incentive_cost), 0) AS roi
FROM fact_campaign_daily
WHERE dt BETWEEN {{start_dt}} AND {{end_dt}}
GROUP BY channel
```

Metric note: ROI = revenue / total cost.
"#;

        let chunks = chunk_reference_text(markdown, Some("markdown"));

        assert!(!chunks.is_empty());
        let sql_chunk = chunks
            .iter()
            .find(|chunk| chunk.chunk_type == "sql_example")
            .expect("markdown SQL code block should become a SQL example chunk");
        assert!(sql_chunk
            .tables
            .contains(&"fact_campaign_daily".to_string()));
        assert!(sql_chunk
            .columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case("channel")));
        assert!(sql_chunk
            .metrics
            .iter()
            .any(|metric| metric.eq_ignore_ascii_case("roi")));
    }

    #[test]
    fn sql_metadata_extraction_handles_generic_fields_and_joins() {
        let sql = r#"
WITH orders AS (
  SELECT user_id, order_dt, channel, paid_amount
  FROM mart_orders
  WHERE order_dt >= '2026-06-01'
)
SELECT
  o.channel,
  COUNT(DISTINCT o.user_id) AS buyers,
  SUM(o.paid_amount) AS revenue,
  SUM(a.spend_amount) AS acquisition_cost,
  SUM(o.paid_amount) / NULLIF(SUM(a.spend_amount), 0) AS roas
FROM orders o
JOIN dim_account a ON a.user_id = o.user_id
GROUP BY o.channel
"#;

        let tables = extract_sql_table_names(sql);
        let columns = extract_column_like_terms(sql);
        let metrics = extract_metric_terms(sql);

        assert!(tables.contains(&"mart_orders".to_string()));
        assert!(tables.contains(&"dim_account".to_string()));
        assert!(columns.iter().any(|c| c.eq_ignore_ascii_case("channel")));
        assert!(columns
            .iter()
            .any(|c| c.eq_ignore_ascii_case("paid_amount")));
        assert!(columns
            .iter()
            .any(|c| c.eq_ignore_ascii_case("spend_amount")));
        assert!(metrics.iter().any(|m| m.eq_ignore_ascii_case("roas")));
    }

    #[test]
    fn sql_metadata_extraction_keeps_trino_qualified_table_names() {
        let sql = r#"
SELECT bo.order_id, oi.item_id, u.user_id
FROM iceberg.mps_prod.business_order bo
JOIN `hive`.`ods`.`order_item` oi ON oi.order_id = bo.order_id
JOIN "iceberg"."dim"."user_profile" u ON u.user_id = bo.user_id
"#;

        let tables = extract_sql_table_names(sql);

        assert!(tables.contains(&"iceberg.mps_prod.business_order".to_string()));
        assert!(tables.contains(&"hive.ods.order_item".to_string()));
        assert!(tables.contains(&"iceberg.dim.user_profile".to_string()));
    }

    #[test]
    fn metric_extraction_uses_generic_sql_aliases_not_industry_terms() {
        let sql = r#"
SELECT
  region,
  SUM(gross_amount) AS gross_amount,
  SUM(refund_amount) AS refund_amount,
  COUNT(DISTINCT buyer_id) AS buyers,
  SUM(gross_amount - refund_amount) / NULLIF(COUNT(DISTINCT buyer_id), 0) AS margin_per_buyer
FROM commerce_daily
GROUP BY region
"#;

        let metrics = extract_metric_terms(sql);

        assert!(metrics
            .iter()
            .any(|m| m.eq_ignore_ascii_case("gross_amount")));
        assert!(metrics
            .iter()
            .any(|m| m.eq_ignore_ascii_case("refund_amount")));
        assert!(metrics
            .iter()
            .any(|m| m.eq_ignore_ascii_case("margin_per_buyer")));
    }

    #[test]
    fn zip_paths_are_sanitized_without_directory_escape() {
        let sanitized = safe_zip_path("../unsafe/../../业务 SQL/demo?.sql");
        let windows_path = safe_zip_path(r"..\unsafe\..\folder\demo.sql");
        let upload_name = safe_reference_upload_name("team/sql/daily roi.sql");

        assert!(!sanitized.contains(".."));
        assert!(!sanitized.starts_with('/'));
        assert!(sanitized.ends_with("demo_.sql"));
        assert_eq!(windows_path, "unsafe/folder/demo.sql");
        assert_eq!(upload_name, "team/sql/daily roi.sql");
    }

    #[test]
    fn schema_overlap_rewards_matching_live_tables() {
        let mut live = HashSet::new();
        live.insert("fact_campaign_daily".to_string());
        live.insert("dim_account".to_string());

        let matching = schema_overlap_score(
            &["fact_campaign_daily".to_string(), "dim_account".to_string()],
            &live,
        );
        let missing = schema_overlap_score(&["legacy_table".to_string()], &live);

        assert!(matching > 1.0);
        assert_eq!(missing, 0.0);
    }

    #[test]
    fn schema_overlap_matches_trino_table_aliases() {
        let mut live = HashSet::new();
        crate::routes::nl2sql::insert_schema_table_aliases(
            &mut live,
            &serde_json::json!({
                "table_name": "iceberg.mps_prod.business_order",
                "fully_qualified_name": "iceberg.mps_prod.business_order",
                "qualified_name": "mps_prod.business_order",
                "physical_table_name": "business_order",
                "name": "business_order",
                "catalog": "iceberg",
                "schema": "mps_prod"
            }),
        );

        assert!(live.contains("iceberg.mps_prod.business_order"));
        assert!(live.contains("mps_prod.business_order"));
        assert!(live.contains("business_order"));
        assert!(
            schema_overlap_score(&["business_order".to_string()], &live) > 0.0,
            "bare SQL examples should match live fully qualified Trino tables"
        );
        assert!(
            schema_overlap_score(&["mps_prod.business_order".to_string()], &live) > 0.0,
            "schema.table SQL examples should match live catalog.schema.table tables"
        );
        assert!(
            schema_overlap_score(&["iceberg.mps_prod.business_order".to_string()], &live) > 0.0,
            "fully-qualified SQL examples should match directly"
        );
    }

    fn test_candidate_chunk(index: u32, content: &str) -> CandidateChunk {
        CandidateChunk {
            pack_id: "pack-1".to_string(),
            pack_name: "SQL 知识库".to_string(),
            file_id: "file-1".to_string(),
            filename: "long_metric.sql".to_string(),
            chunk_id: format!("chunk-{index}"),
            chunk_index: index,
            language: Some("sql".to_string()),
            start_line: index * 80 + 1,
            end_line: index * 80 + 80,
            content: content.to_string(),
            keywords: None,
            chunk_type: "sql_example".to_string(),
            summary: None,
            metadata: None,
            extracted_tables: vec!["fact_metric".to_string()],
            extracted_columns: vec!["dt".to_string()],
            extracted_metrics: vec!["ecpm".to_string()],
            embedding_model: None,
            embedding: None,
            fulltext_score: 1.0,
            verified: true,
            stale: false,
            file_age_days: Some(0),
            pack_scope: "datasource".to_string(),
        }
    }

    #[test]
    fn sql_example_context_opens_wide_file_context() {
        let chunks = vec![
            test_candidate_chunk(0, "WITH base AS (SELECT dt, user_id FROM fact_metric)"),
            test_candidate_chunk(
                1,
                "mid AS (SELECT dt, COUNT(*) AS show_pv FROM base GROUP BY dt)",
            ),
            test_candidate_chunk(2, "SELECT dt, AVG(ecpm) AS ecpm FROM mid GROUP BY dt"),
        ];
        let grouped = group_candidate_chunks_by_file(&chunks);

        let expanded = expand_candidate_context(&chunks[1], &grouped);

        assert!(expanded.content.contains("WITH base"));
        assert!(expanded.content.contains("AVG(ecpm)"));
        assert!(expanded.reason_suffix.contains("sql_example_open"));
    }

    #[test]
    fn rg_search_terms_include_cjk_windows() {
        let terms = search_terms_for_tool("查最近订单信息");

        assert!(terms.contains(&"订单".to_string()));
        assert!(terms.contains(&"订单信".to_string()));
    }

    #[test]
    fn rg_like_terms_prefer_searchable_business_phrases() {
        let terms = rg_like_terms("昨天的订单金额和GMV");

        assert!(terms.contains(&"订单金额".to_string()));
        assert!(terms.contains(&"订单".to_string()));
        assert!(terms.contains(&"gmv".to_string()));
        assert!(terms.iter().all(|term| term.chars().count() >= 2));
    }

    #[test]
    fn rg_score_matches_business_term_inside_sql() {
        let mut chunk = test_candidate_chunk(
            0,
            "-- 订单金额口径\nSELECT order_id, gross_amount FROM business_order WHERE dt='${dt}'",
        );
        chunk.extracted_tables = vec!["business_order".to_string()];
        chunk.extracted_metrics = vec!["gross_amount".to_string()];

        let scored = rg_score_chunk(&chunk, "订单金额").expect("rg should match order SQL");

        assert!(scored.0 > 0.0);
    }

    #[test]
    fn exact_deterministic_sql_match_beats_weak_semantic_similarity() {
        let mut exact = test_candidate_chunk(0, "SELECT roi FROM campaign_performance");
        exact.fulltext_score = 0.0;
        exact.embedding = Some(vec![0.0, 1.0]);
        let mut semantic_only =
            test_candidate_chunk(1, "SELECT revenue / spend FROM campaign_performance");
        semantic_only.fulltext_score = 0.0;
        semantic_only.embedding = Some(vec![1.0, 0.0]);
        let query_tokens = tokenize_for_reference("ROI");
        let empty = HashSet::new();
        let exact_score = score_auto_query_chunk(
            exact,
            "ROI",
            &query_tokens,
            &empty,
            &empty,
            Some(&[0.0, 1.0]),
            &empty,
        )
        .0;
        let semantic_score = score_auto_query_chunk(
            semantic_only,
            "ROI",
            &query_tokens,
            &empty,
            &empty,
            Some(&[0.0, 1.0]),
            &empty,
        )
        .0;

        assert!(
            exact_score > semantic_score,
            "exact SQL evidence must outrank semantic-only evidence: exact={exact_score}, semantic={semantic_score}"
        );
    }

    #[test]
    fn hybrid_candidate_merge_keeps_lexical_score_and_profile_vector() {
        let mut lexical = test_candidate_chunk(0, "SELECT roi FROM campaign_performance");
        lexical.fulltext_score = 6.0;
        lexical.embedding = None;
        let mut semantic = lexical.clone();
        semantic.fulltext_score = 0.0;
        semantic.embedding_model = Some("semantic-profile".to_string());
        semantic.embedding = Some(vec![0.2, 0.8]);

        let mut candidates = HashMap::new();
        merge_hybrid_candidate(&mut candidates, lexical);
        merge_hybrid_candidate(&mut candidates, semantic);

        let merged = candidates.get("chunk-0").expect("merged candidate");
        assert_eq!(merged.fulltext_score, 6.0);
        assert_eq!(merged.embedding.as_deref(), Some([0.2, 0.8].as_slice()));
        assert_eq!(merged.embedding_model.as_deref(), Some("semantic-profile"));
    }

    #[test]
    fn knowledge_outline_extracts_sql_structure() {
        let outline = build_reference_outline(
            "orders.sql",
            "WITH base AS (\nSELECT order_id, gross_amount FROM business_order WHERE dt='${dt}'\n)\nSELECT SUM(gross_amount) AS gmv FROM base",
        );

        assert!(outline["tables"].to_string().contains("business_order"));
        assert!(outline["ctes"].to_string().contains("base"));
        assert!(outline["parameters"].to_string().contains("dt"));
    }

    #[tokio::test]
    async fn sqlite_fulltext_fallback_search_builds_valid_sql() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::query(
            "CREATE TABLE nl2sql_reference_packs (\
             id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT NOT NULL, \
             datasource_id TEXT NOT NULL, datasource_bindings_json TEXT, name TEXT NOT NULL, \
             description TEXT, scope TEXT NOT NULL, tags_json TEXT, enabled INTEGER NOT NULL, \
             verified INTEGER NOT NULL, stale INTEGER NOT NULL, knowledge_kind TEXT NOT NULL, \
             metadata_json TEXT, created_at TEXT, updated_at TEXT)",
        )
        .execute(&db)
        .await
        .expect("packs table");
        sqlx::query(
            "CREATE TABLE nl2sql_reference_files (\
             id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, pack_id TEXT NOT NULL, \
             datasource_id TEXT NOT NULL, filename TEXT NOT NULL, media_type TEXT, language TEXT, \
             size_bytes INTEGER, content_hash TEXT, status TEXT, error TEXT, summary TEXT, \
             version_no INTEGER, metadata_json TEXT, created_at TEXT, updated_at TEXT)",
        )
        .execute(&db)
        .await
        .expect("files table");
        sqlx::query(
            "CREATE TABLE nl2sql_reference_chunks (\
             id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, pack_id TEXT NOT NULL, file_id TEXT NOT NULL, \
             datasource_id TEXT NOT NULL, chunk_index INTEGER, language TEXT, start_line INTEGER, \
             end_line INTEGER, content_text TEXT, keywords_text TEXT, chunk_type TEXT, summary_text TEXT, \
             metadata_json TEXT, extracted_tables_json TEXT, extracted_columns_json TEXT, \
             extracted_metrics_json TEXT, embedding_model TEXT, embedding_json TEXT)",
        )
        .execute(&db)
        .await
        .expect("chunks table");
        sqlx::query(
            "CREATE TABLE nl2sql_reference_chunk_embeddings (\
             tenant_id TEXT NOT NULL, chunk_id TEXT NOT NULL, profile_id TEXT NOT NULL, \
             model TEXT, embedding_json TEXT)",
        )
        .execute(&db)
        .await
        .expect("profile embeddings table");
        sqlx::query(
            "INSERT INTO nl2sql_reference_packs VALUES \
             ('p1', 't1', 'u1', 'd1', '[\"d1\"]', '订单知识', NULL, 'datasource', '[]', 1, 1, 0, 'sql_knowledge', NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("pack row");
        sqlx::query(
            "INSERT INTO nl2sql_reference_files VALUES \
             ('f1', 't1', 'p1', 'd1', 'orders.sql', 'text/sql', 'sql', 10, 'hash', 'indexed', NULL, NULL, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("file row");
        sqlx::query(
            "INSERT INTO nl2sql_reference_chunks VALUES \
             ('c1', 't1', 'p1', 'f1', 'd1', 0, 'sql', 1, 2, \
              'SELECT SUM(amount) AS revenue FROM orders', 'revenue,orders', 'sql_example', NULL, \
              NULL, '[\"orders\"]', '[\"amount\"]', '[\"revenue\"]', NULL, NULL)",
        )
        .execute(&db)
        .await
        .expect("chunk row");
        sqlx::query(
            "INSERT INTO nl2sql_reference_chunk_embeddings VALUES \
             ('t1', 'c1', 'profile-1', 'semantic-model', '[0.1,0.9]')",
        )
        .execute(&db)
        .await
        .expect("profile embedding row");

        validate_file_ids_with_builder(&db, "t1", "d1", &["f1".to_string()])
            .await
            .expect("file binding SQL should be valid");
        let candidates =
            load_candidate_chunks(&db, "t1", "d1", true, &[], &[], Some("revenue"), None)
                .await
                .expect("fallback LIKE query should be valid SQLite");

        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].fulltext_score > 0.0);

        let semantic_candidates =
            load_candidate_chunks(&db, "t1", "d1", true, &[], &[], None, Some("profile-1"))
                .await
                .expect("semantic lane without a lexical filter should be valid SQLite");
        assert_eq!(semantic_candidates.len(), 1);
        assert_eq!(
            semantic_candidates[0].embedding.as_deref(),
            Some([0.1, 0.9].as_slice())
        );
    }

    #[tokio::test]
    async fn approved_feedback_exemplar_is_tenant_and_datasource_scoped() {
        let db = crate::test_sqlite_pool().await;
        sqlx::query(
            "INSERT INTO feedback_learning_events
                (id, tenant_id, scope, correction_json, approved, regression_case_id, created_at)
             VALUES ('f1', 'tenant-a', 'datasource:ds-a', ?, 1, 'case-1', CURRENT_TIMESTAMP)",
        )
        .bind(
            serde_json::json!({
                "question": "昨天 ROI",
                "correctedSql": "SELECT app, SUM(revenue) / SUM(cost) AS roi FROM fact GROUP BY app"
            })
            .to_string(),
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO feedback_regression_cases
                (id, tenant_id, datasource_id, feedback_event_id, analytic_intent_id,
                 original_ir_hash, original_sql_hash, corrected_sql_hash,
                 semantic_diff_json, verification_json, execution_evidence_json,
                 fixture_json, status, approved_by, approved_at, last_verified_at)
             VALUES ('case-1', 'tenant-a', 'ds-a', 'f1', 'query-1',
                     'ir-hash', 'old-hash', 'new-hash', '{}', '{}', NULL, '{}',
                     'approved', 'owner', CURRENT_TIMESTAMP, NULL)",
        )
        .execute(&db)
        .await
        .unwrap();
        let mut own = Vec::new();
        append_approved_feedback_references(
            &db,
            "tenant-a",
            "ds-a",
            "昨天 ROI 哪个 app 最好",
            &mut own,
            6,
        )
        .await
        .unwrap();
        assert!(own.is_empty());
        sqlx::query(
            "UPDATE feedback_regression_cases
             SET status = 'verified', execution_evidence_json = '{}',
                 last_verified_at = CURRENT_TIMESTAMP WHERE id = 'case-1'",
        )
        .execute(&db)
        .await
        .unwrap();
        append_approved_feedback_references(
            &db,
            "tenant-a",
            "ds-a",
            "昨天 ROI 哪个 app 最好",
            &mut own,
            6,
        )
        .await
        .unwrap();
        assert_eq!(own.len(), 1);
        assert!(own[0].content.contains("SUM(revenue)"));

        let mut other = Vec::new();
        append_approved_feedback_references(&db, "tenant-b", "ds-a", "昨天 ROI", &mut other, 6)
            .await
            .unwrap();
        assert!(other.is_empty());
    }
}
