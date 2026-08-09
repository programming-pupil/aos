//! Hybrid code intelligence endpoints for Code Studio.

use super::*;
use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, Mutex as TokioMutex};

const LSP_QUERY_TIMEOUT_SECS: u64 = 8;

static LSP_MANAGER: OnceLock<TokioMutex<LspSessionManager>> = OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CodeIntelQueryRequest {
    action: String,
    path: Option<String>,
    line: Option<u32>,
    character: Option<u32>,
    query: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CodeIntelStatusResponse {
    repository_id: String,
    root_path: String,
    languages: Vec<CodeIntelLanguageStatus>,
    fallback_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeIntelLanguageStatus {
    language: String,
    status: String,
    server_command: Option<String>,
    installed: bool,
    last_error: Option<String>,
    updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CodeIntelQueryResponse {
    source: String,
    status: String,
    language: Option<String>,
    locations: Vec<CodeIntelLocation>,
    hover: Option<CodeIntelHover>,
    diagnostics: Vec<Value>,
    message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeIntelHover {
    content: String,
    language: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeIntelLocation {
    path: String,
    line: u64,
    character: u64,
    end_line: Option<u64>,
    end_character: Option<u64>,
    preview: Option<String>,
}

struct LspSessionManager {
    sessions: HashMap<String, LspSession>,
}

struct LspSession {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<Value>,
    next_id: i64,
    opened_documents: HashSet<String>,
}

pub(super) async fn code_intel_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
) -> Result<Json<CodeIntelStatusResponse>, AppError> {
    let root = repository_root(&state, &claims, &repository_id).await?;
    ensure_code_intel_sessions(&state.db, &claims, &repository_id, &root).await?;
    let rows = sqlx::query(
        r"
        SELECT language, status, server_command, last_error,
               CAST(updated_at AS TEXT) AS updated_at
        FROM rd_code_intel_sessions
        WHERE tenant_id = ? AND user_id = ? AND repository_id = ?
        ORDER BY language ASC
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&repository_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(CodeIntelStatusResponse {
        repository_id,
        root_path: root.to_string_lossy().to_string(),
        languages: rows
            .into_iter()
            .map(|row| {
                let command: Option<String> = row.get("server_command");
                CodeIntelLanguageStatus {
                    language: row.get("language"),
                    status: row.get("status"),
                    installed: command
                        .as_deref()
                        .is_some_and(code_intel_command_is_installed),
                    server_command: command,
                    last_error: row.get("last_error"),
                    updated_at: row.get("updated_at"),
                }
            })
            .collect(),
        fallback_available: true,
    }))
}

pub(super) async fn code_intel_restart(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
) -> Result<Json<CodeIntelStatusResponse>, AppError> {
    let root = repository_root(&state, &claims, &repository_id).await?;
    restart_lsp_sessions_for_repository(&claims, &repository_id).await;
    sqlx::query(
        r"
        UPDATE rd_code_intel_sessions
        SET status = CASE WHEN server_command IS NULL THEN 'disconnected' ELSE 'starting' END,
            last_error = NULL,
            started_at = CASE WHEN server_command IS NULL THEN started_at ELSE CURRENT_TIMESTAMP END,
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND user_id = ? AND repository_id = ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&repository_id)
    .execute(&state.db)
    .await?;
    ensure_code_intel_sessions(&state.db, &claims, &repository_id, &root).await?;
    code_intel_status(State(state), Extension(claims), AxumPath(repository_id)).await
}

pub(super) async fn code_intel_query(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Json(req): Json<CodeIntelQueryRequest>,
) -> Result<Json<CodeIntelQueryResponse>, AppError> {
    ensure_repository_exists(&state, &claims, &repository_id).await?;
    let root = repository_root(&state, &claims, &repository_id).await?;
    if let Some(path) = req.path.as_deref() {
        safe_join(&root, path)?;
    }
    let language = req
        .path
        .as_deref()
        .and_then(|path| language_for_path(Path::new(path)));
    ensure_code_intel_sessions(&state.db, &claims, &repository_id, &root).await?;

    let action = req.action.trim();
    let query = resolve_code_intel_query(&root, &req)?;
    let lsp_result = try_lsp_code_intel(
        &root,
        &claims,
        &repository_id,
        &req,
        language.as_deref(),
        &query,
    )
    .await;
    let status_languages = code_intel_status_languages(language.as_deref(), action);
    for status_language in &status_languages {
        update_code_intel_session_after_query(
            &state.db,
            &claims,
            &repository_id,
            status_language,
            lsp_result.as_ref().map(|value| value.as_ref()),
            lsp_result.as_ref().err().map(String::as_str),
        )
        .await?;
    }
    if let Ok(Some(result)) = lsp_result.as_ref() {
        if result.status == "ok" {
            return Ok(Json(result.clone()));
        }
    }
    let symbol_locations = if matches!(
        action,
        "definition" | "references" | "workspace_symbols" | "document_symbols" | "hover"
    ) {
        query_symbol_index(&state.db, &claims.tenant_id, &repository_id, &query, action).await?
    } else {
        Vec::new()
    };

    if !symbol_locations.is_empty() {
        let hover = (action == "hover").then(|| CodeIntelHover {
            content: symbol_locations
                .first()
                .and_then(|item| item.preview.clone())
                .unwrap_or_else(|| query.clone()),
            language: language.clone(),
        });
        return Ok(Json(CodeIntelQueryResponse {
            source: "symbol_index".to_string(),
            status: "degraded".to_string(),
            language,
            locations: symbol_locations,
            hover,
            diagnostics: Vec::new(),
            message: Some(
                lsp_result
                    .err()
                    .unwrap_or_else(|| "LSP did not return a result".to_string())
                    + "; returned symbol index fallback.",
            ),
        }));
    }

    let rg_locations = if matches!(action, "definition" | "references" | "workspace_symbols") {
        rg_code_intel_search(&root, &query, 20).await?
    } else {
        Vec::new()
    };

    if !rg_locations.is_empty() {
        return Ok(Json(CodeIntelQueryResponse {
            source: "rg".to_string(),
            status: "degraded".to_string(),
            language,
            locations: rg_locations,
            hover: None,
            diagnostics: Vec::new(),
            message: Some(
                lsp_result.err().unwrap_or_else(|| {
                    "LSP and symbol index did not return a precise match".to_string()
                }) + "; used rg fallback.",
            ),
        }));
    }

    Ok(Json(CodeIntelQueryResponse {
        source: "none".to_string(),
        status: "not_found".to_string(),
        language,
        locations: Vec::new(),
        hover: None,
        diagnostics: Vec::new(),
        message: Some("No code intelligence result found.".to_string()),
    }))
}

async fn try_lsp_code_intel(
    root: &Path,
    claims: &Claims,
    repository_id: &str,
    req: &CodeIntelQueryRequest,
    language: Option<&str>,
    query: &str,
) -> Result<Option<CodeIntelQueryResponse>, String> {
    let action = req.action.trim();
    if !matches!(
        action,
        "definition"
            | "references"
            | "hover"
            | "document_symbols"
            | "workspace_symbols"
            | "diagnostics"
    ) {
        return Ok(None);
    }
    if action == "workspace_symbols" && language.is_none() {
        let mut errors = Vec::new();
        for candidate_language in LSP_LANGUAGE_ORDER {
            let Some(command) = lsp_command_for_language(candidate_language) else {
                continue;
            };
            if !code_intel_command_is_installed(command) {
                continue;
            }
            match run_single_lsp_code_intel_query(
                root,
                claims,
                repository_id,
                req,
                candidate_language,
                command,
                query,
            )
            .await
            {
                Ok(Some(result)) if result.status == "ok" => return Ok(Some(result)),
                Ok(_) => {}
                Err(error) => errors.push(format!("{candidate_language}: {error}")),
            }
        }
        return if errors.is_empty() {
            Err("no installed language server for workspace symbol query".to_string())
        } else {
            Err(format!(
                "workspace symbol LSP query failed: {}",
                errors.join("; ")
            ))
        };
    }
    let language = language.ok_or_else(|| "no language context for LSP query".to_string())?;
    let command = lsp_command_for_language(language)
        .ok_or_else(|| format!("no configured language server for {language}"))?;
    if !code_intel_command_is_installed(command) {
        return Err(format!("language server is not installed: {command}"));
    }
    run_single_lsp_code_intel_query(root, claims, repository_id, req, language, command, query)
        .await
}

async fn run_single_lsp_code_intel_query(
    root: &Path,
    claims: &Claims,
    repository_id: &str,
    req: &CodeIntelQueryRequest,
    language: &str,
    command: &str,
    query: &str,
) -> Result<Option<CodeIntelQueryResponse>, String> {
    let action = req.action.trim();
    let (relative_path, absolute_path) = match req.path.as_deref() {
        Some(path) => (
            path.to_string(),
            safe_join(root, path).map_err(|error| error.to_string())?,
        ),
        None if action == "workspace_symbols" => ("".to_string(), root.to_path_buf()),
        None => return Ok(None),
    };
    tokio::time::timeout(
        Duration::from_secs(LSP_QUERY_TIMEOUT_SECS),
        run_lsp_query_with_session(
            root,
            claims,
            repository_id,
            &relative_path,
            &absolute_path,
            req,
            language,
            command,
            query,
        ),
    )
    .await
    .map_err(|_| "LSP query timed out".to_string())?
}

async fn run_lsp_query_with_session(
    root: &Path,
    claims: &Claims,
    repository_id: &str,
    relative_path: &str,
    absolute_path: &Path,
    req: &CodeIntelQueryRequest,
    language: &str,
    command: &str,
    query: &str,
) -> Result<Option<CodeIntelQueryResponse>, String> {
    let action = req.action.trim();
    let method = match action {
        "definition" => "textDocument/definition",
        "references" => "textDocument/references",
        "hover" => "textDocument/hover",
        "document_symbols" => "textDocument/documentSymbol",
        "workspace_symbols" => "workspace/symbol",
        "diagnostics" => "textDocument/diagnostic",
        _ => return Ok(None),
    };
    let session_key = lsp_session_key(claims, repository_id, language);
    let manager = lsp_manager();
    let mut manager = manager.lock().await;
    if !manager.sessions.contains_key(&session_key) {
        let session = start_lsp_session(root, language, command).await?;
        manager.sessions.insert(session_key.clone(), session);
    }
    let session = manager
        .sessions
        .get_mut(&session_key)
        .ok_or_else(|| "LSP session missing after start".to_string())?;
    let file_uri = file_uri(absolute_path);
    if action != "workspace_symbols" && !session.opened_documents.contains(&file_uri) {
        let content = tokio::fs::read_to_string(absolute_path)
            .await
            .map_err(|error| format!("read file for LSP failed: {error}"))?;
        write_lsp_message(
            &mut session.stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": file_uri,
                        "languageId": lsp_language_id(language),
                        "version": 1,
                        "text": content
                    }
                }
            }),
        )
        .await?;
        session.opened_documents.insert(file_uri.clone());
    }
    let request_id = session.next_id;
    session.next_id += 1;
    let position_params = json!({
        "textDocument": { "uri": file_uri },
        "position": {
            "line": req.line.unwrap_or(0),
            "character": req.character.unwrap_or(0)
        }
    });
    let params = match action {
        "references" => {
            let mut value = position_params;
            value["context"] = json!({ "includeDeclaration": true });
            value
        }
        "document_symbols" | "diagnostics" => json!({ "textDocument": { "uri": file_uri } }),
        "workspace_symbols" => json!({ "query": query }),
        _ => position_params,
    };
    write_lsp_message(
        &mut session.stdin,
        &json!({ "jsonrpc": "2.0", "id": request_id, "method": method, "params": params }),
    )
    .await?;
    let response = read_lsp_response_from_session(session, request_id).await?;
    Ok(parse_lsp_query_response(
        root,
        relative_path,
        language,
        action,
        response.get("result").cloned().unwrap_or(Value::Null),
    ))
}

async fn start_lsp_session(
    root: &Path,
    language: &str,
    command: &str,
) -> Result<LspSession, String> {
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| "language server command is empty".to_string())?;
    let args = parts.collect::<Vec<_>>();
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("LSP spawn failed: {error}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill().await;
        return Err("LSP stdin unavailable".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return Err("LSP stdout unavailable".to_string());
    };
    let (tx, rx) = mpsc::channel(128);
    tokio::spawn(read_lsp_stdout_loop(stdout, tx));
    let root_uri = file_uri(root);
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "definition": { "dynamicRegistration": false },
                        "references": { "dynamicRegistration": false },
                        "hover": { "dynamicRegistration": false },
                        "documentSymbol": { "dynamicRegistration": false },
                        "publishDiagnostics": { "relatedInformation": false }
                    },
                    "workspace": {
                        "workspaceFolders": false,
                        "symbol": { "dynamicRegistration": false }
                    }
                },
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": root.file_name().and_then(|name| name.to_str()).unwrap_or("workspace")
                }]
            }
        }),
    )
    .await?;
    let mut session = LspSession {
        child,
        stdin,
        responses: rx,
        next_id: 2,
        opened_documents: HashSet::new(),
    };
    let _ = read_lsp_response_from_session(&mut session, 1).await?;
    write_lsp_message(
        &mut session.stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    )
    .await?;
    tracing::debug!(language, command, "persistent LSP session started");
    Ok(session)
}

async fn read_lsp_stdout_loop(stdout: tokio::process::ChildStdout, tx: mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    let mut buffer = Vec::<u8>::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tracing::warn!("persistent LSP stdout read failed: {}", error);
                break;
            }
        };
        buffer.extend_from_slice(&chunk[..read]);
        loop {
            match take_lsp_message(&buffer) {
                Ok(Some((message, consumed))) => {
                    buffer.drain(..consumed);
                    if tx.send(message).await.is_err() {
                        return;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!("persistent LSP response parse failed: {}", error);
                    return;
                }
            }
        }
    }
}

async fn read_lsp_response_from_session(
    session: &mut LspSession,
    expected_id: i64,
) -> Result<Value, String> {
    loop {
        let Some(message) = session.responses.recv().await else {
            return Err("language server response channel closed".to_string());
        };
        if message.get("id").and_then(Value::as_i64) == Some(expected_id) {
            return Ok(message);
        }
    }
}

async fn write_lsp_message(
    stdin: &mut tokio::process::ChildStdin,
    value: &Value,
) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin
        .write_all(header.as_bytes())
        .await
        .map_err(|error| format!("write LSP header failed: {error}"))?;
    stdin
        .write_all(&body)
        .await
        .map_err(|error| format!("write LSP body failed: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("flush LSP message failed: {error}"))
}

fn take_lsp_message(buffer: &[u8]) -> Result<Option<(Value, usize)>, String> {
    let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(None);
    };
    let header = std::str::from_utf8(&buffer[..header_end])
        .map_err(|error| format!("invalid LSP header utf8: {error}"))?;
    let content_length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| "LSP response missing Content-Length".to_string())?;
    let body_start = header_end + 4;
    let body_end = body_start + content_length;
    if buffer.len() < body_end {
        return Ok(None);
    }
    let body = serde_json::from_slice::<Value>(&buffer[body_start..body_end])
        .map_err(|error| format!("invalid LSP JSON response: {error}"))?;
    Ok(Some((body, body_end)))
}

fn parse_lsp_query_response(
    root: &Path,
    fallback_path: &str,
    language: &str,
    action: &str,
    result: Value,
) -> Option<CodeIntelQueryResponse> {
    if action == "hover" {
        let content = parse_lsp_hover(&result)?;
        return Some(CodeIntelQueryResponse {
            source: "lsp".to_string(),
            status: "ok".to_string(),
            language: Some(language.to_string()),
            locations: Vec::new(),
            hover: Some(CodeIntelHover {
                content,
                language: Some(language.to_string()),
            }),
            diagnostics: Vec::new(),
            message: Some("LSP hover result".to_string()),
        });
    }
    if action == "diagnostics" {
        let diagnostics = result
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        return Some(CodeIntelQueryResponse {
            source: "lsp".to_string(),
            status: "ok".to_string(),
            language: Some(language.to_string()),
            locations: Vec::new(),
            hover: None,
            diagnostics,
            message: Some("LSP diagnostics result".to_string()),
        });
    }

    let locations = if action == "document_symbols" {
        parse_lsp_document_symbols(root, fallback_path, &result)
    } else if action == "workspace_symbols" {
        parse_lsp_workspace_symbols(root, fallback_path, &result)
    } else {
        parse_lsp_locations(root, fallback_path, &result)
    };
    (!locations.is_empty()).then(|| CodeIntelQueryResponse {
        source: "lsp".to_string(),
        status: "ok".to_string(),
        language: Some(language.to_string()),
        locations,
        hover: None,
        diagnostics: Vec::new(),
        message: Some("LSP result".to_string()),
    })
}

fn parse_lsp_workspace_symbols(
    root: &Path,
    fallback_path: &str,
    result: &Value,
) -> Vec<CodeIntelLocation> {
    result
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let location = value.get("location")?;
            let uri = location
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let range = location.get("range").unwrap_or(&Value::Null);
            let mut code_location = location_from_lsp_range(root, uri, range, fallback_path)?;
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("symbol");
            let kind = value
                .get("kind")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            code_location.preview = Some(format!("symbol(kind={kind}) {name}"));
            Some(code_location)
        })
        .collect()
}

fn parse_lsp_locations(root: &Path, fallback_path: &str, result: &Value) -> Vec<CodeIntelLocation> {
    let values = if let Some(array) = result.as_array() {
        array.clone()
    } else if result.is_object() {
        vec![result.clone()]
    } else {
        Vec::new()
    };
    values
        .into_iter()
        .filter_map(|value| {
            let target = value.get("targetUri").or_else(|| value.get("uri"))?;
            let range = value
                .get("targetSelectionRange")
                .or_else(|| value.get("targetRange"))
                .or_else(|| value.get("range"))?;
            location_from_lsp_range(
                root,
                target.as_str().unwrap_or_default(),
                range,
                fallback_path,
            )
        })
        .collect()
}

fn parse_lsp_document_symbols(
    root: &Path,
    fallback_path: &str,
    result: &Value,
) -> Vec<CodeIntelLocation> {
    fn walk(root: &Path, fallback_path: &str, values: &[Value], out: &mut Vec<CodeIntelLocation>) {
        for value in values {
            let range = value
                .get("selectionRange")
                .or_else(|| value.get("range"))
                .unwrap_or(&Value::Null);
            if let Some(mut location) = location_from_lsp_range(root, "", range, fallback_path) {
                location.preview = value.get("name").and_then(Value::as_str).map(|name| {
                    let kind = value
                        .get("kind")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    format!("symbol(kind={kind}) {name}")
                });
                out.push(location);
            }
            if let Some(children) = value.get("children").and_then(Value::as_array) {
                walk(root, fallback_path, children, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(values) = result.as_array() {
        walk(root, fallback_path, values, &mut out);
    }
    out
}

fn location_from_lsp_range(
    root: &Path,
    uri: &str,
    range: &Value,
    fallback_path: &str,
) -> Option<CodeIntelLocation> {
    let start = range.get("start")?;
    let end = range.get("end");
    let path = if uri.is_empty() {
        fallback_path.to_string()
    } else {
        file_uri_to_repo_path(root, uri).unwrap_or_else(|| fallback_path.to_string())
    };
    Some(CodeIntelLocation {
        path,
        line: start.get("line").and_then(Value::as_u64).unwrap_or(0),
        character: start.get("character").and_then(Value::as_u64).unwrap_or(0),
        end_line: end
            .and_then(|value| value.get("line"))
            .and_then(Value::as_u64),
        end_character: end
            .and_then(|value| value.get("character"))
            .and_then(Value::as_u64),
        preview: None,
    })
}

fn parse_lsp_hover(result: &Value) -> Option<String> {
    let contents = result.get("contents").unwrap_or(result);
    if let Some(value) = contents.as_str() {
        return Some(value.to_string());
    }
    if let Some(value) = contents.get("value").and_then(Value::as_str) {
        return Some(value.to_string());
    }
    if let Some(array) = contents.as_array() {
        let parts = array
            .iter()
            .filter_map(parse_lsp_hover)
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>();
        return (!parts.is_empty()).then(|| parts.join("\n\n"));
    }
    None
}

fn file_uri(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    format!("file://{}", urlencoding::encode(&raw).replace("%2F", "/"))
}

fn file_uri_to_repo_path(root: &Path, uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?;
    let decoded = urlencoding::decode(raw).ok()?;
    let path = PathBuf::from(decoded.as_ref());
    let relative = path.strip_prefix(root).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn lsp_command_for_language(language: &str) -> Option<&'static str> {
    match language {
        "typescript" | "javascript" => Some("typescript-language-server --stdio"),
        "rust" => Some("rust-analyzer"),
        "python" => Some("pyright-langserver --stdio"),
        "go" => Some("gopls"),
        "java" => Some("jdtls"),
        "c" | "cpp" => Some("clangd"),
        _ => None,
    }
}

const LSP_LANGUAGE_ORDER: &[&str] = &[
    "typescript",
    "javascript",
    "rust",
    "python",
    "go",
    "java",
    "c",
    "cpp",
];

fn lsp_language_id(language: &str) -> &str {
    match language {
        "typescript" => "typescript",
        "javascript" => "javascript",
        "rust" => "rust",
        "python" => "python",
        "go" => "go",
        "java" => "java",
        "c" => "c",
        "cpp" => "cpp",
        other => other,
    }
}

fn code_intel_status_languages<'a>(language: Option<&'a str>, _action: &str) -> Vec<&'a str> {
    if let Some(language) = language {
        return vec![language];
    }
    Vec::new()
}

fn lsp_manager() -> &'static TokioMutex<LspSessionManager> {
    LSP_MANAGER.get_or_init(|| {
        TokioMutex::new(LspSessionManager {
            sessions: HashMap::new(),
        })
    })
}

fn lsp_session_key(claims: &Claims, repository_id: &str, language: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        claims.tenant_id, claims.sub, repository_id, language
    )
}

async fn restart_lsp_sessions_for_repository(claims: &Claims, repository_id: &str) {
    let prefix = format!("{}:{}:{}:", claims.tenant_id, claims.sub, repository_id);
    let manager = lsp_manager();
    let mut manager = manager.lock().await;
    let keys = manager
        .sessions
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(session) = manager.sessions.remove(&key) {
            shutdown_lsp_session(session).await;
        }
    }
}

async fn shutdown_lsp_session(mut session: LspSession) {
    let shutdown_id = session.next_id;
    let _ = write_lsp_message(
        &mut session.stdin,
        &json!({ "jsonrpc": "2.0", "id": shutdown_id, "method": "shutdown", "params": null }),
    )
    .await;
    let _ = write_lsp_message(
        &mut session.stdin,
        &json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    )
    .await;
    let _ = session.child.kill().await;
}

async fn ensure_code_intel_sessions(
    db: &SqlitePool,
    claims: &Claims,
    repository_id: &str,
    root: &Path,
) -> Result<(), AppError> {
    for (language, command) in [
        ("typescript", Some("typescript-language-server --stdio")),
        ("javascript", Some("typescript-language-server --stdio")),
        ("rust", Some("rust-analyzer")),
        ("python", Some("pyright-langserver --stdio")),
        ("go", Some("gopls")),
        ("java", Some("jdtls")),
        ("c", Some("clangd")),
        ("cpp", Some("clangd")),
    ] {
        let installed = command
            .as_deref()
            .is_some_and(code_intel_command_is_installed);
        let status = if installed {
            "starting"
        } else {
            "disconnected"
        };
        let last_error =
            (!installed).then(|| "language server command is not installed".to_string());
        sqlx::query(
            r"
            INSERT INTO rd_code_intel_sessions
                (id, tenant_id, user_id, repository_id, language, status, server_command,
                 root_path, last_error, started_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, CASE WHEN ? THEN CURRENT_TIMESTAMP ELSE NULL END)
            ON CONFLICT DO UPDATE SET
                server_command = excluded.server_command,
                root_path = excluded.root_path,
                status = CASE
                    WHEN status = 'connected' THEN status
                    WHEN excluded.last_error IS NULL THEN 'starting'
                    ELSE 'disconnected'
                END,
                last_error = excluded.last_error,
                updated_at = CURRENT_TIMESTAMP
            ",
        )
        .bind(format!("rdcis-{}", uuid::Uuid::new_v4()))
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(repository_id)
        .bind(language)
        .bind(status)
        .bind(command)
        .bind(root.to_string_lossy().to_string())
        .bind(last_error)
        .bind(installed)
        .execute(db)
        .await?;
    }
    Ok(())
}

async fn update_code_intel_session_after_query(
    db: &SqlitePool,
    claims: &Claims,
    repository_id: &str,
    language: &str,
    lsp_result: std::result::Result<Option<&CodeIntelQueryResponse>, &String>,
    lsp_error: Option<&str>,
) -> Result<(), AppError> {
    let (status, last_error) = match lsp_result {
        Ok(Some(result)) if result.source == "lsp" && result.status == "ok" => ("connected", None),
        Ok(_) => (
            "degraded",
            Some("LSP returned no precise result; fallback may be used"),
        ),
        Err(_) => {
            let error = lsp_error.unwrap_or("LSP query failed");
            if error.contains("not installed") {
                ("disconnected", Some(error))
            } else {
                ("error", Some(error))
            }
        }
    };
    sqlx::query(
        r"
        UPDATE rd_code_intel_sessions
        SET status = ?, last_error = ?, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND user_id = ? AND repository_id = ? AND language = ?
        ",
    )
    .bind(status)
    .bind(last_error)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(repository_id)
    .bind(language)
    .execute(db)
    .await?;
    Ok(())
}

fn code_intel_command_is_installed(command: &str) -> bool {
    let Some(bin) = command.split_whitespace().next() else {
        return false;
    };
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|path| path.join(bin))
                .find(|candidate| candidate.is_file())
        })
        .is_some()
}

fn resolve_code_intel_query(root: &Path, req: &CodeIntelQueryRequest) -> Result<String, AppError> {
    if let Some(query) = req
        .query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(sanitize_symbol_query(query));
    }
    let Some(path) = req.path.as_deref() else {
        return Err(AppError::ValidationError(
            "query or path is required".to_string(),
        ));
    };
    let line = req.line.unwrap_or(0);
    let character = req.character.unwrap_or(0);
    let content = std::fs::read_to_string(safe_join(root, path)?)?;
    Ok(symbol_at_position(&content, line, character)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string()))
}

fn sanitize_symbol_query(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '$' | ':' | '.'))
        .take(128)
        .collect()
}

fn symbol_at_position(content: &str, line: u32, character: u32) -> Option<String> {
    let line_text = content.lines().nth(usize::try_from(line).ok()?)?;
    let chars = line_text.chars().collect::<Vec<_>>();
    let mut idx = usize::try_from(character)
        .ok()?
        .min(chars.len().saturating_sub(1));
    if chars.is_empty() {
        return None;
    }
    while idx > 0 && !symbol_char(chars[idx]) {
        idx -= 1;
    }
    if !symbol_char(chars[idx]) {
        return None;
    }
    let mut start = idx;
    while start > 0 && symbol_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx + 1;
    while end < chars.len() && symbol_char(chars[end]) {
        end += 1;
    }
    Some(chars[start..end].iter().collect())
}

fn symbol_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$')
}

async fn query_symbol_index(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    query: &str,
    action: &str,
) -> Result<Vec<CodeIntelLocation>, AppError> {
    let like = format!("%{query}%");
    let rows = sqlx::query(
        r"
        SELECT file_path, symbol_name, symbol_kind, signature, line_number
        FROM rd_repository_symbols
        WHERE tenant_id = ? AND repository_id = ?
          AND (symbol_name = ? OR symbol_name LIKE ? OR signature LIKE ?)
        ORDER BY
          CASE WHEN symbol_name = ? THEN 0 ELSE 1 END,
          symbol_kind ASC,
          file_path ASC
        LIMIT ?
        ",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .bind(query)
    .bind(&like)
    .bind(&like)
    .bind(query)
    .bind(if action == "references" {
        80_i64
    } else {
        20_i64
    })
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let line = row.get::<u64, _>("line_number").saturating_sub(1);
            CodeIntelLocation {
                path: row.get("file_path"),
                line,
                character: 0,
                end_line: Some(line),
                end_character: None,
                preview: Some(format!(
                    "{} {} {}",
                    row.get::<String, _>("symbol_kind"),
                    row.get::<String, _>("symbol_name"),
                    row.get::<Option<String>, _>("signature")
                        .unwrap_or_default()
                )),
            }
        })
        .collect())
}

async fn rg_code_intel_search(
    root: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<CodeIntelLocation>, AppError> {
    let hits = run_rg_repository_search(root, query, limit).await?;
    Ok(hits
        .unwrap_or_default()
        .into_iter()
        .map(|hit| CodeIntelLocation {
            path: hit.path,
            line: hit.line_number.saturating_sub(1),
            character: 0,
            end_line: None,
            end_character: None,
            preview: Some(hit.snippet),
        })
        .collect())
}
