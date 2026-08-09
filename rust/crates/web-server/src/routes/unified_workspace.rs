//! Security policies and administrative publication APIs for the unified workspace.

use axum::extract::{Extension, Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::BTreeSet;
use std::path::{Component, Path as FsPath, PathBuf};

use crate::auth::Claims;
use crate::error::{AppError, Result as AppResult};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublishSharedResourceRequest {
    resource_type: String,
    #[serde(default)]
    resource_id: Option<String>,
    virtual_path: String,
    #[serde(default)]
    source_path: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    grantee_user_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSharedGrantsRequest {
    action: String,
    user_ids: Vec<String>,
}

#[derive(Debug)]
struct SharedSource {
    owner_user_id: String,
    resource_type: String,
    resource_id: String,
    version: String,
    content_hash: String,
    size_bytes: u64,
    mime_type: Option<String>,
    metadata: Value,
}

pub(crate) async fn publish_shared_resource(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<PublishSharedResourceRequest>,
) -> AppResult<Json<Value>> {
    require_workspace_admin(&claims)?;
    let virtual_path = normalize_shared_publish_path(&request.virtual_path)?;
    let source = resolve_shared_source(&state, &claims, &request).await?;
    let grantees = validate_grantees(&state, &claims.tenant_id, &request.grantee_user_ids).await?;
    let workspace_id = tenant_shared_workspace_id(&claims.tenant_id);
    let entry_id = format!("shared-{}", uuid::Uuid::new_v4());
    let mut transaction = state.db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut transaction).await?;
    ensure_tenant_shared_workspace(&mut transaction, &claims, &workspace_id).await?;
    // The shared workspace row is the serialization point for concurrent
    // publishes, including the first publication of a new virtual path.
    sqlx::query::<sqlx::Sqlite>(
        "SELECT id FROM agent_workspaces
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&workspace_id)
    .fetch_one(&mut *transaction)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_workspace_entries
         SET is_current = 0, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND workspace_id = ? AND virtual_path = ?
           AND enabled = 1 AND is_current = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&workspace_id)
    .bind(&virtual_path)
    .execute(&mut *transaction)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_workspace_entries
            (id, tenant_id, owner_user_id, workspace_id, visibility, resource_type,
             resource_id, virtual_path, version, content_hash, size_bytes, mime_type,
             metadata_json, enabled, is_current)
         VALUES (?, ?, ?, ?, 'tenant_shared', ?, ?, ?, ?, ?, ?, ?, ?, 1, 1)",
    )
    .bind(&entry_id)
    .bind(&claims.tenant_id)
    .bind(&source.owner_user_id)
    .bind(&workspace_id)
    .bind(&source.resource_type)
    .bind(&source.resource_id)
    .bind(&virtual_path)
    .bind(&source.version)
    .bind(&source.content_hash)
    .bind(crate::sqlite_i64(source.size_bytes))
    .bind(&source.mime_type)
    .bind(&source.metadata)
    .execute(&mut *transaction)
    .await?;
    for user_id in &grantees {
        upsert_shared_grant(
            &mut transaction,
            &claims.tenant_id,
            &workspace_id,
            &entry_id,
            &source.resource_id,
            user_id,
        )
        .await?;
        bump_personal_workspace_acl(&mut transaction, &claims.tenant_id, user_id).await?;
    }
    bump_workspace_acl(&mut transaction, &claims.tenant_id, &workspace_id).await?;
    transaction.commit().await?;
    Ok(Json(json!({
        "id": entry_id,
        "path": format!("/{virtual_path}"),
        "resourceType": source.resource_type,
        "resourceId": source.resource_id,
        "version": source.version,
        "granteeUserIds": grantees,
    })))
}

pub(crate) async fn update_shared_resource_grants(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(entry_id): Path<String>,
    Json(request): Json<UpdateSharedGrantsRequest>,
) -> AppResult<Json<Value>> {
    require_workspace_admin(&claims)?;
    let action = request.action.trim().to_ascii_lowercase();
    if !matches!(action.as_str(), "grant" | "revoke") {
        return Err(AppError::ValidationError(
            "action must be `grant` or `revoke`".to_string(),
        ));
    }
    let user_ids = validate_grantees(&state, &claims.tenant_id, &request.user_ids).await?;
    let mut transaction = state.db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut transaction).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT workspace_id, resource_id FROM agent_workspace_entries
         WHERE tenant_id = ? AND id = ? AND visibility = 'tenant_shared'
           AND enabled = 1 AND is_current = 1 AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&entry_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound("shared workspace resource".to_string()))?;
    let workspace_id = row.get::<String, _>("workspace_id");
    let resource_id = row.get::<String, _>("resource_id");
    for user_id in &user_ids {
        if action == "grant" {
            upsert_shared_grant(
                &mut transaction,
                &claims.tenant_id,
                &workspace_id,
                &entry_id,
                &resource_id,
                user_id,
            )
            .await?;
        } else {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE agent_workspace_grants
                 SET enabled = 0, revoked_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND workspace_id = ? AND entry_id = ?
                   AND resource_id = ?
                   AND grantee_user_id = ?",
            )
            .bind(&claims.tenant_id)
            .bind(&workspace_id)
            .bind(&entry_id)
            .bind(&resource_id)
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
        }
        bump_personal_workspace_acl(&mut transaction, &claims.tenant_id, user_id).await?;
    }
    bump_workspace_acl(&mut transaction, &claims.tenant_id, &workspace_id).await?;
    transaction.commit().await?;
    Ok(Json(json!({
        "id": entry_id,
        "action": action,
        "userIds": user_ids,
    })))
}

pub(crate) async fn unpublish_shared_resource(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(entry_id): Path<String>,
) -> AppResult<Json<Value>> {
    require_workspace_admin(&claims)?;
    let mut transaction = state.db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut transaction).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT workspace_id, resource_id FROM agent_workspace_entries
         WHERE tenant_id = ? AND id = ? AND visibility = 'tenant_shared'
           AND enabled = 1 AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&entry_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound("shared workspace resource".to_string()))?;
    let workspace_id = row.get::<String, _>("workspace_id");
    let resource_id = row.get::<String, _>("resource_id");
    let grantees = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT grantee_user_id FROM agent_workspace_grants
         WHERE tenant_id = ? AND workspace_id = ? AND entry_id = ?
           AND resource_id = ?
           AND enabled = 1 AND revoked_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&workspace_id)
    .bind(&entry_id)
    .bind(&resource_id)
    .fetch_all(&mut *transaction)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_workspace_entries
         SET enabled = 0, is_current = 0, deleted_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&entry_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_workspace_grants
         SET enabled = 0, revoked_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND workspace_id = ? AND entry_id = ? AND resource_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&workspace_id)
    .bind(&entry_id)
    .bind(&resource_id)
    .execute(&mut *transaction)
    .await?;
    for user_id in &grantees {
        bump_personal_workspace_acl(&mut transaction, &claims.tenant_id, user_id).await?;
    }
    bump_workspace_acl(&mut transaction, &claims.tenant_id, &workspace_id).await?;
    transaction.commit().await?;
    Ok(Json(json!({"id": entry_id, "unpublished": true})))
}

pub(crate) async fn list_published_shared_resources(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> AppResult<Json<Value>> {
    require_workspace_admin(&claims)?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT e.id, e.virtual_path, e.resource_type, e.resource_id, e.version,
                CAST(e.size_bytes AS INTEGER) AS size_bytes, CAST(e.updated_at AS TEXT) AS updated_at,
                COUNT(g.id) AS grant_count
         FROM agent_workspace_entries e
         LEFT JOIN agent_workspace_grants g
           ON g.tenant_id = e.tenant_id AND g.workspace_id = e.workspace_id
          AND g.entry_id = e.id AND g.enabled = 1 AND g.revoked_at IS NULL
         WHERE e.tenant_id = ? AND e.visibility = 'tenant_shared'
           AND e.enabled = 1 AND e.is_current = 1 AND e.deleted_at IS NULL
         GROUP BY e.id, e.virtual_path, e.resource_type, e.resource_id, e.version,
                  e.size_bytes, e.updated_at
         ORDER BY e.virtual_path ASC, e.id ASC LIMIT 1000",
    )
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "items": rows.into_iter().map(|row| json!({
            "id": row.get::<String, _>("id"),
            "path": format!("/{}", row.get::<String, _>("virtual_path").trim_start_matches('/')),
            "resourceType": row.get::<String, _>("resource_type"),
            "resourceId": row.get::<String, _>("resource_id"),
            "version": row.get::<String, _>("version"),
            "sizeBytes": row.try_get::<u64, _>("size_bytes").unwrap_or(0),
            "updatedAt": row.get::<String, _>("updated_at"),
            "grantCount": row.try_get::<u64, _>("grant_count").unwrap_or(0),
        })).collect::<Vec<_>>()
    })))
}

fn require_workspace_admin(claims: &Claims) -> AppResult<()> {
    if matches!(claims.role.as_str(), "admin" | "superadmin") {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn normalize_shared_publish_path(raw: &str) -> AppResult<String> {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    if raw.chars().any(char::is_control)
        || raw.contains('\\')
        || ["%2e", "%2f", "%5c", "%00"]
            .iter()
            .any(|encoded| lower.contains(encoded))
    {
        return Err(AppError::ValidationError(
            "invalid shared workspace path".to_string(),
        ));
    }
    let raw = raw.trim_start_matches('/');
    let raw = raw.strip_prefix("shared/").unwrap_or(raw);
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(AppError::ValidationError(
                    "shared workspace path traversal is not allowed".to_string(),
                ))
            }
            value if value.contains(':') => {
                return Err(AppError::ValidationError(
                    "shared workspace path cannot contain a physical prefix".to_string(),
                ))
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        return Err(AppError::ValidationError(
            "shared workspace path must identify a file".to_string(),
        ));
    }
    Ok(format!("shared/{}", parts.join("/")))
}

async fn resolve_shared_source(
    state: &AppState,
    claims: &Claims,
    request: &PublishSharedResourceRequest,
) -> AppResult<SharedSource> {
    let resource_type = request.resource_type.trim().to_ascii_lowercase();
    if resource_type == "shared_text" || resource_type == "text" {
        let content = request
            .content
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::ValidationError("content is required".to_string()))?;
        if content.len() > 1024 * 1024 {
            return Err(AppError::PayloadTooLarge(
                "shared text exceeds 1 MiB".to_string(),
            ));
        }
        let hash = sha256_hex(content.as_bytes());
        return Ok(SharedSource {
            owner_user_id: claims.sub.clone(),
            resource_type: "shared_text".to_string(),
            resource_id: request
                .resource_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            version: hash.clone(),
            content_hash: hash,
            size_bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
            mime_type: Some("text/plain".to_string()),
            metadata: json!({"content": content}),
        });
    }
    let resource_id = request
        .resource_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::ValidationError("resourceId is required".to_string()))?;
    match resource_type.as_str() {
        "upload" => {
            let row = sqlx::query::<sqlx::Sqlite>(
                "SELECT user_id, filename, media_type, CAST(size_bytes AS INTEGER) AS size_bytes,
                        CAST(updated_at AS TEXT) AS updated_at
                 FROM chat_file_workspace_files
                 WHERE tenant_id = ? AND file_id = ? AND status = 'indexed' LIMIT 1",
            )
            .bind(&claims.tenant_id)
            .bind(resource_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound("publishable upload".to_string()))?;
            let version = row.get::<String, _>("updated_at");
            Ok(SharedSource {
                owner_user_id: row.get("user_id"),
                resource_type,
                resource_id: resource_id.to_string(),
                content_hash: sha256_hex(format!("{resource_id}:{version}").as_bytes()),
                version,
                size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
                mime_type: Some(row.get("media_type")),
                metadata: json!({"filename": row.get::<String, _>("filename")}),
            })
        }
        "history" => {
            let row = sqlx::query::<sqlx::Sqlite>(
                "SELECT user_id, content_hash, content_kind,
                        CAST(char_count AS INTEGER) AS char_count,
                        CAST(created_at AS TEXT) AS created_at
                 FROM agent_context_archives
                 WHERE tenant_id = ? AND id = ? LIMIT 1",
            )
            .bind(&claims.tenant_id)
            .bind(resource_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound("publishable history archive".to_string()))?;
            let hash = row.get::<String, _>("content_hash");
            Ok(SharedSource {
                owner_user_id: row.get("user_id"),
                resource_type,
                resource_id: resource_id.to_string(),
                version: hash.clone(),
                content_hash: hash,
                size_bytes: row.try_get::<u64, _>("char_count").unwrap_or(0),
                mime_type: Some("text/markdown".to_string()),
                metadata: json!({
                    "contentKind": row.get::<String, _>("content_kind"),
                    "createdAt": row.get::<String, _>("created_at"),
                }),
            })
        }
        "generated" => {
            let row = sqlx::query::<sqlx::Sqlite>(
                "SELECT user_id, artifact_type, CAST(payload_json AS TEXT) AS content,
                        CAST(created_at AS TEXT) AS created_at
                 FROM chat_turn_artifacts
                 WHERE tenant_id = ? AND id = ? LIMIT 1",
            )
            .bind(&claims.tenant_id)
            .bind(resource_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound("publishable generated artifact".to_string()))?;
            let content = row.get::<String, _>("content");
            let hash = sha256_hex(content.as_bytes());
            Ok(SharedSource {
                owner_user_id: row.get("user_id"),
                resource_type,
                resource_id: resource_id.to_string(),
                version: hash.clone(),
                content_hash: hash,
                size_bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
                mime_type: Some("application/json".to_string()),
                metadata: json!({
                    "artifactType": row.get::<String, _>("artifact_type"),
                    "createdAt": row.get::<String, _>("created_at"),
                }),
            })
        }
        "sql_knowledge" => {
            let row = sqlx::query::<sqlx::Sqlite>(
                "SELECT COALESCE(p.user_id, ds.user_id) AS owner_user_id, f.pack_id,
                        f.filename, f.media_type, CAST(f.size_bytes AS INTEGER) AS size_bytes,
                        f.content_hash
                 FROM nl2sql_reference_files f
                 JOIN nl2sql_reference_packs p
                   ON p.tenant_id = f.tenant_id AND p.id = f.pack_id
                 LEFT JOIN data_sources ds
                   ON ds.tenant_id = f.tenant_id AND ds.id = f.datasource_id
                 WHERE f.tenant_id = ? AND f.id = ? AND p.enabled = 1
                   AND f.status = 'indexed' LIMIT 1",
            )
            .bind(&claims.tenant_id)
            .bind(resource_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound("publishable SQL knowledge file".to_string()))?;
            let hash = row.get::<String, _>("content_hash");
            Ok(SharedSource {
                owner_user_id: row
                    .get::<Option<String>, _>("owner_user_id")
                    .unwrap_or_else(|| claims.sub.clone()),
                resource_type,
                resource_id: resource_id.to_string(),
                version: hash.clone(),
                content_hash: hash,
                size_bytes: row.try_get::<u64, _>("size_bytes").unwrap_or(0),
                mime_type: row.get("media_type"),
                metadata: json!({
                    "packId": row.get::<String, _>("pack_id"),
                    "filename": row.get::<String, _>("filename"),
                }),
            })
        }
        "project" => resolve_shared_project_source(state, claims, resource_id, request).await,
        _ => Err(AppError::ValidationError(
            "resourceType must be upload, history, generated, sql_knowledge, project, or shared_text"
                .to_string(),
        )),
    }
}

async fn resolve_shared_project_source(
    state: &AppState,
    claims: &Claims,
    project_id: &str,
    request: &PublishSharedResourceRequest,
) -> AppResult<SharedSource> {
    let source_path = normalize_project_source_path(
        request
            .source_path
            .as_deref()
            .ok_or_else(|| AppError::ValidationError("sourcePath is required".to_string()))?,
    )?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT user_id, clone_path FROM gitlab_projects
         WHERE tenant_id = ? AND id = ? AND is_cloned = 1 AND clone_path IS NOT NULL LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("publishable project".to_string()))?;
    let owner_user_id = row.get::<String, _>("user_id");
    let managed_root = state
        .data_dir
        .join(&claims.tenant_id)
        .join(&owner_user_id)
        .join("workspace")
        .canonicalize()
        .map_err(|_| AppError::NotFound("publishable project".to_string()))?;
    let clone_root = PathBuf::from(row.get::<String, _>("clone_path"))
        .canonicalize()
        .map_err(|_| AppError::NotFound("publishable project".to_string()))?;
    if !clone_root.starts_with(&managed_root) {
        return Err(AppError::ValidationError(
            "project clone is outside the managed user workspace".to_string(),
        ));
    }
    let file = clone_root
        .join(&source_path)
        .canonicalize()
        .map_err(|_| AppError::NotFound("publishable project file".to_string()))?;
    if !file.starts_with(&clone_root) || !file.is_file() {
        return Err(AppError::NotFound("publishable project file".to_string()));
    }
    let bytes = std::fs::read(&file)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(AppError::PayloadTooLarge(
            "project file exceeds 2 MiB".to_string(),
        ));
    }
    std::str::from_utf8(&bytes)
        .map_err(|_| AppError::ValidationError("project file must be UTF-8 text".to_string()))?;
    let hash = sha256_hex(&bytes);
    Ok(SharedSource {
        owner_user_id,
        resource_type: "project".to_string(),
        resource_id: project_id.to_string(),
        version: hash.clone(),
        content_hash: hash,
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        mime_type: Some("text/plain".to_string()),
        metadata: json!({"sourcePath": source_path}),
    })
}

fn normalize_project_source_path(raw: &str) -> AppResult<String> {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    if raw.is_empty()
        || raw.chars().any(char::is_control)
        || raw.contains('\\')
        || raw.starts_with('/')
        || ["%2e", "%2f", "%5c", "%00"]
            .iter()
            .any(|encoded| lower.contains(encoded))
    {
        return Err(AppError::ValidationError(
            "invalid project sourcePath".to_string(),
        ));
    }
    let path = FsPath::new(raw);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::ValidationError(
            "project sourcePath traversal is not allowed".to_string(),
        ));
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

async fn validate_grantees(
    state: &AppState,
    tenant_id: &str,
    raw_user_ids: &[String],
) -> AppResult<Vec<String>> {
    let user_ids = raw_user_ids
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if user_ids.len() > 500 {
        return Err(AppError::ValidationError(
            "a publication can grant at most 500 users".to_string(),
        ));
    }
    for user_id in &user_ids {
        let exists = sqlx::query_scalar::<sqlx::Sqlite, i64>(
            "SELECT COUNT(*) FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
        if exists != 1 {
            return Err(AppError::ValidationError(
                "one or more grantees are not active users in this tenant".to_string(),
            ));
        }
    }
    Ok(user_ids)
}

async fn ensure_tenant_shared_workspace(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claims: &Claims,
    workspace_id: &str,
) -> AppResult<()> {
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_workspaces
            (id, tenant_id, owner_user_id, workspace_type, visibility, enabled, acl_version)
         VALUES (?, ?, ?, 'tenant_shared', 'tenant_shared', 1, 1)
         ON CONFLICT DO UPDATE SET enabled = 1",
    )
    .bind(workspace_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn upsert_shared_grant(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    workspace_id: &str,
    entry_id: &str,
    resource_id: &str,
    user_id: &str,
) -> AppResult<()> {
    let digest = sha256_hex(format!("{tenant_id}:{workspace_id}:{entry_id}:{user_id}").as_bytes());
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_workspace_grants
            (id, tenant_id, workspace_id, entry_id, resource_id, grantee_user_id,
             permission, enabled, revoked_at)
         VALUES (?, ?, ?, ?, ?, ?, 'read', 1, NULL)
         ON CONFLICT DO UPDATE SET resource_id = excluded.resource_id, permission = 'read',
             enabled = 1, revoked_at = NULL, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(format!("grant-{}", &digest[..24]))
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(entry_id)
    .bind(resource_id)
    .bind(user_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn bump_workspace_acl(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    workspace_id: &str,
) -> AppResult<()> {
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_workspaces SET acl_version = acl_version + 1,
             updated_at = CURRENT_TIMESTAMP WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn bump_personal_workspace_acl(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_workspaces SET acl_version = acl_version + 1,
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND owner_user_id = ? AND workspace_type = 'personal'",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn tenant_shared_workspace_id(tenant_id: &str) -> String {
    let digest = sha256_hex(format!("tenant-shared:{tenant_id}").as_bytes());
    format!("ws-shared-{}", &digest[..20])
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Queued,
    RunningModel,
    WaitingSubagent,
    ResumingModel,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

pub fn valid_turn_transition(from: TurnState, to: TurnState) -> bool {
    use TurnState::*;
    matches!(
        (from, to),
        (Queued, RunningModel)
            | (Queued, Failed)
            | (Queued, Cancelled)
            | (RunningModel, WaitingSubagent)
            | (RunningModel, Verifying)
            | (RunningModel, Failed)
            | (RunningModel, Cancelled)
            | (WaitingSubagent, ResumingModel)
            | (WaitingSubagent, Failed)
            | (WaitingSubagent, Cancelled)
            | (ResumingModel, WaitingSubagent)
            | (ResumingModel, Verifying)
            | (ResumingModel, Failed)
            | (ResumingModel, Cancelled)
            | (Verifying, RunningModel)
            | (Verifying, ResumingModel)
            | (Verifying, WaitingSubagent)
            | (Verifying, Completed)
            | (Verifying, Failed)
            | (Verifying, Cancelled)
    )
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionChecklist {
    pub risk_level: String,
    #[serde(default)]
    pub required_evidence: Vec<String>,
    pub claims: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub tools_used: Vec<String>,
    pub checks_performed: Vec<String>,
    pub remaining_uncertainty: Vec<String>,
    pub final_answer: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionDecision {
    pub allowed: bool,
    pub verification_required: Vec<String>,
}

pub fn evaluate_completion(checklist: &CompletionChecklist) -> CompletionDecision {
    let tools = checklist
        .tools_used
        .iter()
        .map(|v| v.as_str())
        .collect::<BTreeSet<_>>();
    let checks = checklist
        .checks_performed
        .iter()
        .map(|v| v.as_str())
        .collect::<BTreeSet<_>>();
    let required = checklist
        .required_evidence
        .iter()
        .map(|value| value.as_str())
        .collect::<BTreeSet<_>>();
    let mut missing = Vec::new();
    if required.contains("web")
        && !checks.contains("sources_cited")
        && !checks.contains("provider_native_search_verified")
    {
        missing.push("required web research has no server-verified cited source".into());
    }
    if required.contains("workspace") {
        if !checks.contains("workspace_evidence_read") {
            missing.push("required workspace evidence was not searched and read".into());
        }
    }
    if required.contains("code_change") {
        if !checks.contains("code_modified") {
            missing.push("requested code change was not applied".into());
        }
        if !checks.contains("diff_reviewed") {
            missing.push("requested code change has not been diff-reviewed".into());
        }
        if !checks.contains("tests_run") && !checks.contains("tests_unavailable") {
            missing.push("requested code change has not been verified".into());
        }
    }
    if required.contains("data_execution") {
        for (check, message) in [
            ("sql_recorded", "required data query did not record SQL"),
            (
                "schema_checked",
                "required data query did not validate schema",
            ),
            (
                "execution_checked",
                "required data query did not complete successfully",
            ),
        ] {
            if !checks.contains(check) {
                missing.push(message.to_string());
            }
        }
    }
    if required.contains("deep_research") && !checks.contains("research_claims_sourced") {
        missing.push("required deep research did not complete with sources".into());
    }
    if required.contains("super_adversarial") && !checks.contains("adversarial_completed") {
        missing.push("required super adversarial analysis did not complete".into());
    }
    // File writes are not inherently code changes: reports and generated SQL
    // are legitimate deliverables. The parent server classifies real source
    // edits and adds `code_change` to required evidence before reaching here.
    // Claims/evidenceRefs are useful telemetry, but they are model-authored
    // metadata and must not become a second, stricter truth source. Required
    // workspace/web evidence is checked above from the server tool ledger by
    // the parent orchestrator. Incidental reads/searches do not invalidate an
    // otherwise complete answer merely because the model omitted bookkeeping.
    if tools.iter().any(|t| t.starts_with("nl2sql")) {
        for required in ["sql_recorded", "schema_checked", "execution_checked"] {
            if !checks.contains(required) {
                missing.push(format!("SQL completion is missing {required}"));
            }
        }
    }
    if tools.contains("data_attribution") && !checks.contains("attribution_evidence_linked") {
        missing.push("attribution causes lack successful evidence steps".into());
    }
    if tools.contains("deep_research") && !checks.contains("research_claims_sourced") {
        missing.push("research claims lack sources or inference labels".into());
    }
    missing.sort();
    missing.dedup();
    CompletionDecision {
        allowed: missing.is_empty(),
        verification_required: missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn shared_publication_path_is_canonical_and_rooted() {
        assert_eq!(
            normalize_shared_publish_path("/shared/finance/roi.sql").expect("valid shared path"),
            "shared/finance/roi.sql"
        );
        for path in [
            "../secret",
            "/shared/../secret",
            "/shared/%2e%2e/secret",
            "C:secret",
            "/shared/a\\b",
        ] {
            assert!(normalize_shared_publish_path(path).is_err(), "{path}");
        }
    }
    #[test]
    fn terminal_states_never_transition() {
        for state in [
            TurnState::Completed,
            TurnState::Failed,
            TurnState::Cancelled,
        ] {
            assert!(!valid_turn_transition(state, TurnState::RunningModel));
        }
    }
    #[test]
    fn simple_completion_has_zero_extra_requirements() {
        assert!(
            evaluate_completion(&CompletionChecklist {
                final_answer: "ok".into(),
                ..Default::default()
            })
            .allowed
        );
    }
    #[test]
    fn code_and_sql_require_real_checks() {
        let d = evaluate_completion(&CompletionChecklist {
            tools_used: vec!["edit_file".into(), "nl2sql_analyze".into()],
            required_evidence: vec!["code_change".into(), "data_execution".into()],
            ..Default::default()
        });
        assert!(d.verification_required.len() >= 5);
    }

    #[test]
    fn declared_evidence_requirement_rejects_a_tool_free_completion() {
        for requirement in [
            "web",
            "workspace",
            "code_change",
            "data_execution",
            "deep_research",
        ] {
            let decision = evaluate_completion(&CompletionChecklist {
                required_evidence: vec![requirement.to_string()],
                final_answer: "unsupported answer".to_string(),
                ..Default::default()
            });
            assert!(!decision.allowed, "{requirement}");
            assert!(!decision.verification_required.is_empty(), "{requirement}");
        }
    }

    #[test]
    fn incidental_live_lookup_does_not_create_a_hidden_metadata_gate() {
        for tool in [
            "web_search",
            "WebSearch",
            "provider_native_web_search",
            "mcp__browser__search",
        ] {
            let decision = evaluate_completion(&CompletionChecklist {
                tools_used: vec![tool.to_string()],
                claims: vec!["a current fact".to_string()],
                ..Default::default()
            });
            assert!(decision.allowed, "{tool}");
        }
    }

    #[test]
    fn generated_report_write_does_not_create_a_code_verification_gate() {
        let decision = evaluate_completion(&CompletionChecklist {
            tools_used: vec!["write_file".to_string()],
            final_answer: "报告已生成".to_string(),
            ..Default::default()
        });
        assert!(decision.allowed);
    }

    #[test]
    fn verified_web_answer_does_not_require_model_authored_claims_ledger() {
        let decision = evaluate_completion(&CompletionChecklist {
            risk_level: "high".to_string(),
            required_evidence: vec!["web".to_string()],
            checks_performed: vec!["sources_cited".to_string()],
            final_answer: "有来源支撑的结论".to_string(),
            ..Default::default()
        });
        assert!(decision.allowed);
    }

    #[test]
    fn provider_native_search_can_ground_live_answer_without_clickable_citation() {
        let decision = evaluate_completion(&CompletionChecklist {
            risk_level: "high".to_string(),
            required_evidence: vec!["web".to_string()],
            checks_performed: vec!["provider_native_search_verified".to_string()],
            final_answer: "原生联网已返回天气结果".to_string(),
            ..Default::default()
        });
        assert!(decision.allowed);
    }

    #[test]
    fn verified_workspace_read_does_not_require_duplicate_hidden_path_metadata() {
        let decision = evaluate_completion(&CompletionChecklist {
            risk_level: "high".to_string(),
            required_evidence: vec!["workspace".to_string()],
            checks_performed: vec!["workspace_evidence_read".to_string()],
            final_answer: "结论来自 /sql-knowledge/team/roi.sql".to_string(),
            ..Default::default()
        });
        assert!(decision.allowed);
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        // Feature: unified-agent-workspace, Property: required evidence cannot be satisfied by a tool-free answer.
        fn arbitrary_required_evidence_never_passes_without_server_checks(
            requirement in prop_oneof![
                Just("web"),
                Just("workspace"),
                Just("code_change"),
                Just("data_execution"),
                Just("deep_research"),
            ]
        ) {
            let decision = evaluate_completion(&CompletionChecklist {
                required_evidence: vec![requirement.to_string()],
                final_answer: "claim".to_string(),
                ..Default::default()
            });
            prop_assert!(!decision.allowed);
        }

        // Feature: unified-agent-workspace, Property: shared publication paths cannot escape /shared.
        #[test]
        fn arbitrary_traversal_segments_are_rejected(prefix in "[a-z]{0,12}", suffix in "[a-z]{0,12}") {
            let path = format!("/shared/{}/../{}", prefix, suffix);
            prop_assert!(normalize_shared_publish_path(&path).is_err());
        }
    }
}
