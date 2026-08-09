use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    Json,
};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{Row, SqlitePool};
use tokio::time::{timeout, Duration};
use uuid::Uuid;

use crate::auth::Claims;
use crate::error::AppError;
use crate::state::AppState;

const SECRET_MASK: &str = "********";

mod pr_drafts;
#[cfg(test)]
use pr_drafts::{build_pr_markdown, build_provider_pr_payload, parse_repository_path};
pub(super) use pr_drafts::{publish_task_pr_draft, task_pr_draft};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdIntegrationRequest {
    provider: String,
    name: String,
    #[serde(default, alias = "config_json")]
    config_json: Option<Value>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdIntegrationUpdateRequest {
    provider: Option<String>,
    name: Option<String>,
    #[serde(default, alias = "config_json")]
    config_json: Option<Value>,
    enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdIntegrationDto {
    id: String,
    provider: String,
    name: String,
    config_json: Option<Value>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdIntegrationTestResult {
    ok: bool,
    provider: String,
    message: String,
    checked_url: Option<String>,
    status_code: Option<u16>,
    detail_json: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdPrDraftQuery {
    #[serde(alias = "integration_id")]
    integration_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdPrDraftPublishRequest {
    #[serde(alias = "integration_id")]
    integration_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RdPrDraftChangeDto {
    file_path: String,
    change_type: String,
    applied: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RdPrDraftTestDto {
    command: String,
    status: String,
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdPrDraftDto {
    task_id: String,
    title: String,
    description: String,
    branch_name: String,
    base_branch: String,
    repository_id: Option<String>,
    repository_name: Option<String>,
    repository_url: Option<String>,
    changes: Vec<RdPrDraftChangeDto>,
    tests: Vec<RdPrDraftTestDto>,
    provider_payloads: Vec<Value>,
    markdown: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdPrDraftPublishResult {
    ok: bool,
    provider: String,
    integration_id: String,
    remote_url: Option<String>,
    status_code: Option<u16>,
    message: String,
    response_json: Value,
    draft: RdPrDraftDto,
}

struct IntegrationRecord {
    id: String,
    provider: String,
    name: String,
    config_json: Option<Value>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

pub(super) async fn list_integrations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Vec<RdIntegrationDto>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, provider, name, config_json, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at \
         FROM rd_integrations WHERE tenant_id = ? ORDER BY enabled DESC, updated_at DESC",
    )
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.iter()
            .map(|row| record_to_dto(row_to_integration(row), true))
            .collect(),
    ))
}

pub(super) async fn create_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdIntegrationRequest>,
) -> Result<Json<RdIntegrationDto>, AppError> {
    let provider = normalize_provider(&req.provider)?;
    let name = require_non_empty(&req.name, "name")?;
    let config = req.config_json.unwrap_or_else(|| json!({}));
    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO rd_integrations (id, tenant_id, provider, name, config_json, enabled) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&provider)
    .bind(&name)
    .bind(&config)
    .bind(req.enabled.unwrap_or(true))
    .execute(&state.db)
    .await?;

    get_integration(&state.db, &claims.tenant_id, &id, true)
        .await
        .map(Json)
}

pub(super) async fn update_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RdIntegrationUpdateRequest>,
) -> Result<Json<RdIntegrationDto>, AppError> {
    let existing = get_integration_record(&state.db, &claims.tenant_id, &id).await?;
    let provider = match req.provider {
        Some(provider) => normalize_provider(&provider)?,
        None => existing.provider,
    };
    let name = match req.name {
        Some(name) => require_non_empty(&name, "name")?,
        None => existing.name,
    };
    let config = match req.config_json {
        Some(config) => preserve_masked_secrets(existing.config_json.as_ref(), config),
        None => existing.config_json.unwrap_or_else(|| json!({})),
    };
    let enabled = req.enabled.unwrap_or(existing.enabled);

    let result = sqlx::query(
        "UPDATE rd_integrations SET provider = ?, name = ?, config_json = ?, enabled = ? WHERE id = ? AND tenant_id = ?",
    )
    .bind(&provider)
    .bind(&name)
    .bind(&config)
    .bind(enabled)
    .bind(&id)
    .bind(&claims.tenant_id)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("rd integration not found".to_string()));
    }

    get_integration(&state.db, &claims.tenant_id, &id, true)
        .await
        .map(Json)
}

pub(super) async fn delete_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    let result = sqlx::query("DELETE FROM rd_integrations WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "deleted": result.rows_affected() > 0 })))
}

pub(super) async fn test_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<RdIntegrationTestResult>, AppError> {
    let integration = get_integration_record(&state.db, &claims.tenant_id, &id).await?;
    test_provider_connection(&integration).await.map(Json)
}

async fn get_integration(
    db: &SqlitePool,
    tenant_id: &str,
    id: &str,
    redact: bool,
) -> Result<RdIntegrationDto, AppError> {
    Ok(record_to_dto(
        get_integration_record(db, tenant_id, id).await?,
        redact,
    ))
}

async fn get_integration_record(
    db: &SqlitePool,
    tenant_id: &str,
    id: &str,
) -> Result<IntegrationRecord, AppError> {
    let row = sqlx::query(
        "SELECT id, provider, name, config_json, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at \
         FROM rd_integrations WHERE id = ? AND tenant_id = ?",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("rd integration not found".to_string()))?;
    Ok(row_to_integration(&row))
}

fn row_to_integration(row: &sqlx::sqlite::SqliteRow) -> IntegrationRecord {
    IntegrationRecord {
        id: row.get("id"),
        provider: row.get("provider"),
        name: row.get("name"),
        config_json: row.get("config_json"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn record_to_dto(record: IntegrationRecord, redact: bool) -> RdIntegrationDto {
    RdIntegrationDto {
        id: record.id,
        provider: record.provider,
        name: record.name,
        config_json: if redact {
            record.config_json.map(redact_secret_values)
        } else {
            record.config_json
        },
        enabled: record.enabled,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

fn normalize_provider(provider: &str) -> Result<String, AppError> {
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() {
        return Err(AppError::ValidationError(
            "provider is required".to_string(),
        ));
    }
    match provider.as_str() {
        "github" | "gitlab" | "jira" | "sentry" | "custom" => Ok(provider),
        _ => Err(AppError::ValidationError(format!(
            "unsupported rd integration provider: {provider}"
        ))),
    }
}

fn require_non_empty(value: &str, field: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::ValidationError(format!("{field} is required")));
    }
    Ok(value.to_string())
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("private_key")
        || key == "key"
        || key == "api_key"
}

fn redact_secret_values(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let redacted = if is_secret_key(&key) {
                        Value::String(SECRET_MASK.to_string())
                    } else {
                        redact_secret_values(value)
                    };
                    (key, redacted)
                })
                .collect(),
        ),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(redact_secret_values).collect())
        }
        other => other,
    }
}

fn preserve_masked_secrets(existing: Option<&Value>, incoming: Value) -> Value {
    match (existing, incoming) {
        (Some(Value::Object(existing)), Value::Object(incoming)) => {
            let mut merged = Map::new();
            for (key, value) in incoming {
                let preserved = if is_secret_key(&key)
                    && matches!(&value, Value::String(text) if text == SECRET_MASK)
                {
                    existing
                        .get(&key)
                        .cloned()
                        .unwrap_or(Value::String(String::new()))
                } else {
                    preserve_masked_secrets(existing.get(&key), value)
                };
                merged.insert(key, preserved);
            }
            Value::Object(merged)
        }
        (_, value) => value,
    }
}

async fn test_provider_connection(
    integration: &IntegrationRecord,
) -> Result<RdIntegrationTestResult, AppError> {
    let config = integration.config_json.as_ref().unwrap_or(&Value::Null);
    match integration.provider.as_str() {
        "github" => {
            test_bearer_endpoint(
                "github",
                config_string(config, &["apiBase", "api_base"])
                    .unwrap_or_else(|| "https://api.github.com".to_string()),
                "/user",
                config_string(config, &["token", "accessToken", "access_token"]),
                "Authorization",
                "Bearer",
            )
            .await
        }
        "gitlab" => {
            test_bearer_endpoint(
                "gitlab",
                config_string(config, &["apiBase", "api_base"])
                    .unwrap_or_else(|| "https://gitlab.com/api/v4".to_string()),
                "/user",
                config_string(
                    config,
                    &["token", "accessToken", "privateToken", "private_token"],
                ),
                "PRIVATE-TOKEN",
                "",
            )
            .await
        }
        "jira" => {
            test_jira_connection(
                config_string(config, &["baseUrl", "base_url", "url"]),
                config_string(config, &["email", "username", "user"]),
                config_string(config, &["apiToken", "api_token", "token"]),
            )
            .await
        }
        "sentry" => {
            test_bearer_endpoint(
                "sentry",
                config_string(config, &["apiBase", "api_base", "baseUrl", "base_url"])
                    .unwrap_or_else(|| "https://sentry.io/api/0".to_string()),
                "/organizations/",
                config_string(config, &["token", "authToken", "auth_token"]),
                "Authorization",
                "Bearer",
            )
            .await
        }
        _ => {
            let url = config_string(
                config,
                &["url", "webhookUrl", "webhook_url", "baseUrl", "base_url"],
            );
            if url.is_none() {
                Ok(RdIntegrationTestResult {
                    ok: false,
                    provider: integration.provider.clone(),
                    message: "Custom integration url/webhookUrl/baseUrl is required.".to_string(),
                    checked_url: None,
                    status_code: None,
                    detail_json: json!({ "error": "missing_url" }),
                })
            } else {
                Ok(config_only_result(
                    &integration.provider,
                    url,
                    "Custom integration config is valid. AOS will generate provider-neutral payloads until a native adapter is configured.",
                ))
            }
        }
    }
}

async fn test_bearer_endpoint(
    provider: &str,
    base_url: String,
    path: &str,
    token: Option<String>,
    header_name: &str,
    auth_prefix: &str,
) -> Result<RdIntegrationTestResult, AppError> {
    if token.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Ok(RdIntegrationTestResult {
            ok: false,
            provider: provider.to_string(),
            message: format!("{provider} token is required for connection test."),
            checked_url: Some(base_url),
            status_code: None,
            detail_json: json!({ "error": "missing_token" }),
        });
    }
    let url = join_url(&base_url, path);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("AOS-Code-Studio/1.0")
        .build()
        .map_err(|error| AppError::Internal(format!("failed to build http client: {error}")))?;
    let mut req = client.get(&url);
    if let Some(token) = token {
        let header_value = if auth_prefix.is_empty() {
            token
        } else {
            format!("{auth_prefix} {token}")
        };
        req = req.header(header_name, header_value);
    }
    let result = timeout(Duration::from_secs(10), req.send()).await;
    match result {
        Ok(Ok(resp)) => {
            let status = resp.status();
            Ok(RdIntegrationTestResult {
                ok: status.is_success(),
                provider: provider.to_string(),
                message: if status.is_success() {
                    "Remote authentication check passed.".to_string()
                } else {
                    format!("Remote check returned HTTP {status}.")
                },
                checked_url: Some(url),
                status_code: Some(status.as_u16()),
                detail_json: json!({ "status": status.as_u16() }),
            })
        }
        Ok(Err(error)) => Ok(RdIntegrationTestResult {
            ok: false,
            provider: provider.to_string(),
            message: format!("Remote check failed: {error}"),
            checked_url: Some(url),
            status_code: None,
            detail_json: json!({ "error": error.to_string() }),
        }),
        Err(_) => Ok(RdIntegrationTestResult {
            ok: false,
            provider: provider.to_string(),
            message: "Remote check timed out.".to_string(),
            checked_url: Some(url),
            status_code: None,
            detail_json: json!({ "error": "timeout" }),
        }),
    }
}

async fn test_jira_connection(
    base_url: Option<String>,
    email: Option<String>,
    api_token: Option<String>,
) -> Result<RdIntegrationTestResult, AppError> {
    let Some(base_url) = base_url.filter(|value| !value.trim().is_empty()) else {
        return Ok(RdIntegrationTestResult {
            ok: false,
            provider: "jira".to_string(),
            message: "Jira baseUrl is required for connection test.".to_string(),
            checked_url: None,
            status_code: None,
            detail_json: json!({ "error": "missing_base_url" }),
        });
    };
    let Some(email) = email.filter(|value| !value.trim().is_empty()) else {
        return Ok(RdIntegrationTestResult {
            ok: false,
            provider: "jira".to_string(),
            message: "Jira email is required for connection test.".to_string(),
            checked_url: Some(base_url),
            status_code: None,
            detail_json: json!({ "error": "missing_email" }),
        });
    };
    let Some(api_token) = api_token.filter(|value| !value.trim().is_empty()) else {
        return Ok(RdIntegrationTestResult {
            ok: false,
            provider: "jira".to_string(),
            message: "Jira apiToken is required for connection test.".to_string(),
            checked_url: Some(base_url),
            status_code: None,
            detail_json: json!({ "error": "missing_api_token" }),
        });
    };
    let url = join_url(&base_url, "/rest/api/3/myself");
    let auth = general_purpose::STANDARD.encode(format!("{email}:{api_token}"));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("AOS-Code-Studio/1.0")
        .build()
        .map_err(|error| AppError::Internal(format!("failed to build http client: {error}")))?;
    let result = timeout(
        Duration::from_secs(10),
        client
            .get(&url)
            .header("Authorization", format!("Basic {auth}"))
            .send(),
    )
    .await;
    match result {
        Ok(Ok(resp)) => {
            let status = resp.status();
            Ok(RdIntegrationTestResult {
                ok: status.is_success(),
                provider: "jira".to_string(),
                message: if status.is_success() {
                    "Remote authentication check passed.".to_string()
                } else {
                    format!("Remote check returned HTTP {status}.")
                },
                checked_url: Some(url),
                status_code: Some(status.as_u16()),
                detail_json: json!({ "status": status.as_u16() }),
            })
        }
        Ok(Err(error)) => Ok(RdIntegrationTestResult {
            ok: false,
            provider: "jira".to_string(),
            message: format!("Remote check failed: {error}"),
            checked_url: Some(url),
            status_code: None,
            detail_json: json!({ "error": error.to_string() }),
        }),
        Err(_) => Ok(RdIntegrationTestResult {
            ok: false,
            provider: "jira".to_string(),
            message: "Remote check timed out.".to_string(),
            checked_url: Some(url),
            status_code: None,
            detail_json: json!({ "error": "timeout" }),
        }),
    }
}

fn config_only_result(
    provider: &str,
    checked_url: Option<String>,
    message: &str,
) -> RdIntegrationTestResult {
    RdIntegrationTestResult {
        ok: true,
        provider: provider.to_string(),
        message: message.to_string(),
        checked_url,
        status_code: None,
        detail_json: json!({ "mode": "config_only" }),
    }
}

fn config_string(config: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        config
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn join_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integration(provider: &str, config_json: Value) -> IntegrationRecord {
        IntegrationRecord {
            id: "integration-1".to_string(),
            provider: provider.to_string(),
            name: "Test Integration".to_string(),
            config_json: Some(config_json),
            enabled: true,
            created_at: "2026-01-01 00:00:00".to_string(),
            updated_at: "2026-01-01 00:00:00".to_string(),
        }
    }

    #[test]
    fn redacts_and_preserves_masked_secret_values() {
        let original = json!({
            "url": "https://example.com",
            "token": "real-token",
            "nested": {
                "apiToken": "nested-token",
                "public": "visible"
            }
        });

        let redacted = redact_secret_values(original.clone());
        assert_eq!(redacted["token"], SECRET_MASK);
        assert_eq!(redacted["nested"]["apiToken"], SECRET_MASK);
        assert_eq!(redacted["nested"]["public"], "visible");

        let incoming = json!({
            "url": "https://changed.example.com",
            "token": SECRET_MASK,
            "nested": {
                "apiToken": SECRET_MASK,
                "public": "changed"
            }
        });
        let preserved = preserve_masked_secrets(Some(&original), incoming);

        assert_eq!(preserved["url"], "https://changed.example.com");
        assert_eq!(preserved["token"], "real-token");
        assert_eq!(preserved["nested"]["apiToken"], "nested-token");
        assert_eq!(preserved["nested"]["public"], "changed");
    }

    #[tokio::test]
    async fn provider_tests_fail_fast_when_required_credentials_are_missing() {
        let github = integration("github", json!({ "apiBase": "https://api.github.com" }));
        let github_result = test_provider_connection(&github).await.unwrap();
        assert!(!github_result.ok);
        assert_eq!(github_result.detail_json["error"], "missing_token");

        let custom = integration("custom", json!({ "token": "optional" }));
        let custom_result = test_provider_connection(&custom).await.unwrap();
        assert!(!custom_result.ok);
        assert_eq!(custom_result.detail_json["error"], "missing_url");

        let jira = integration("jira", json!({ "email": "dev@example.com" }));
        let jira_result = test_provider_connection(&jira).await.unwrap();
        assert!(!jira_result.ok);
        assert_eq!(jira_result.detail_json["error"], "missing_base_url");
    }

    #[test]
    fn parses_common_repository_clone_urls() {
        assert_eq!(
            parse_repository_path("git@github.com:owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            parse_repository_path("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            parse_repository_path("https://gitlab.com/group/subgroup/project.git?ref=main"),
            Some("group/subgroup/project".to_string())
        );
    }

    #[test]
    fn github_pr_payload_requires_repository_and_uses_complete_endpoint() {
        let missing = build_provider_pr_payload(
            &integration("github", json!({ "token": "ghp_xxx" })),
            None,
            "Fix bug",
            "Details",
            "aos/fix-bug",
            "main",
        );
        assert_eq!(missing["ok"], false);
        assert_eq!(missing["error"]["code"], "missing_repository");

        let payload = build_provider_pr_payload(
            &integration(
                "github",
                json!({
                    "apiBase": "https://github.example.com/api/v3",
                    "token": "ghp_xxx",
                    "repository": "owner/repo"
                }),
            ),
            None,
            "Fix bug",
            "Details",
            "aos/fix-bug",
            "main",
        );
        assert_eq!(payload["provider"], "github");
        assert_eq!(
            payload["url"],
            "https://github.example.com/api/v3/repos/owner/repo/pulls"
        );
        assert_eq!(payload["body"]["draft"], true);
    }

    #[test]
    fn gitlab_pr_payload_requires_project_and_encodes_complete_endpoint() {
        let missing = build_provider_pr_payload(
            &integration("gitlab", json!({ "privateToken": "glpat_xxx" })),
            None,
            "Fix bug",
            "Details",
            "aos/fix-bug",
            "main",
        );
        assert_eq!(missing["ok"], false);
        assert_eq!(missing["error"]["code"], "missing_project");

        let payload = build_provider_pr_payload(
            &integration(
                "gitlab",
                json!({
                    "apiBase": "https://gitlab.example.com/api/v4",
                    "privateToken": "glpat_xxx",
                    "projectPath": "group/subgroup/project"
                }),
            ),
            None,
            "Fix bug",
            "Details",
            "aos/fix-bug",
            "main",
        );
        assert_eq!(payload["provider"], "gitlab");
        assert_eq!(
            payload["url"],
            "https://gitlab.example.com/api/v4/projects/group%2Fsubgroup%2Fproject/merge_requests"
        );
        assert_eq!(payload["body"]["remove_source_branch"], true);
    }

    #[test]
    fn custom_pr_payload_includes_target_url_without_leaking_token() {
        let payload = build_provider_pr_payload(
            &integration(
                "custom",
                json!({
                    "url": "https://hooks.example.com/aos",
                    "token": "secret-token"
                }),
            ),
            None,
            "Fix bug",
            "Details",
            "aos/fix-bug",
            "main",
        );

        assert_eq!(payload["provider"], "custom");
        assert_eq!(payload["method"], "POST");
        assert_eq!(payload["url"], "https://hooks.example.com/aos");
        assert_eq!(payload["authConfigured"], true);
        assert!(!payload.to_string().contains("secret-token"));
    }

    #[test]
    fn pr_markdown_has_reviewable_sections() {
        let markdown = build_pr_markdown(
            "Fix checkout bug",
            "## Summary\n- Fixed checkout error.",
            "aos/fix-checkout",
            "main",
            Some("checkout-service"),
            Some("https://github.com/acme/checkout-service.git"),
            &[RdPrDraftChangeDto {
                file_path: "src/checkout.rs".to_string(),
                change_type: "modify".to_string(),
                applied: true,
            }],
            &[RdPrDraftTestDto {
                command: "cargo check".to_string(),
                status: "passed".to_string(),
                exit_code: Some(0),
                duration_ms: Some(1234),
            }],
        );

        assert_eq!(markdown.matches("## File Changes").count(), 1);
        assert!(markdown.contains("`src/checkout.rs`"));
        assert!(markdown.contains("`cargo check`: passed (exit 0)"));
    }
}
