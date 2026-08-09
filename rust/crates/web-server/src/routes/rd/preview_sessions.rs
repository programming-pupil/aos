//! Preview debug sessions for Code Studio.

use super::*;
const PREVIEW_COMMAND_TIMEOUT_SECS: u64 = 86_400;
const PREVIEW_PROXY_TIMEOUT_SECS: u64 = 30;
const PREVIEW_SCREENSHOT_TIMEOUT_SECS: u64 = 90;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreviewSessionCreateRequest {
    command: String,
    port: Option<u16>,
    path: Option<String>,
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreviewConsoleEventRequest {
    event_type: Option<String>,
    severity: Option<String>,
    message: String,
    metadata_json: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreviewSessionDto {
    id: String,
    repository_id: String,
    task_id: Option<String>,
    runtime_session_id: Option<String>,
    process_id: Option<String>,
    command: String,
    port: Option<u16>,
    path: String,
    url: Option<String>,
    proxied_url: Option<String>,
    status: String,
    last_error: Option<String>,
    logs_preview: Option<String>,
    started_at: Option<String>,
    stopped_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreviewEventDto {
    id: String,
    event_type: String,
    severity: String,
    message: String,
    metadata_json: Option<Value>,
    created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreviewLogsResponse {
    session: PreviewSessionDto,
    events: Vec<PreviewEventDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreviewAuthorizationResponse {
    url: String,
    expires_in_seconds: u64,
}

pub(super) async fn create_preview_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Json(req): Json<PreviewSessionCreateRequest>,
) -> Result<Json<PreviewSessionDto>, AppError> {
    if req.command.trim().is_empty() {
        return Err(AppError::ValidationError(
            "preview command is required".to_string(),
        ));
    }
    reject_dangerous_command(&req.command)?;
    let repo_root = repository_root(&state, &claims, &repository_id).await?;
    let port = choose_preview_port(req.port.unwrap_or(5173)).await;
    let preview_path = normalize_preview_path(req.path.as_deref());
    let id = format!("rdprev-{}", uuid::Uuid::new_v4());
    let url = format!("http://127.0.0.1:{port}{preview_path}");
    let proxied_url = preview_proxy_url(&id, &preview_path);
    let agent_task_id = match req.task_id.as_deref() {
        Some(task_id) => {
            sqlx::query_scalar::<_, String>(
                "SELECT at.id FROM agent_tasks at
                 WHERE at.tenant_id = ? AND at.owner_user_id = ?
                   AND at.linked_resource_type = 'rd_task' AND at.linked_resource_id = ?
                 ORDER BY at.created_at DESC LIMIT 1",
            )
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .bind(task_id)
            .fetch_optional(&state.db)
            .await?
        }
        None => None,
    };

    let runtime_session = crate::routes::agent_runtime::create_runtime_session(
        &state,
        crate::routes::agent_runtime::RuntimeSessionCreateInput {
            tenant_id: claims.tenant_id.clone(),
            user_id: claims.sub.clone(),
            agent_task_id,
            capability_key: "rd_preview".to_string(),
            isolation_mode: Some("local_process".to_string()),
            workspace_hint: Some(format!("rd-preview/{repository_id}/{id}")),
        },
    )
    .await?;
    let runtime_repo_dir = PathBuf::from(&runtime_session.workspace_root).join("workspace/repo");
    let cwd = prepare_preview_workspace(
        &state,
        &claims,
        &repository_id,
        &id,
        &repo_root,
        &runtime_repo_dir,
    )
    .await?;

    sqlx::query(
        r"
        INSERT INTO rd_preview_sessions
            (id, tenant_id, user_id, repository_id, task_id, runtime_session_id,
             command, port, path, url, proxied_url, status, started_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'starting', CURRENT_TIMESTAMP)
        ",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&repository_id)
    .bind(&req.task_id)
    .bind(&runtime_session.id)
    .bind(req.command.trim())
    .bind(u32::from(port))
    .bind(&preview_path)
    .bind(&url)
    .bind(&proxied_url)
    .execute(&state.db)
    .await?;

    record_preview_event(
        &state,
        &claims.tenant_id,
        &id,
        "preview.started",
        "info",
        "Preview dev server starting",
        Some(json!({ "command": req.command.trim(), "port": port, "url": url })),
    )
    .await?;

    let command = with_preview_port_env(req.command.trim(), port);
    spawn_preview_runtime_command(
        state.clone(),
        claims.tenant_id.clone(),
        id.clone(),
        runtime_session.id.clone(),
        command,
        cwd,
    );

    get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &id)
        .await
        .map(Json)
}

pub(super) async fn get_preview_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<PreviewSessionDto>, AppError> {
    get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id)
        .await
        .map(Json)
}

pub(super) async fn authorize_preview_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<PreviewAuthorizationResponse>, AppError> {
    let session =
        get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id).await?;
    let proxied_url = session.proxied_url.ok_or_else(|| {
        AppError::ValidationError("preview session does not expose a proxy URL".to_string())
    })?;
    let token = crate::auth::create_preview_token(&state, &claims, &session_id).await?;
    let separator = if proxied_url.contains('?') { '&' } else { '?' };
    Ok(Json(PreviewAuthorizationResponse {
        url: format!(
            "{proxied_url}{separator}preview_token={}",
            urlencoding::encode(&token)
        ),
        expires_in_seconds: 4 * 60 * 60,
    }))
}

pub(super) async fn preview_proxy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((session_id, proxy_path)): AxumPath<(String, String)>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Result<Response, AppError> {
    proxy_preview_request(state, claims, session_id, proxy_path, headers, uri).await
}

pub(super) async fn preview_proxy_root(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Result<Response, AppError> {
    proxy_preview_request(state, claims, session_id, String::new(), headers, uri).await
}

async fn proxy_preview_request(
    state: AppState,
    claims: Claims,
    session_id: String,
    proxy_path: String,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Result<Response, AppError> {
    let session =
        get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id).await?;
    let port = session.port.ok_or_else(|| {
        AppError::ValidationError("preview session does not have a bound port".to_string())
    })?;
    let target_path = normalize_proxy_path(&proxy_path);
    let target_query = preview_upstream_query(uri.query());
    let target_url = format!("http://127.0.0.1:{port}{target_path}{target_query}");
    let proxy_query_suffix = preview_proxy_query_suffix(uri.query());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(PREVIEW_PROXY_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| AppError::Internal(format!("preview proxy client failed: {error}")))?;
    let mut request = client.get(&target_url);
    for header_name in [
        axum::http::header::ACCEPT,
        axum::http::header::ACCEPT_LANGUAGE,
        axum::http::header::USER_AGENT,
    ] {
        if let Some(value) = headers.get(&header_name) {
            request = request.header(header_name.as_str(), value.as_bytes());
        }
    }
    let upstream = request
        .send()
        .await
        .map_err(|error| AppError::Internal(format!("preview proxy request failed: {error}")))?;
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = upstream
        .bytes()
        .await
        .map_err(|error| AppError::Internal(format!("preview proxy body failed: {error}")))?;
    let (body, content_type) =
        rewrite_preview_proxy_body(&session_id, &proxy_query_suffix, &content_type, &bytes);
    Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .header(axum::http::header::CACHE_CONTROL, "no-store")
        .header("Referrer-Policy", "no-referrer")
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(body))
        .map_err(|error| AppError::Internal(format!("preview proxy response failed: {error}")))
}

pub(super) async fn stop_preview_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<PreviewSessionDto>, AppError> {
    let session =
        get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id).await?;
    if let Some(runtime_session_id) = session.runtime_session_id.as_deref() {
        crate::routes::agent_runtime::request_cancel_runtime_session(
            &state,
            &claims.tenant_id,
            runtime_session_id,
        )
        .await?;
    }
    sqlx::query(
        r"
        UPDATE rd_preview_sessions
        SET status = 'stopped', stopped_at = COALESCE(stopped_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
        ",
    )
    .bind(&session_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    record_preview_event(
        &state,
        &claims.tenant_id,
        &session_id,
        "preview.stopped",
        "info",
        "Preview dev server stopped",
        None,
    )
    .await?;
    get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id)
        .await
        .map(Json)
}

pub(super) async fn preview_session_logs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<PreviewLogsResponse>, AppError> {
    let session =
        get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id).await?;
    let rows = sqlx::query(
        r"
        SELECT id, event_type, severity, message, CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM rd_preview_events
        WHERE tenant_id = ? AND session_id = ?
        ORDER BY created_at DESC
        LIMIT 100
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&session_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(PreviewLogsResponse {
        session,
        events: rows.into_iter().map(row_to_preview_event).collect(),
    }))
}

pub(super) async fn record_preview_console_event(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
    Json(req): Json<PreviewConsoleEventRequest>,
) -> Result<Json<PreviewEventDto>, AppError> {
    get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id).await?;
    let event_type = req.event_type.as_deref().unwrap_or("console");
    let severity = req.severity.as_deref().unwrap_or("info");
    let event = record_preview_event(
        &state,
        &claims.tenant_id,
        &session_id,
        event_type,
        severity,
        &req.message,
        req.metadata_json,
    )
    .await?;
    Ok(Json(event))
}

pub(super) async fn preview_screenshot(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<PreviewEventDto>, AppError> {
    let session =
        get_preview_session_inner(&state.db, &claims.tenant_id, &claims.sub, &session_id).await?;
    let event = capture_preview_screenshot(&state, &claims.tenant_id, &session).await?;
    Ok(Json(event))
}

async fn capture_preview_screenshot(
    state: &AppState,
    tenant_id: &str,
    session: &PreviewSessionDto,
) -> Result<PreviewEventDto, AppError> {
    let Some(runtime_session_id) = session.runtime_session_id.as_deref() else {
        return record_preview_event(
            state,
            tenant_id,
            &session.id,
            "screenshot.failed",
            "error",
            "Preview session has no runtime session for screenshot capture",
            None,
        )
        .await;
    };
    if !binary_is_installed("npx") {
        return record_preview_event(
            state,
            tenant_id,
            &session.id,
            "screenshot.failed",
            "error",
            "Screenshot capture requires npx and Playwright CLI",
            Some(json!({ "hint": "Install Node.js/npm or add Playwright to the repository dev dependencies." })),
        )
        .await;
    }
    let Some(url) = session
        .url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return record_preview_event(
            state,
            tenant_id,
            &session.id,
            "screenshot.failed",
            "error",
            "Preview session has no URL to capture",
            None,
        )
        .await;
    };
    let workspace_root =
        runtime_session_workspace_root(state, tenant_id, runtime_session_id).await?;
    let cwd = PathBuf::from(&workspace_root).join("workspace/repo");
    let filename = format!(
        "{}-{}.png",
        safe_filename(&session.id),
        chrono::Utc::now().timestamp_millis()
    );
    let relative_path = format!("workspace/.aos/artifacts/{filename}");
    let command = format!(
        "mkdir -p ../.aos/artifacts && npx --yes playwright screenshot --full-page --timeout 60000 {} {}",
        shell_quote(url),
        shell_quote(&format!("../.aos/artifacts/{filename}"))
    );
    let result = crate::routes::agent_runtime::run_runtime_command(
        state,
        crate::routes::agent_runtime::RuntimeCommandInput {
            tenant_id: tenant_id.to_string(),
            runtime_session_id: runtime_session_id.to_string(),
            agent_task_id: None,
            command,
            cwd,
            timeout_secs: PREVIEW_SCREENSHOT_TIMEOUT_SECS,
        },
    )
    .await?;
    if result.status != crate::routes::agent_runtime::RUNTIME_PROCESS_STATUS_COMPLETED {
        return record_preview_event(
            state,
            tenant_id,
            &session.id,
            "screenshot.failed",
            "error",
            "Preview screenshot command failed",
            Some(json!({
                "processId": result.process_id,
                "status": result.status,
                "exitCode": result.exit_code,
                "stderrPreview": truncate_text(&result.stderr_text, 1200),
                "stdoutPreview": truncate_text(&result.stdout_text, 600)
            })),
        )
        .await;
    }
    let screenshot_path = PathBuf::from(&workspace_root).join(&relative_path);
    let bytes = tokio::fs::read(&screenshot_path).await.map_err(|error| {
        AppError::Internal(format!("preview screenshot file read failed: {error}"))
    })?;
    let artifact_id = crate::routes::agent_runtime::write_runtime_artifact(
        state,
        crate::routes::agent_runtime::RuntimeArtifactWriteInput {
            tenant_id: tenant_id.to_string(),
            runtime_session_id: runtime_session_id.to_string(),
            agent_task_id: None,
            artifact_type: "screenshot".to_string(),
            relative_path,
            content: bytes,
            content_text_preview: Some(format!(
                "Preview screenshot for {}",
                session.url.clone().unwrap_or_default()
            )),
        },
    )
    .await?;
    record_preview_event(
        &state,
        tenant_id,
        &session.id,
        "screenshot.captured",
        "info",
        "Preview screenshot captured as runtime artifact",
        Some(json!({
            "artifactId": artifact_id,
            "runtimeSessionId": runtime_session_id,
            "processId": result.process_id,
            "url": session.url,
            "contentType": "image/png"
        })),
    )
    .await
}

fn normalize_preview_path(path: Option<&str>) -> String {
    let mut value = path.unwrap_or("/").trim().to_string();
    if value.is_empty() {
        value = "/".to_string();
    }
    if !value.starts_with('/') {
        value.insert(0, '/');
    }
    value
}

fn normalize_proxy_path(proxy_path: &str) -> String {
    let trimmed = proxy_path.trim_start_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn preview_proxy_url(session_id: &str, preview_path: &str) -> String {
    let path = normalize_preview_path(Some(preview_path));
    format!(
        "/api/v1/rd/preview-sessions/{session_id}/proxy{}",
        if path == "/" { "/".to_string() } else { path }
    )
}

fn rewrite_preview_proxy_body(
    session_id: &str,
    proxy_query_suffix: &str,
    content_type: &str,
    bytes: &[u8],
) -> (Vec<u8>, String) {
    let lower = content_type.to_ascii_lowercase();
    if lower.contains("text/html") {
        let html = String::from_utf8_lossy(bytes);
        let rewritten = inject_preview_capture_script(
            session_id,
            &rewrite_preview_absolute_paths(session_id, proxy_query_suffix, &html),
        );
        return (
            rewritten.into_bytes(),
            "text/html; charset=utf-8".to_string(),
        );
    }
    if lower.contains("javascript")
        || lower.contains("ecmascript")
        || lower.contains("typescript")
        || lower.contains("text/css")
    {
        let text = String::from_utf8_lossy(bytes);
        let rewritten = rewrite_preview_absolute_paths(session_id, proxy_query_suffix, &text);
        return (rewritten.into_bytes(), content_type.to_string());
    }
    (bytes.to_vec(), content_type.to_string())
}

fn rewrite_preview_absolute_paths(
    session_id: &str,
    proxy_query_suffix: &str,
    input: &str,
) -> String {
    let mut out = input.to_string();
    for attr in ["src", "href", "action", "poster"] {
        out = rewrite_preview_quoted_absolute_paths(
            session_id,
            proxy_query_suffix,
            &out,
            &format!("{attr}=\""),
            "\"",
        );
        out = rewrite_preview_quoted_absolute_paths(
            session_id,
            proxy_query_suffix,
            &out,
            &format!("{attr}='"),
            "'",
        );
    }
    for prefix in ["from \"", "import \"", "import(\""] {
        out = rewrite_preview_quoted_absolute_paths(
            session_id,
            proxy_query_suffix,
            &out,
            prefix,
            "\"",
        );
    }
    for prefix in ["from '", "import '", "import('"] {
        out = rewrite_preview_quoted_absolute_paths(
            session_id,
            proxy_query_suffix,
            &out,
            prefix,
            "'",
        );
    }
    out = rewrite_preview_css_urls(session_id, proxy_query_suffix, &out);
    out
}

fn rewrite_preview_quoted_absolute_paths(
    session_id: &str,
    proxy_query_suffix: &str,
    input: &str,
    marker: &str,
    terminator: &str,
) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find(marker) {
        let (before, after_marker_start) = rest.split_at(index);
        out.push_str(before);
        out.push_str(marker);
        let after_marker = &after_marker_start[marker.len()..];
        if let Some(end_index) = after_marker.find(terminator) {
            let (candidate, after_candidate) = after_marker.split_at(end_index);
            out.push_str(&preview_proxy_absolute_url(
                session_id,
                proxy_query_suffix,
                candidate,
            ));
            rest = after_candidate;
        } else {
            out.push_str(after_marker);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

fn rewrite_preview_css_urls(session_id: &str, proxy_query_suffix: &str, input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find("url(/") {
        let (before, after_marker_start) = rest.split_at(index);
        out.push_str(before);
        out.push_str("url(");
        let after_marker = &after_marker_start["url(".len()..];
        if let Some(end_index) = after_marker.find(')') {
            let (candidate, after_candidate) = after_marker.split_at(end_index);
            out.push_str(&preview_proxy_absolute_url(
                session_id,
                proxy_query_suffix,
                candidate.trim_matches(['"', '\'']),
            ));
            rest = after_candidate;
        } else {
            out.push_str(after_marker);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

fn preview_proxy_absolute_url(session_id: &str, proxy_query_suffix: &str, raw: &str) -> String {
    if !raw.starts_with('/')
        || raw.starts_with("//")
        || raw.starts_with("/api/v1/rd/preview-sessions/")
    {
        return raw.to_string();
    }
    let prefix = format!("/api/v1/rd/preview-sessions/{session_id}/proxy");
    let (without_hash, hash) = raw
        .split_once('#')
        .map(|(left, right)| (left, format!("#{right}")))
        .unwrap_or((raw, String::new()));
    if proxy_query_suffix.is_empty() {
        return format!("{prefix}{without_hash}{hash}");
    }
    let glue = if without_hash.contains('?') { '&' } else { '?' };
    format!(
        "{prefix}{without_hash}{glue}{}{}",
        proxy_query_suffix.trim_start_matches('?'),
        hash
    )
}

fn preview_upstream_query(raw: Option<&str>) -> String {
    let filtered = filter_preview_query(raw, false);
    if filtered.is_empty() {
        String::new()
    } else {
        format!("?{filtered}")
    }
}

fn preview_proxy_query_suffix(raw: Option<&str>) -> String {
    let filtered = filter_preview_query(raw, true);
    if filtered.is_empty() {
        String::new()
    } else {
        format!("?{filtered}")
    }
}

fn filter_preview_query(raw: Option<&str>, keep_auth_only: bool) -> String {
    raw.unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.trim().is_empty())
        .filter(|pair| {
            let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
            let is_auth = matches!(key, "preview_token");
            if keep_auth_only {
                is_auth
            } else {
                !is_auth
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn inject_preview_capture_script(session_id: &str, html: &str) -> String {
    let session_json = Value::String(session_id.to_string()).to_string();
    let script = format!(
        r#"<script data-aos-preview-capture>
(function() {{
  if (window.__AOS_PREVIEW_CAPTURE_INSTALLED__) return;
  window.__AOS_PREVIEW_CAPTURE_INSTALLED__ = true;
  const sessionId = {session_json};
  const safeString = (value) => {{
    try {{
      if (typeof value === 'string') return value;
      if (value instanceof Error) return value.stack || value.message || String(value);
      return JSON.stringify(value);
    }} catch (_) {{
      return String(value);
    }}
  }};
  const emit = (eventType, severity, message, metadataJson) => {{
    try {{
      window.parent && window.parent.postMessage({{
        type: 'aos-preview-event',
        sessionId,
        eventType,
        severity,
        message: safeString(message).slice(0, 4000),
        metadataJson: Object.assign({{ href: window.location.href }}, metadataJson || {{}})
      }}, window.location.origin);
    }} catch (_) {{}}
  }};
  ['error', 'warn'].forEach((level) => {{
    const original = console[level];
    console[level] = function(...args) {{
      emit('console.' + level, level === 'error' ? 'error' : 'warn', args.map(safeString).join(' '), {{ args: args.map(safeString) }});
      return original && original.apply(console, args);
    }};
  }});
  window.addEventListener('error', (event) => emit('browser.error', 'error', event.message || 'window error', {{
    filename: event.filename,
    line: event.lineno,
    column: event.colno,
    stack: event.error && event.error.stack
  }}));
  window.addEventListener('unhandledrejection', (event) => emit('browser.unhandled_rejection', 'error', event.reason || 'unhandled promise rejection', {{
    reason: safeString(event.reason)
  }}));
  if (window.fetch) {{
    const originalFetch = window.fetch;
    window.fetch = function(input, init) {{
      const url = typeof input === 'string' ? input : (input && input.url) || '';
      return originalFetch.apply(this, arguments).then((response) => {{
        if (!response.ok) emit('network.fetch', 'error', response.status + ' ' + response.statusText + ' ' + url, {{
          url,
          status: response.status,
          statusText: response.statusText
        }});
        return response;
      }}).catch((error) => {{
        emit('network.fetch', 'error', (error && error.message) || String(error), {{ url, stack: error && error.stack }});
        throw error;
      }});
    }};
  }}
  const originalOpen = XMLHttpRequest.prototype.open;
  const originalSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function(method, url) {{
    this.__aosPreviewRequest = {{ method, url }};
    return originalOpen.apply(this, arguments);
  }};
  XMLHttpRequest.prototype.send = function() {{
    this.addEventListener('loadend', () => {{
      const info = this.__aosPreviewRequest || {{}};
      if (this.status >= 400 || this.status === 0) emit('network.xhr', this.status >= 400 ? 'error' : 'warn',
        this.status + ' ' + (info.method || 'GET') + ' ' + (info.url || ''), {{
          method: info.method,
          url: info.url,
          status: this.status,
          statusText: this.statusText
        }});
    }});
    return originalSend.apply(this, arguments);
  }};
}})();
</script>"#
    );
    if let Some(index) = html.to_ascii_lowercase().rfind("</body>") {
        let mut out = String::with_capacity(html.len() + script.len());
        out.push_str(&html[..index]);
        out.push_str(&script);
        out.push_str(&html[index..]);
        out
    } else {
        format!("{html}{script}")
    }
}

fn with_preview_port_env(command: &str, port: u16) -> String {
    format!("PORT={port} AOS_PREVIEW_PORT={port} {command}")
}

async fn prepare_preview_workspace(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    preview_id: &str,
    repo_root: &Path,
    runtime_repo_dir: &Path,
) -> Result<PathBuf, AppError> {
    if runtime_repo_dir.exists() {
        tokio::fs::remove_dir_all(runtime_repo_dir)
            .await
            .map_err(AppError::Io)?;
    }
    let runtime_parent = runtime_repo_dir
        .parent()
        .ok_or_else(|| AppError::Internal("runtime repo dir has no parent".to_string()))?;
    tokio::fs::create_dir_all(runtime_parent)
        .await
        .map_err(AppError::Io)?;

    match create_rd_candidate_worktree(
        state,
        claims,
        preview_id,
        repository_id,
        None,
        Some(runtime_parent),
    )
    .await
    {
        Ok(candidate) => {
            record_preview_event(
                state,
                &claims.tenant_id,
                preview_id,
                "preview.workspace",
                "info",
                "Preview workspace prepared as isolated candidate worktree",
                Some(json!({
                    "strategy": "git_worktree",
                    "path": candidate.path.to_string_lossy()
                })),
            )
            .await?;
            Ok(candidate.path)
        }
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                repository_id,
                preview_id,
                "preview git worktree preparation failed, falling back to directory copy: {}",
                error
            );
            copy_preview_workspace(repo_root, runtime_repo_dir).await?;
            record_preview_event(
                state,
                &claims.tenant_id,
                preview_id,
                "preview.workspace.degraded",
                "warn",
                "Preview workspace prepared by copying repository files because git worktree was unavailable",
                Some(json!({
                    "strategy": "copy",
                    "error": error.to_string(),
                    "path": runtime_repo_dir.to_string_lossy()
                })),
            )
            .await?;
            Ok(runtime_repo_dir.to_path_buf())
        }
    }
}

async fn copy_preview_workspace(repo_root: &Path, dest: &Path) -> Result<(), AppError> {
    let repo_root = repo_root.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || copy_preview_workspace_blocking(&repo_root, &dest))
        .await
        .map_err(|error| {
            AppError::Internal(format!("preview workspace copy join failed: {error}"))
        })?
}

fn copy_preview_workspace_blocking(repo_root: &Path, dest: &Path) -> Result<(), AppError> {
    std::fs::create_dir_all(dest).map_err(AppError::Io)?;
    let walker = WalkDir::new(repo_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(repo_root)
                .map(|relative| {
                    relative.as_os_str().is_empty()
                        || (!should_skip_path(relative)
                            && relative != Path::new(".git")
                            && relative != Path::new(".aos"))
                })
                .unwrap_or(false)
        });
    for entry in walker {
        let entry = entry.map_err(|error| AppError::Internal(error.to_string()))?;
        let path = entry.path();
        let relative = path.strip_prefix(repo_root).map_err(|error| {
            AppError::Internal(format!("preview workspace path strip failed: {error}"))
        })?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = dest.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).map_err(AppError::Io)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(AppError::Io)?;
            }
            std::fs::copy(path, &target).map_err(AppError::Io)?;
        }
    }
    Ok(())
}

fn spawn_preview_runtime_command(
    state: AppState,
    tenant_id: String,
    preview_id: String,
    runtime_session_id: String,
    command: String,
    cwd: PathBuf,
) {
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            r"
            UPDATE rd_preview_sessions
            SET status = 'running', updated_at = CURRENT_TIMESTAMP
            WHERE id = ? AND tenant_id = ? AND status = 'starting'
            ",
        )
        .bind(&preview_id)
        .bind(&tenant_id)
        .execute(&state.db)
        .await
        {
            tracing::warn!(
                tenant_id = %tenant_id,
                preview_id,
                "failed to mark preview session running before command start: {}",
                error
            );
        }
        if let Err(error) = record_preview_event(
            &state,
            &tenant_id,
            &preview_id,
            "preview.running",
            "info",
            "Preview dev server command launched",
            Some(json!({ "runtimeSessionId": runtime_session_id, "cwd": cwd.to_string_lossy() })),
        )
        .await
        {
            tracing::warn!(
                tenant_id = %tenant_id,
                preview_id,
                "failed to record preview running event: {}",
                error
            );
        }

        spawn_preview_process_id_attach_loop(
            state.clone(),
            tenant_id.clone(),
            preview_id.clone(),
            runtime_session_id.clone(),
        );

        let command_result = crate::routes::agent_runtime::run_runtime_command(
            &state,
            crate::routes::agent_runtime::RuntimeCommandInput {
                tenant_id: tenant_id.clone(),
                runtime_session_id: runtime_session_id.clone(),
                agent_task_id: None,
                command,
                cwd,
                timeout_secs: PREVIEW_COMMAND_TIMEOUT_SECS,
            },
        )
        .await;

        match command_result {
            Ok(result) => {
                let logs_preview = truncate_text(
                    &format!("{}\n{}", result.stdout_text, result.stderr_text),
                    8_000,
                );
                let cancelled =
                    result.status == crate::routes::agent_runtime::RUNTIME_PROCESS_STATUS_CANCELLED;
                let status = if cancelled {
                    "stopped"
                } else if result.status
                    == crate::routes::agent_runtime::RUNTIME_PROCESS_STATUS_TIMED_OUT
                {
                    "failed"
                } else {
                    "failed"
                };
                let last_error = match status {
                    "stopped" => None,
                    _ => Some(format!(
                        "preview command exited unexpectedly: status={}, exit_code={:?}",
                        result.status, result.exit_code
                    )),
                };
                if let Err(error) = sqlx::query(
                    r"
                    UPDATE rd_preview_sessions
                    SET status = CASE WHEN status = 'stopped' THEN status ELSE ? END,
                        logs_preview = ?,
                        last_error = CASE WHEN status = 'stopped' THEN last_error ELSE ? END,
                        stopped_at = CASE WHEN ? = 'stopped' THEN COALESCE(stopped_at, CURRENT_TIMESTAMP) ELSE stopped_at END,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE id = ? AND tenant_id = ?
                    ",
                )
                .bind(status)
                .bind(&logs_preview)
                .bind(last_error.as_deref())
                .bind(status)
                .bind(&preview_id)
                .bind(&tenant_id)
                .execute(&state.db)
                .await
                {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        preview_id,
                        "failed to persist preview command result: {}",
                        error
                    );
                }
                let event_type = if status == "stopped" {
                    "preview.stopped"
                } else {
                    "preview.failed"
                };
                let severity = if status == "stopped" { "info" } else { "error" };
                let message = if status == "stopped" {
                    "Preview command stopped"
                } else {
                    "Preview command exited unexpectedly"
                };
                let _ = record_preview_event(
                    &state,
                    &tenant_id,
                    &preview_id,
                    event_type,
                    severity,
                    message,
                    Some(json!({
                        "status": result.status,
                        "exitCode": result.exit_code,
                        "stdoutPreview": truncate_text(&result.stdout_text, 1_000),
                        "stderrPreview": truncate_text(&result.stderr_text, 1_000)
                    })),
                )
                .await;
            }
            Err(error) => {
                let error_text = error.to_string();
                if let Err(update_error) = sqlx::query(
                    r"
                    UPDATE rd_preview_sessions
                    SET status = CASE WHEN status = 'stopped' THEN status ELSE 'failed' END,
                        last_error = CASE WHEN status = 'stopped' THEN last_error ELSE ? END,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE id = ? AND tenant_id = ?
                    ",
                )
                .bind(&error_text)
                .bind(&preview_id)
                .bind(&tenant_id)
                .execute(&state.db)
                .await
                {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        preview_id,
                        "failed to persist preview command error: {}",
                        update_error
                    );
                }
                let _ = record_preview_event(
                    &state,
                    &tenant_id,
                    &preview_id,
                    "preview.failed",
                    "error",
                    "Preview command failed",
                    Some(json!({ "error": error_text })),
                )
                .await;
            }
        }
    });
}

fn spawn_preview_process_id_attach_loop(
    state: AppState,
    tenant_id: String,
    preview_id: String,
    runtime_session_id: String,
) {
    tokio::spawn(async move {
        for _ in 0..30 {
            match latest_runtime_process_id(&state.db, &tenant_id, &runtime_session_id).await {
                Ok(Some(process_id)) => {
                    if let Err(error) = sqlx::query(
                        r"
                        UPDATE rd_preview_sessions
                        SET process_id = COALESCE(process_id, ?), updated_at = CURRENT_TIMESTAMP
                        WHERE tenant_id = ? AND id = ?
                        ",
                    )
                    .bind(&process_id)
                    .bind(&tenant_id)
                    .bind(&preview_id)
                    .execute(&state.db)
                    .await
                    {
                        tracing::warn!(
                            tenant_id = %tenant_id,
                            preview_id,
                            process_id,
                            "failed to attach preview process id: {}",
                            error
                        );
                    }
                    return;
                }
                Ok(None) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        preview_id,
                        "failed to lookup preview runtime process id: {}",
                        error
                    );
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
}

async fn latest_runtime_process_id(
    db: &SqlitePool,
    tenant_id: &str,
    runtime_session_id: &str,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        r"
        SELECT id
        FROM agent_runtime_processes
        WHERE tenant_id = ? AND runtime_session_id = ?
        ORDER BY started_at DESC, created_at DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(runtime_session_id)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)
}

async fn choose_preview_port(preferred: u16) -> u16 {
    for port in preferred..preferred.saturating_add(50) {
        if tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return port;
        }
    }
    preferred
}

async fn record_preview_event(
    state: &AppState,
    tenant_id: &str,
    session_id: &str,
    event_type: &str,
    severity: &str,
    message: &str,
    metadata_json: Option<Value>,
) -> Result<PreviewEventDto, AppError> {
    let id = format!("rdpe-{}", uuid::Uuid::new_v4());
    sqlx::query(
        r"
        INSERT INTO rd_preview_events
            (id, tenant_id, session_id, event_type, severity, message, metadata_json)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(session_id)
    .bind(event_type)
    .bind(severity)
    .bind(message)
    .bind(metadata_json.as_ref().map(json_to_string).transpose()?)
    .execute(&state.db)
    .await?;

    let row = sqlx::query(
        r"
        SELECT id, event_type, severity, message, CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM rd_preview_events
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(&id)
    .fetch_one(&state.db)
    .await?;
    Ok(row_to_preview_event(row))
}

async fn get_preview_session_inner(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> Result<PreviewSessionDto, AppError> {
    let row = sqlx::query(
        r"
        SELECT id, repository_id, task_id, runtime_session_id, process_id, command, port,
               path, url, proxied_url, status, last_error, logs_preview,
               CAST(started_at AS TEXT) AS started_at,
               CAST(stopped_at AS TEXT) AS stopped_at,
               CAST(created_at AS TEXT) AS created_at,
               CAST(updated_at AS TEXT) AS updated_at
        FROM rd_preview_sessions
        WHERE tenant_id = ? AND user_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("preview session not found".to_string()))?;
    Ok(row_to_preview_session(row))
}

fn row_to_preview_session(row: sqlx::sqlite::SqliteRow) -> PreviewSessionDto {
    PreviewSessionDto {
        id: row.get("id"),
        repository_id: row.get("repository_id"),
        task_id: row.get("task_id"),
        runtime_session_id: row.get("runtime_session_id"),
        process_id: row.get("process_id"),
        command: row.get("command"),
        port: row
            .try_get::<Option<u64>, _>("port")
            .ok()
            .flatten()
            .and_then(|value| u16::try_from(value).ok()),
        path: row.get("path"),
        url: row.get("url"),
        proxied_url: row.get("proxied_url"),
        status: row.get("status"),
        last_error: row.get("last_error"),
        logs_preview: row.get("logs_preview"),
        started_at: row.get("started_at"),
        stopped_at: row.get("stopped_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn row_to_preview_event(row: sqlx::sqlite::SqliteRow) -> PreviewEventDto {
    PreviewEventDto {
        id: row.get("id"),
        event_type: row.get("event_type"),
        severity: row.get("severity"),
        message: row.get("message"),
        metadata_json: parse_json_opt(row.get("metadata_json")),
        created_at: row.get("created_at"),
    }
}

async fn runtime_session_workspace_root(
    state: &AppState,
    tenant_id: &str,
    runtime_session_id: &str,
) -> Result<String, AppError> {
    sqlx::query_scalar(
        "SELECT workspace_root FROM agent_runtime_sessions WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(runtime_session_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("runtime session not found".to_string()))
}

fn binary_is_installed(bin: &str) -> bool {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|path| path.join(bin))
                .find(|candidate| candidate.is_file())
        })
        .is_some()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn safe_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn json_to_string(value: &Value) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(AppError::Json)
}
