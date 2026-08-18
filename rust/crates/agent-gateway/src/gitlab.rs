//! Gitlab project management — clone, sync, and manage per-user Git projects.
//!
//! ## Design
//!
//! Each user can add multiple Gitlab projects. When they start an agent session,
//! they can choose which project(s) to work on. Projects are cloned into the
//! user's workspace: `$workspace/{project_name}/`.
//!
//! ## Security
//!
//! - Token validation: user's Gitlab token is verified before storage.
//! - Path isolation: all git operations happen inside the user's workspace.
//! - Token injection: the token is never written to disk in plaintext; it's
//!   only injected into `GIT_ASKPASS` or the URL at clone time.
//!
//! ## Future work
//!
//! - Branch management
//! - Pull/push with user-level credentials
//! - Auto-sync on a schedule

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::{GatewayError, Result};

/// A Gitlab project registered by a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitlabProject {
    pub id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub name: String,
    pub url: String,
    pub branch: String,
    pub gitlab_token: Option<String>, // encrypted, nullable
    pub description: Option<String>,
    pub clone_path: Option<String>,
    pub is_cloned: bool,
    pub last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct AddProjectRequest {
    pub name: String,
    pub url: String,
    pub branch: Option<String>,
    pub gitlab_token: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProjectRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub branch: Option<String>,
    pub gitlab_token: Option<String>,
    pub description: Option<String>,
}

struct GitAuth {
    askpass_path: PathBuf,
    username: String,
    password: String,
}

pub struct GitlabProjectManager {
    db: SqlitePool,
    data_dir: PathBuf,
    http_client: reqwest::Client,
}

fn parse_sqlite_utc_datetime(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|| {
            [
                "%Y-%m-%d %H:%M:%S%.f%:z",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S",
            ]
            .into_iter()
            .find_map(|format| {
                if format.ends_with("%:z") {
                    chrono::DateTime::parse_from_str(value, format)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                } else {
                    chrono::NaiveDateTime::parse_from_str(value, format)
                        .ok()
                        .map(|dt| chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc))
                }
            })
        })
}

impl GitlabProjectManager {
    #[must_use]
    pub fn new(db: SqlitePool, data_dir: PathBuf) -> Self {
        Self {
            db,
            data_dir,
            http_client: reqwest::Client::new(),
        }
    }

    /// Add a new Gitlab project for a user.
    pub async fn add_project(
        &self,
        tenant_id: &str,
        user_id: &str,
        req: AddProjectRequest,
    ) -> Result<GitlabProject> {
        // 1. Validate the URL
        if req.url.is_empty() {
            return Err(GatewayError::Validation("url is required".to_string()));
        }

        // 2. Optionally validate the token
        if let Some(ref token) = req.gitlab_token {
            self.validate_token(&req.url, token).await?;
        }

        // 3. Check for duplicates
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM gitlab_projects WHERE tenant_id = ? AND user_id = ? AND url = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&req.url)
        .fetch_optional(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        if existing.is_some() {
            return Err(GatewayError::ProjectAlreadyExists(req.url.clone()));
        }

        // 4. Insert into DB
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let branch = req.branch.unwrap_or_else(|| "main".to_string());

        // Encrypt the token before storing
        let stored_token = req
            .gitlab_token
            .map(|token| encrypt_token(&token, tenant_id, &id));

        sqlx::query(
            r"
            INSERT INTO gitlab_projects
                (id, tenant_id, user_id, name, url, branch, gitlab_token, description, is_cloned, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?)
            ",
        )
        .bind(&id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(&req.name)
        .bind(&req.url)
        .bind(&branch)
        .bind(&stored_token)
        .bind(&req.description)
        .bind(now)
        .execute(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        tracing::info!(user_id, project_id = %id, name = %req.name, "gitlab project added");

        Ok(GitlabProject {
            id,
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            name: req.name,
            url: req.url,
            branch,
            gitlab_token: None, // Never expose token in responses
            description: req.description,
            clone_path: None,
            is_cloned: false,
            last_sync_at: None,
            created_at: now,
        })
    }

    /// List all projects for a user.
    pub async fn list_projects(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Vec<GitlabProject>> {
        type ProjectRow = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
            Option<String>,
        );
        let rows: Vec<ProjectRow> = sqlx::query_as(
            r"
            SELECT id, name, url, branch, description, gitlab_token, clone_path, CAST(last_sync_at AS TEXT), is_cloned, CAST(created_at AS TEXT)
            FROM gitlab_projects
            WHERE tenant_id = ? AND user_id = ?
            ORDER BY created_at DESC
            ",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_all(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    name,
                    url,
                    branch,
                    description,
                    _token,
                    clone_path,
                    last_sync_at,
                    is_cloned,
                    created_at,
                )| {
                    GitlabProject {
                        id,
                        tenant_id: tenant_id.to_string(),
                        user_id: user_id.to_string(),
                        name,
                        url,
                        branch,
                        gitlab_token: None,
                        description,
                        clone_path,
                        is_cloned,
                        last_sync_at: last_sync_at.as_deref().and_then(parse_sqlite_utc_datetime),
                        created_at: created_at
                            .and_then(|s| {
                                chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()
                            })
                            .map_or_else(chrono::Utc::now, |dt| {
                                chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)
                            }),
                    }
                },
            )
            .collect())
    }

    /// Get a single project.
    pub async fn get_project(
        &self,
        tenant_id: &str,
        user_id: &str,
        project_id: &str,
    ) -> Result<GitlabProject> {
        type ProjectRow = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
            Option<String>,
        );
        let row: Option<ProjectRow> = sqlx::query_as(
            r"
            SELECT id, name, url, branch, description, gitlab_token, clone_path, CAST(last_sync_at AS TEXT), is_cloned, CAST(created_at AS TEXT)
            FROM gitlab_projects
            WHERE id = ? AND tenant_id = ? AND user_id = ?
            ",
        )
        .bind(project_id)
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        let (
            id,
            name,
            url,
            branch,
            description,
            _token,
            clone_path,
            last_sync_at,
            is_cloned,
            created_at,
        ) = row.ok_or_else(|| GatewayError::ProjectNotFound(project_id.to_string()))?;

        Ok(GitlabProject {
            id,
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            name,
            url,
            branch,
            gitlab_token: None,
            description,
            clone_path,
            is_cloned,
            last_sync_at: last_sync_at.as_deref().and_then(parse_sqlite_utc_datetime),
            created_at: created_at
                .and_then(|s| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok())
                .map_or_else(chrono::Utc::now, |dt| {
                    chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)
                }),
        })
    }

    /// Update mutable project metadata. Empty token input intentionally means
    /// "keep the existing token" so normal edits never erase private repo access.
    pub async fn update_project(
        &self,
        tenant_id: &str,
        user_id: &str,
        project_id: &str,
        req: UpdateProjectRequest,
    ) -> Result<GitlabProject> {
        type ProjectRow = (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        );
        let row: ProjectRow = sqlx::query_as(
            r"
            SELECT name, url, branch, description, gitlab_token, clone_path, is_cloned
            FROM gitlab_projects
            WHERE id = ? AND tenant_id = ? AND user_id = ?
            ",
        )
        .bind(project_id)
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        .map_err(GatewayError::Database)?
        .ok_or_else(|| GatewayError::ProjectNotFound(project_id.to_string()))?;

        let (current_name, current_url, current_branch, current_desc, current_token, _, _) = row;
        let name = req
            .name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map_or(current_name, ToOwned::to_owned);
        let url = req
            .url
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map_or(current_url, ToOwned::to_owned);
        let branch = req
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map_or(current_branch, ToOwned::to_owned);
        let description = req
            .description
            .as_ref()
            .map(|value| value.trim().to_string())
            .map(|value| if value.is_empty() { None } else { Some(value) })
            .unwrap_or(current_desc);
        let token = req
            .gitlab_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|token| encrypt_token(token, tenant_id, project_id))
            .or(current_token);

        let duplicate: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM gitlab_projects WHERE tenant_id = ? AND user_id = ? AND url = ? AND id <> ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&url)
        .bind(project_id)
        .fetch_optional(&self.db)
        .await
        .map_err(GatewayError::Database)?;
        if duplicate.is_some() {
            return Err(GatewayError::ProjectAlreadyExists(url));
        }

        let result = sqlx::query(
            r"
            UPDATE gitlab_projects
            SET name = ?, url = ?, branch = ?, gitlab_token = ?, description = ?
            WHERE id = ? AND tenant_id = ? AND user_id = ?
            ",
        )
        .bind(&name)
        .bind(&url)
        .bind(&branch)
        .bind(&token)
        .bind(&description)
        .bind(project_id)
        .bind(tenant_id)
        .bind(user_id)
        .execute(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        if result.rows_affected() == 0 {
            return Err(GatewayError::ProjectNotFound(project_id.to_string()));
        }

        tracing::info!(user_id, project_id = %project_id, branch = %branch, "gitlab project updated");
        self.get_project(tenant_id, user_id, project_id).await
    }

    /// Delete a project.
    pub async fn delete_project(
        &self,
        tenant_id: &str,
        user_id: &str,
        project_id: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            "DELETE FROM gitlab_projects WHERE id = ? AND tenant_id = ? AND user_id = ?",
        )
        .bind(project_id)
        .bind(tenant_id)
        .bind(user_id)
        .execute(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        if result.rows_affected() == 0 {
            return Err(GatewayError::ProjectNotFound(project_id.to_string()));
        }

        tracing::info!(user_id, project_id = %project_id, "gitlab project deleted");
        Ok(())
    }

    /// Clone (or update) a project into the user's workspace.
    pub async fn sync_project(
        &self,
        tenant_id: &str,
        user_id: &str,
        project_id: &str,
    ) -> Result<PathBuf> {
        let project = self
            .get_project_for_git(tenant_id, user_id, project_id)
            .await?;

        let workspace = self
            .data_dir
            .join(tenant_id)
            .join(user_id)
            .join("workspace");

        let clone_dir = project
            .clone_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .unwrap_or_else(|| workspace.join(sanitize_dir_name(&project.name)));

        // Ensure workspace exists
        tokio::fs::create_dir_all(&workspace)
            .await
            .map_err(GatewayError::Io)?;

        let auth = self
            .prepare_git_auth(&workspace, &project.url, project.gitlab_token.as_deref())
            .await?;

        if clone_dir.exists() {
            // Already cloned: keep origin current, switch to the selected branch, then pull.
            tracing::info!(project_id = %project_id, path = %clone_dir.display(), "syncing existing clone");
            self.run_git(
                Some(&clone_dir),
                &["remote", "set-url", "origin", &project.url],
                None,
            )
            .await?;
            self.run_git(
                Some(&clone_dir),
                &["fetch", "origin", "--prune"],
                auth.as_ref(),
            )
            .await?;

            let checkout = self
                .run_git(Some(&clone_dir), &["checkout", &project.branch], None)
                .await;
            if checkout.is_err() {
                self.run_git(
                    Some(&clone_dir),
                    &[
                        "checkout",
                        "-B",
                        &project.branch,
                        &format!("origin/{}", project.branch),
                    ],
                    None,
                )
                .await?;
            }
            self.run_git(
                Some(&clone_dir),
                &["pull", "--ff-only", "origin", &project.branch],
                auth.as_ref(),
            )
            .await?;
        } else {
            // Fresh clone
            tracing::info!(project_id = %project_id, url = %project.url, "cloning project");
            self.run_git(
                None,
                &[
                    "clone",
                    "--branch",
                    &project.branch,
                    "--single-branch",
                    &project.url,
                    clone_dir.to_str().unwrap(),
                ],
                auth.as_ref(),
            )
            .await?;
        }

        // Update DB
        let now = chrono::Utc::now();
        sqlx::query(
            "UPDATE gitlab_projects SET is_cloned = 1, clone_path = ?, last_sync_at = ? WHERE id = ?",
        )
        .bind(clone_dir.to_string_lossy().as_ref())
        .bind(now)
        .bind(project_id)
        .execute(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        Ok(clone_dir)
    }

    /// List remote branches for a project, falling back to local refs when the
    /// remote cannot be reached. This avoids the `--single-branch` clone trap
    /// where `git branch --all` only knows about the originally cloned branch.
    pub async fn list_branches(
        &self,
        tenant_id: &str,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<String>> {
        let project = self
            .get_project_for_git(tenant_id, user_id, project_id)
            .await?;
        let workspace = self
            .data_dir
            .join(tenant_id)
            .join(user_id)
            .join("workspace");
        tokio::fs::create_dir_all(&workspace)
            .await
            .map_err(GatewayError::Io)?;
        let auth = self
            .prepare_git_auth(&workspace, &project.url, project.gitlab_token.as_deref())
            .await?;

        let mut branches = BTreeSet::new();
        if let Ok(output) = self
            .run_git(None, &["ls-remote", "--heads", &project.url], auth.as_ref())
            .await
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if let Some((_, reference)) = line.split_once("refs/heads/") {
                    let branch = reference.trim();
                    if !branch.is_empty() {
                        branches.insert(branch.to_string());
                    }
                }
            }
        }

        if let Some(clone_path) = project.clone_path.as_deref().map(PathBuf::from) {
            if clone_path.exists() {
                if let Ok(output) = self
                    .run_git(
                        Some(&clone_path),
                        &["branch", "--all", "--format", "%(refname:short)"],
                        None,
                    )
                    .await
                {
                    for line in String::from_utf8_lossy(&output.stdout).lines() {
                        if let Some(branch) = normalize_branch_ref(line.trim()) {
                            branches.insert(branch);
                        }
                    }
                }
            }
        }

        Ok(branches.into_iter().collect())
    }

    async fn get_project_for_git(
        &self,
        tenant_id: &str,
        user_id: &str,
        project_id: &str,
    ) -> Result<GitlabProject> {
        type ProjectRow = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
            Option<String>,
        );
        let row: Option<ProjectRow> = sqlx::query_as(
            r"
            SELECT id, name, url, branch, description, gitlab_token, clone_path, CAST(last_sync_at AS TEXT), is_cloned, CAST(created_at AS TEXT)
            FROM gitlab_projects
            WHERE id = ? AND tenant_id = ? AND user_id = ?
            ",
        )
        .bind(project_id)
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        .map_err(GatewayError::Database)?;

        let (
            id,
            name,
            url,
            branch,
            description,
            token,
            clone_path,
            last_sync_at,
            is_cloned,
            created_at,
        ) = row.ok_or_else(|| GatewayError::ProjectNotFound(project_id.to_string()))?;

        Ok(GitlabProject {
            id,
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            name,
            url,
            branch,
            gitlab_token: token.map(|value| decrypt_token(&value, tenant_id, project_id)),
            description,
            clone_path,
            is_cloned,
            last_sync_at: last_sync_at.as_deref().and_then(parse_sqlite_utc_datetime),
            created_at: created_at
                .and_then(|s| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok())
                .map_or_else(chrono::Utc::now, |dt| {
                    chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)
                }),
        })
    }

    async fn prepare_git_auth(
        &self,
        workspace: &Path,
        url: &str,
        token: Option<&str>,
    ) -> Result<Option<GitAuth>> {
        let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let askpass_path = workspace.join(".aos-git-askpass.sh");
        tokio::fs::write(
            &askpass_path,
            "#!/bin/sh\ncase \"$1\" in\n*Username*) printf '%s\\n' \"$AOS_GIT_USERNAME\" ;;\n*) printf '%s\\n' \"$AOS_GIT_PASSWORD\" ;;\nesac\n",
        )
        .await
        .map_err(GatewayError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&askpass_path, std::fs::Permissions::from_mode(0o700))
                .map_err(GatewayError::Io)?;
        }
        Ok(Some(GitAuth {
            askpass_path,
            username: git_username_for_url(url),
            password: token.to_string(),
        }))
    }

    async fn run_git(
        &self,
        current_dir: Option<&Path>,
        args: &[&str],
        auth: Option<&GitAuth>,
    ) -> Result<Output> {
        let mut command = tokio::process::Command::new("git");
        command.args(args).env("GIT_TERMINAL_PROMPT", "0");
        if let Some(path) = current_dir {
            command.current_dir(path);
        }
        if let Some(auth) = auth {
            command
                .env("GIT_ASKPASS", &auth.askpass_path)
                .env("AOS_GIT_USERNAME", &auth.username)
                .env("AOS_GIT_PASSWORD", &auth.password);
        }
        let output = command
            .output()
            .await
            .map_err(|e| GatewayError::GitError(format!("git {} failed: {e}", args.join(" "))))?;
        if !output.status.success() {
            return Err(GatewayError::GitError(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output)
    }

    /// Validate a Gitlab token by making a test API call.
    async fn validate_token(&self, base_url: &str, token: &str) -> Result<()> {
        let api_url: &str = if base_url.contains("gitlab.com") {
            "https://gitlab.com/api/v4/user"
        } else {
            &format!("{}/api/v4/user", base_url.trim_end_matches('/'))
        };

        let resp = self
            .http_client
            .get(api_url)
            .header("PRIVATE-TOKEN", token)
            .send()
            .await
            .map_err(|e| GatewayError::GitError(format!("failed to validate token: {e}")))?;

        if !resp.status().is_success() {
            return Err(GatewayError::InvalidToken);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Sanitize a project name for use as a directory name.
fn sanitize_dir_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn git_username_for_url(url: &str) -> String {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    if host.contains("github.com") {
        "x-access-token".to_string()
    } else if host.contains("bitbucket") {
        "x-token-auth".to_string()
    } else {
        "oauth2".to_string()
    }
}

fn normalize_branch_ref(value: &str) -> Option<String> {
    let normalized = value
        .strip_prefix("remotes/origin/")
        .or_else(|| value.strip_prefix("origin/"))
        .unwrap_or(value)
        .trim();
    if normalized.is_empty() || normalized == "HEAD" || normalized.contains("HEAD ->") {
        None
    } else {
        Some(normalized.to_string())
    }
}

const TOKEN_CIPHERTEXT_PREFIX: &str = "aosgcm:v1:";
const TOKEN_NONCE_LEN: usize = 12;

/// Encrypt a token with AES-256-GCM.
///
/// Existing deployments that stored the legacy XOR/base64 format remain
/// readable through `decrypt_token`, but all new writes use authenticated
/// encryption.
pub fn encrypt_token(token: &str, tenant_id: &str, project_id: &str) -> String {
    crate::crypto::encrypt_scoped(token, &repository_token_aad(tenant_id, project_id))
        .expect("repository token encryption failed")
}

pub fn decrypt_token(encrypted: &str, tenant_id: &str, project_id: &str) -> String {
    if encrypted.starts_with("aosenc:v2:") {
        return crate::crypto::decrypt_scoped(
            encrypted,
            &repository_token_aad(tenant_id, project_id),
        )
        .unwrap_or_default();
    }
    if encrypted.starts_with("aosenc:v1:") {
        return crate::crypto::decrypt(encrypted).unwrap_or_default();
    }
    if let Some(value) = encrypted.strip_prefix(TOKEN_CIPHERTEXT_PREFIX) {
        return decrypt_aes_gcm_token(value).unwrap_or_default();
    }
    decrypt_legacy_xor_token(encrypted)
}

fn repository_token_aad(tenant_id: &str, project_id: &str) -> String {
    crate::crypto::scoped_aad("repository.token", tenant_id, project_id)
}

fn token_encryption_key() -> [u8; 32] {
    let key = std::env::var("TOKEN_ENCRYPTION_KEY").unwrap_or_else(|_| {
        #[cfg(debug_assertions)]
        {
            static WARN_ONCE: std::sync::Once = std::sync::Once::new();
            WARN_ONCE.call_once(|| {
                tracing::warn!(
                    "TOKEN_ENCRYPTION_KEY is missing; using a development-only fallback key"
                );
            });
            "dev-key-32-chars-long!!".to_string()
        }
        #[cfg(not(debug_assertions))]
        {
            panic!("TOKEN_ENCRYPTION_KEY must be set in release builds")
        }
    });
    Sha256::digest(key.as_bytes()).into()
}

fn decrypt_aes_gcm_token(value: &str) -> Option<String> {
    let (nonce_raw, ciphertext_raw) = value.split_once(':')?;
    let nonce_bytes = BASE64_STANDARD.decode(nonce_raw).ok()?;
    if nonce_bytes.len() != TOKEN_NONCE_LEN {
        return None;
    }
    let ciphertext = BASE64_STANDARD.decode(ciphertext_raw).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&token_encryption_key()).ok()?;
    let decrypted = cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
        .ok()?;
    String::from_utf8(decrypted).ok()
}

fn decrypt_legacy_xor_token(encrypted: &str) -> String {
    let key = std::env::var("TOKEN_ENCRYPTION_KEY")
        .unwrap_or_else(|_| "dev-key-32-chars-long!!".to_string());
    let key_bytes = key.as_bytes();

    let data = BASE64_STANDARD.decode(encrypted).unwrap_or_default();

    data.iter()
        .enumerate()
        .map(|(i, b)| (b ^ key_bytes[i % key_bytes.len()]) as char)
        .collect()
}

#[cfg(test)]
mod datetime_tests {
    use super::parse_sqlite_utc_datetime;

    #[test]
    fn parses_sqlite_and_rfc3339_sync_timestamps() {
        for value in [
            "2026-08-11 03:03:28",
            "2026-08-11 03:03:28.538017",
            "2026-08-11T03:03:28.538017Z",
            "2026-08-11T11:03:28.538017+08:00",
        ] {
            assert!(
                parse_sqlite_utc_datetime(value).is_some(),
                "timestamp should parse: {value}"
            );
        }
    }
}
