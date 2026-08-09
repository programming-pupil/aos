//! PR draft construction and external publish adapters for RD integrations.

use super::*;

const PUBLISH_RESPONSE_MAX_CHARS: usize = 12_000;

pub(in crate::routes::rd) async fn task_pr_draft(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<RdPrDraftQuery>,
) -> Result<Json<RdPrDraftDto>, AppError> {
    build_task_pr_draft(
        &state.db,
        &claims,
        &task_id,
        query.integration_id.as_deref(),
    )
    .await
    .map(Json)
}

pub(in crate::routes::rd) async fn publish_task_pr_draft(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(task_id): AxumPath<String>,
    Json(req): Json<RdPrDraftPublishRequest>,
) -> Result<Json<RdPrDraftPublishResult>, AppError> {
    let integration_id = require_non_empty(&req.integration_id, "integration_id")?;
    let integration = get_integration_record(&state.db, &claims.tenant_id, &integration_id).await?;
    if !integration.enabled {
        return Err(AppError::ValidationError(
            "rd integration is disabled".to_string(),
        ));
    }
    let draft = build_task_pr_draft(&state.db, &claims, &task_id, Some(&integration.id)).await?;
    let result = publish_provider_pr_draft(&integration, draft).await;
    let event_status = if result.ok { "completed" } else { "failed" };
    if let Err(error) = super::super::record_event(
        &state.db,
        &claims.tenant_id,
        &task_id,
        "external_publish",
        event_status,
        &result.message,
        json!({
            "integrationId": &result.integration_id,
            "provider": &result.provider,
            "remoteUrl": &result.remote_url,
            "statusCode": result.status_code,
            "ok": result.ok,
            "response": &result.response_json,
        }),
    )
    .await
    {
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            task_id = %task_id,
            integration_id = %integration_id,
            "failed to record RD external publish event: {}",
            error
        );
    }
    Ok(Json(result))
}

async fn build_task_pr_draft(
    db: &SqlitePool,
    claims: &Claims,
    task_id: &str,
    integration_id: Option<&str>,
) -> Result<RdPrDraftDto, AppError> {
    let row = sqlx::query(
        "SELECT \
           t.id, t.repository_id, t.title, t.prompt, t.plan_md, t.answer_md, t.review_md, \
           t.pr_title, t.pr_description, t.status, \
           p.name AS repository_name, p.url AS repository_url, p.branch AS repository_branch \
         FROM rd_tasks t \
         LEFT JOIN gitlab_projects p ON p.id = t.repository_id AND p.tenant_id = t.tenant_id \
         WHERE t.id = ? AND t.tenant_id = ? AND t.user_id = ?",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("rd task not found".to_string()))?;

    let change_rows = sqlx::query(
        "SELECT file_path, change_type, applied FROM rd_file_changes WHERE task_id = ? AND tenant_id = ? ORDER BY created_at ASC",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .fetch_all(db)
    .await?;
    let test_rows = sqlx::query(
        "SELECT command, status, exit_code, duration_ms FROM rd_test_runs WHERE task_id = ? AND tenant_id = ? ORDER BY created_at DESC LIMIT 8",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .fetch_all(db)
    .await?;

    let repository_id: Option<String> = row.get("repository_id");
    let repository_name: Option<String> = row.get("repository_name");
    let repository_url: Option<String> = row.get("repository_url");
    let base_branch: String = row
        .get::<Option<String>, _>("repository_branch")
        .unwrap_or_else(|| "main".to_string());
    let title = row
        .get::<Option<String>, _>("pr_title")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("AOS: {}", row.get::<String, _>("title")));
    let description = row
        .get::<Option<String>, _>("pr_description")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| build_default_pr_description(&row, &change_rows, &test_rows));
    let branch_name = format!(
        "aos/{}-{}",
        slugify_branch_component(&row.get::<String, _>("title")),
        task_id.chars().take(8).collect::<String>()
    );
    let changes = change_rows
        .iter()
        .map(|row| RdPrDraftChangeDto {
            file_path: row.get("file_path"),
            change_type: row.get("change_type"),
            applied: row.get("applied"),
        })
        .collect::<Vec<_>>();
    let tests = test_rows
        .iter()
        .map(|row| RdPrDraftTestDto {
            command: row.get("command"),
            status: row.get("status"),
            exit_code: row.get("exit_code"),
            duration_ms: row.get("duration_ms"),
        })
        .collect::<Vec<_>>();

    let mut provider_payloads = Vec::new();
    if let Some(integration_id) = integration_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let integration = get_integration_record(db, &claims.tenant_id, integration_id).await?;
        if !integration.enabled {
            return Err(AppError::ValidationError(
                "rd integration is disabled".to_string(),
            ));
        }
        provider_payloads.push(build_provider_pr_payload(
            &integration,
            repository_url.as_deref(),
            &title,
            &description,
            &branch_name,
            &base_branch,
        ));
    }

    let markdown = build_pr_markdown(
        &title,
        &description,
        &branch_name,
        &base_branch,
        repository_name.as_deref(),
        repository_url.as_deref(),
        &changes,
        &tests,
    );

    Ok(RdPrDraftDto {
        task_id: task_id.to_string(),
        title,
        description,
        branch_name,
        base_branch,
        repository_id,
        repository_name,
        repository_url,
        changes,
        tests,
        provider_payloads,
        markdown,
    })
}

fn build_default_pr_description(
    task: &sqlx::sqlite::SqliteRow,
    changes: &[sqlx::sqlite::SqliteRow],
    tests: &[sqlx::sqlite::SqliteRow],
) -> String {
    let mut lines = vec![
        "## Summary".to_string(),
        task.get::<Option<String>, _>("answer_md")
            .or_else(|| task.get::<Option<String>, _>("review_md"))
            .unwrap_or_else(|| task.get::<String, _>("prompt")),
        String::new(),
        "## Plan".to_string(),
        task.get::<Option<String>, _>("plan_md")
            .unwrap_or_else(|| "- Plan was not generated.".to_string()),
        String::new(),
        "## Changes".to_string(),
    ];
    if changes.is_empty() {
        lines.push("- No file changes recorded.".to_string());
    } else {
        for row in changes {
            lines.push(format!(
                "- `{}` ({}, applied={})",
                row.get::<String, _>("file_path"),
                row.get::<String, _>("change_type"),
                row.get::<bool, _>("applied")
            ));
        }
    }
    lines.push(String::new());
    lines.push("## Tests".to_string());
    if tests.is_empty() {
        lines.push("- Not run.".to_string());
    } else {
        for row in tests {
            lines.push(format!(
                "- `{}`: {}{}",
                row.get::<String, _>("command"),
                row.get::<String, _>("status"),
                row.get::<Option<i32>, _>("exit_code")
                    .map(|code| format!(" (exit {code})"))
                    .unwrap_or_default()
            ));
        }
    }
    lines.join("\n")
}

pub(super) fn build_provider_pr_payload(
    integration: &IntegrationRecord,
    repository_url: Option<&str>,
    title: &str,
    description: &str,
    branch_name: &str,
    base_branch: &str,
) -> Value {
    let config = integration.config_json.as_ref().unwrap_or(&Value::Null);
    match integration.provider.as_str() {
        "github" => {
            let api_base = config_string(config, &["apiBase", "api_base"])
                .unwrap_or_else(|| "https://api.github.com".to_string());
            let repository =
                config_string(config, &["repository", "repo", "fullName", "full_name"])
                    .or_else(|| repository_url.and_then(parse_repository_path));
            let Some(repository) = repository.filter(|value| !value.trim().is_empty()) else {
                return missing_provider_payload(
                    "github",
                    "missing_repository",
                    "Configure config.repository as owner/repo or attach a repository URL that can be parsed.",
                );
            };
            json!({
                "provider": "github",
                "method": "POST",
                "url": join_url(&api_base, &format!("/repos/{repository}/pulls")),
                "body": {
                    "title": title,
                    "head": branch_name,
                    "base": base_branch,
                    "body": description,
                    "draft": true
                }
            })
        }
        "gitlab" => {
            let api_base = config_string(config, &["apiBase", "api_base"])
                .unwrap_or_else(|| "https://gitlab.com/api/v4".to_string());
            let project = config_string(
                config,
                &["projectPath", "project_path", "projectId", "project_id"],
            )
            .or_else(|| repository_url.and_then(parse_repository_path));
            let Some(project) = project.filter(|value| !value.trim().is_empty()) else {
                return missing_provider_payload(
                    "gitlab",
                    "missing_project",
                    "Configure config.projectPath/projectId or attach a repository URL that can be parsed.",
                );
            };
            json!({
                "provider": "gitlab",
                "method": "POST",
                "url": join_url(
                    &api_base,
                    &format!("/projects/{}/merge_requests", urlencoding::encode(&project)),
                ),
                "body": {
                    "title": title,
                    "source_branch": branch_name,
                    "target_branch": base_branch,
                    "description": description,
                    "remove_source_branch": true
                }
            })
        }
        _ => {
            let target_url = config_string(
                config,
                &["url", "webhookUrl", "webhook_url", "baseUrl", "base_url"],
            );
            let auth_configured = config_string(
                config,
                &[
                    "token",
                    "accessToken",
                    "access_token",
                    "secret",
                    "apiKey",
                    "api_key",
                ],
            )
            .is_some();
            json!({
                "provider": &integration.provider,
                "method": "POST",
                "url": target_url,
                "authConfigured": auth_configured,
                "body": {
                    "title": title,
                    "branch": branch_name,
                    "base": base_branch,
                    "description": description
                }
            })
        }
    }
}

async fn publish_provider_pr_draft(
    integration: &IntegrationRecord,
    draft: RdPrDraftDto,
) -> RdPrDraftPublishResult {
    let config = integration.config_json.as_ref().unwrap_or(&Value::Null);
    let provider = integration.provider.clone();
    let integration_id = integration.id.clone();
    let publish = match provider.as_str() {
        "github" => publish_github_pr(config, &draft).await,
        "gitlab" => publish_gitlab_mr(config, &draft).await,
        "custom" => publish_custom_webhook(config, &draft).await,
        "jira" | "sentry" => ProviderPublishOutcome {
            ok: false,
            remote_url: None,
            status_code: None,
            message: format!(
                "{} integration does not support PR draft publishing yet.",
                provider
            ),
            response_json: json!({ "error": "unsupported_publish_action" }),
        },
        _ => ProviderPublishOutcome {
            ok: false,
            remote_url: None,
            status_code: None,
            message: format!("Unsupported integration provider: {provider}."),
            response_json: json!({ "error": "unsupported_provider" }),
        },
    };
    RdPrDraftPublishResult {
        ok: publish.ok,
        provider,
        integration_id,
        remote_url: publish.remote_url,
        status_code: publish.status_code,
        message: publish.message,
        response_json: publish.response_json,
        draft,
    }
}

struct ProviderPublishOutcome {
    ok: bool,
    remote_url: Option<String>,
    status_code: Option<u16>,
    message: String,
    response_json: Value,
}

async fn publish_github_pr(config: &Value, draft: &RdPrDraftDto) -> ProviderPublishOutcome {
    let Some(token) = config_string(config, &["token", "accessToken", "access_token"]) else {
        return publish_config_error("github token is required.", "missing_token");
    };
    let Some(repository) = config_string(config, &["repository", "repo", "fullName", "full_name"])
        .or_else(|| {
            draft
                .repository_url
                .as_deref()
                .and_then(parse_repository_path)
        })
        .filter(|value| !value.trim().is_empty())
    else {
        return publish_config_error("github repository is required.", "missing_repository");
    };
    let api_base = config_string(config, &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://api.github.com".to_string());
    let url = join_url(&api_base, &format!("/repos/{repository}/pulls"));
    let body = json!({
        "title": &draft.title,
        "head": &draft.branch_name,
        "base": &draft.base_branch,
        "body": &draft.description,
        "draft": true
    });
    let client = match build_publish_client() {
        Ok(client) => client,
        Err(error) => return publish_internal_error(error),
    };
    let request = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .json(&body);
    send_publish_request("github", url, request).await
}

async fn publish_gitlab_mr(config: &Value, draft: &RdPrDraftDto) -> ProviderPublishOutcome {
    let Some(token) = config_string(
        config,
        &["token", "accessToken", "privateToken", "private_token"],
    ) else {
        return publish_config_error("gitlab token/privateToken is required.", "missing_token");
    };
    let Some(project) = config_string(
        config,
        &["projectPath", "project_path", "projectId", "project_id"],
    )
    .or_else(|| {
        draft
            .repository_url
            .as_deref()
            .and_then(parse_repository_path)
    })
    .filter(|value| !value.trim().is_empty()) else {
        return publish_config_error(
            "gitlab projectPath/projectId is required.",
            "missing_project",
        );
    };
    let api_base = config_string(config, &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://gitlab.com/api/v4".to_string());
    let url = join_url(
        &api_base,
        &format!("/projects/{}/merge_requests", urlencoding::encode(&project)),
    );
    let body = json!({
        "title": &draft.title,
        "source_branch": &draft.branch_name,
        "target_branch": &draft.base_branch,
        "description": &draft.description,
        "remove_source_branch": true
    });
    let client = match build_publish_client() {
        Ok(client) => client,
        Err(error) => return publish_internal_error(error),
    };
    let request = client
        .post(&url)
        .header("PRIVATE-TOKEN", token)
        .header("Accept", "application/json")
        .json(&body);
    send_publish_request("gitlab", url, request).await
}

async fn publish_custom_webhook(config: &Value, draft: &RdPrDraftDto) -> ProviderPublishOutcome {
    let Some(url) = config_string(
        config,
        &["url", "webhookUrl", "webhook_url", "baseUrl", "base_url"],
    ) else {
        return publish_config_error("custom url/webhookUrl/baseUrl is required.", "missing_url");
    };
    let body = json!({
        "type": "rd_pr_draft",
        "title": &draft.title,
        "branch": &draft.branch_name,
        "base": &draft.base_branch,
        "markdown": &draft.markdown,
        "draft": draft,
    });
    let client = match build_publish_client() {
        Ok(client) => client,
        Err(error) => return publish_internal_error(error),
    };
    let mut request = client
        .post(&url)
        .header("Accept", "application/json")
        .json(&body);
    if let Some(token) = config_string(
        config,
        &[
            "token",
            "accessToken",
            "access_token",
            "secret",
            "apiKey",
            "api_key",
        ],
    ) {
        if let Some(header_name) = config_string(config, &["authHeader", "auth_header"]) {
            request = request.header(header_name, token);
        } else {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
    }
    send_publish_request("custom", url, request).await
}

fn build_publish_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("AOS-Code-Studio/1.0")
        .build()
        .map_err(|error| format!("failed to build http client: {error}"))
}

async fn send_publish_request(
    provider: &str,
    url: String,
    request: reqwest::RequestBuilder,
) -> ProviderPublishOutcome {
    let result = timeout(Duration::from_secs(30), request.send()).await;
    match result {
        Ok(Ok(resp)) => {
            let status = resp.status();
            let status_code = status.as_u16();
            let text = match resp.text().await {
                Ok(text) => text,
                Err(error) => {
                    return ProviderPublishOutcome {
                        ok: false,
                        remote_url: Some(url),
                        status_code: Some(status_code),
                        message: format!("Remote publish response read failed: {error}"),
                        response_json: json!({ "error": error.to_string() }),
                    };
                }
            };
            let response_json = parse_publish_response_body(&text);
            let remote_url = extract_remote_url(&response_json).or(Some(url));
            ProviderPublishOutcome {
                ok: status.is_success(),
                remote_url,
                status_code: Some(status_code),
                message: if status.is_success() {
                    format!("{provider} publish completed.")
                } else {
                    format!("{provider} publish returned HTTP {status}.")
                },
                response_json,
            }
        }
        Ok(Err(error)) => ProviderPublishOutcome {
            ok: false,
            remote_url: Some(url),
            status_code: None,
            message: format!("Remote publish failed: {error}"),
            response_json: json!({ "error": error.to_string() }),
        },
        Err(_) => ProviderPublishOutcome {
            ok: false,
            remote_url: Some(url),
            status_code: None,
            message: "Remote publish timed out.".to_string(),
            response_json: json!({ "error": "timeout" }),
        },
    }
}

fn publish_config_error(message: &str, code: &str) -> ProviderPublishOutcome {
    ProviderPublishOutcome {
        ok: false,
        remote_url: None,
        status_code: None,
        message: message.to_string(),
        response_json: json!({ "error": code }),
    }
}

fn publish_internal_error(message: String) -> ProviderPublishOutcome {
    ProviderPublishOutcome {
        ok: false,
        remote_url: None,
        status_code: None,
        message,
        response_json: json!({ "error": "internal_error" }),
    }
}

fn parse_publish_response_body(text: &str) -> Value {
    let truncated = truncate_publish_text(text);
    serde_json::from_str(&truncated).unwrap_or_else(|_| json!({ "body": truncated }))
}

fn truncate_publish_text(text: &str) -> String {
    if text.len() <= PUBLISH_RESPONSE_MAX_CHARS {
        return text.to_string();
    }
    let mut end = 0;
    for (idx, _) in text.char_indices() {
        if idx > PUBLISH_RESPONSE_MAX_CHARS {
            break;
        }
        end = idx;
    }
    format!("{}...<truncated>", &text[..end])
}

fn extract_remote_url(value: &Value) -> Option<String> {
    ["html_url", "web_url", "url", "permalink"]
        .iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn missing_provider_payload(provider: &str, code: &str, message: &str) -> Value {
    json!({
        "provider": provider,
        "ok": false,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

pub(super) fn build_pr_markdown(
    title: &str,
    description: &str,
    branch_name: &str,
    base_branch: &str,
    repository_name: Option<&str>,
    repository_url: Option<&str>,
    changes: &[RdPrDraftChangeDto],
    tests: &[RdPrDraftTestDto],
) -> String {
    let mut lines = vec![
        format!("# {title}"),
        String::new(),
        format!("- Branch: `{branch_name}` -> `{base_branch}`"),
    ];
    if let Some(name) = repository_name {
        lines.push(format!("- Repository: `{name}`"));
    }
    if let Some(url) = repository_url {
        lines.push(format!("- URL: {url}"));
    }
    lines.push(String::new());
    lines.push(description.to_string());
    lines.push(String::new());
    lines.push("## File Changes".to_string());
    if changes.is_empty() {
        lines.push("- No file changes recorded.".to_string());
    } else {
        for change in changes {
            lines.push(format!(
                "- `{}` ({}, applied={})",
                change.file_path, change.change_type, change.applied
            ));
        }
    }
    lines.push(String::new());
    lines.push("## Test Runs".to_string());
    if tests.is_empty() {
        lines.push("- Not run.".to_string());
    } else {
        for test in tests {
            lines.push(format!(
                "- `{}`: {}{}",
                test.command,
                test.status,
                test.exit_code
                    .map(|code| format!(" (exit {code})"))
                    .unwrap_or_default()
            ));
        }
    }
    lines.join("\n")
}

fn slugify_branch_component(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars().take(60) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

pub(super) fn parse_repository_path(url: &str) -> Option<String> {
    let trimmed = url.trim().split(['?', '#']).next().unwrap_or("").trim();
    let value = trimmed.trim_end_matches('/').trim_end_matches(".git");
    if let Some(rest) = value.strip_prefix("git@") {
        return rest
            .split_once(':')
            .map(|(_, path)| path.trim_matches('/').to_string());
    }
    let marker = "://";
    let without_scheme = value
        .find(marker)
        .map(|idx| &value[idx + marker.len()..])
        .unwrap_or(value);
    let mut parts = without_scheme.splitn(2, '/');
    let _host = parts.next()?;
    parts.next().map(|path| path.trim_matches('/').to_string())
}
