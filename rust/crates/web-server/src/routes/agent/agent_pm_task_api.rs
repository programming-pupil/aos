use super::*;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Path as FsPath, PathBuf};
use zip::ZipArchive;

fn pm_stage_user_message(stage: &str, status: &str) -> Option<&'static str> {
    pm_stage_user_message_v2(stage, status)
}

fn pm_task_image_summary_model(session_model: &str) -> String {
    if let Some(explicit) = std::env::var("PM_TASK_IMAGE_SUMMARY_MODEL")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
    {
        return explicit;
    }
    // By default, reuse the active session model. This avoids routing to a model
    // that may not exist on the current provider/base URL.
    session_model.trim().to_string()
}

#[cfg(feature = "bot-agents")]
#[derive(Debug, Clone)]
pub(crate) struct PmResearchBotTaskInput {
    pub message: String,
    pub visible_message: Option<String>,
    pub model: Option<String>,
    pub session_id: Option<String>,
}

#[cfg(feature = "bot-agents")]
#[derive(Debug, Clone)]
pub(crate) struct PmResearchBotTaskResult {
    pub task_id: String,
    pub session_id: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PmResearchBotResumeResult {
    pub task_id: String,
    pub session_id: String,
    pub status: String,
    pub restarted: bool,
}

fn summary_text_indicates_no_vision(summary: &str) -> bool {
    let normalized = summary.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    let tokens = [
        "cannot view image",
        "can't view image",
        "cannot see image",
        "can't see image",
        "unable to view image",
        "do not have access to image",
        "i cannot access the image",
        "i'm unable to analyze images",
        "cannot analyze images",
        "look at the image",
        "看不到图",
        "看不到图片",
        "无法查看图片",
        "无法看到图片",
        "无法识别图片",
        "无法分析图片",
        "当前看不到附件",
    ];
    tokens.iter().any(|token| normalized.contains(token))
}

fn pm_image_context_warning_message(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("image")
        && (lower.contains("unsupported")
            || lower.contains("not support")
            || lower.contains("does not support")
            || lower.contains("no vision")
            || lower.contains("cannot view image")
            || lower.contains("can't view image")
            || lower.contains("cannot see image")
            || lower.contains("can't see image"))
    {
        "图片解析失败：当前模型或路由可能不支持视觉输入，已降级为文本回答。"
    } else {
        "图片解析部分失败，系统将继续基于可用信息回答。"
    }
}

fn pm_detail_has_subtask_hint(detail: Option<&serde_json::Value>) -> bool {
    let Some(obj) = detail.and_then(|value| value.as_object()) else {
        return false;
    };
    for key in ["targetSubtask", "subtask", "subtaskKey", "subtaskId"] {
        if obj
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        {
            return true;
        }
    }
    false
}

fn pm_detail_has_image_context_warning(detail: Option<&serde_json::Value>) -> bool {
    detail
        .and_then(|value| value.as_object())
        .and_then(|obj| obj.get("imageContextWarning"))
        .and_then(|warn| warn.as_object())
        .is_some()
}

fn pm_extra_sse_event_name(evt: &PmResearchTaskEvent) -> Option<&'static str> {
    if pm_detail_has_image_context_warning(evt.detail.as_ref()) {
        return Some("image_context_warning");
    }
    let stage = evt.stage.as_deref().unwrap_or_default();
    match (stage, evt.status.as_str()) {
        ("retrieve", "running") if pm_detail_has_subtask_hint(evt.detail.as_ref()) => {
            Some("subtask_started")
        }
        ("retrieve", "completed") if pm_detail_has_subtask_hint(evt.detail.as_ref()) => {
            Some("subtask_completed")
        }
        ("retrieve", "failed") if pm_detail_has_subtask_hint(evt.detail.as_ref()) => {
            Some("subtask_failed")
        }
        ("synthesize", "running") => Some("merge_started"),
        ("synthesize", "completed" | "failed" | "cancelled") => Some("merge_completed"),
        _ => None,
    }
}

fn pm_task_event_same_view(a: &PmResearchTaskEvent, b: &PmResearchTaskEvent) -> bool {
    a.status == b.status
        && a.stage == b.stage
        && a.attempt == b.attempt
        && a.message == b.message
        && a.elapsed_ms == b.elapsed_ms
        && a.stage_elapsed_ms == b.stage_elapsed_ms
        && a.detail == b.detail
        && a.response == b.response
        && a.error == b.error
}

fn pm_select_best_task_snapshot(
    db_snapshot: Option<(PmResearchTaskEvent, bool)>,
    memory_snapshot: Option<(PmResearchTaskEvent, bool)>,
) -> Option<(PmResearchTaskEvent, bool)> {
    fn is_completed_with_response(pair: &(PmResearchTaskEvent, bool)) -> bool {
        pair.0.status == "completed" && pair.0.response.is_some()
    }
    match (db_snapshot, memory_snapshot) {
        (Some(db_pair), Some(mem_pair))
            if is_completed_with_response(&db_pair) && !is_completed_with_response(&mem_pair) =>
        {
            Some(db_pair)
        }
        (Some(db_pair), _) if !pm_task_is_terminal_status(&db_pair.0.status) => Some(db_pair),
        (_, Some(mem_pair))
            if pm_task_is_terminal_status(&mem_pair.0.status)
                || pm_task_event_is_terminal(&mem_pair.0) =>
        {
            Some(mem_pair)
        }
        (Some(db_pair), None) => Some(db_pair),
        (Some(db_pair), Some(_)) => Some(db_pair),
        (None, Some(mem_pair)) => Some(mem_pair),
        (None, None) => None,
    }
}

async fn pm_load_best_task_snapshot(
    state: &AppState,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<Option<(PmResearchTaskEvent, bool)>, AppError> {
    let db_snapshot =
        load_pm_task_snapshot_from_db(state.control_db(), task_id, tenant_id, user_id).await?;
    let memory_snapshot = pm_research_task_manager()
        .snapshot(task_id, tenant_id, user_id)
        .await;
    Ok(pm_select_best_task_snapshot(db_snapshot, memory_snapshot))
}

async fn pm_refresh_nonterminal_task_state(
    state: &AppState,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    _reason: &'static str,
) -> Result<Option<(PmResearchTaskEvent, bool)>, AppError> {
    // Deadline recovery belongs to the PM governance worker. Status and SSE
    // reads must not mutate professional task state.
    pm_load_best_task_snapshot(state, task_id, tenant_id, user_id).await
}

fn pm_task_image_max_count() -> usize {
    pm_env_usize("PM_TASK_IMAGE_MAX_COUNT", 5).max(1)
}

fn pm_task_image_max_bytes() -> usize {
    pm_env_usize("PM_TASK_IMAGE_MAX_BYTES", 8 * 1024 * 1024).max(256 * 1024)
}

fn pm_task_stream_db_poll_interval(live_broadcast_available: bool) -> Duration {
    let (key, default_ms, minimum_ms) = if live_broadcast_available {
        ("PM_RESEARCH_TASK_LIVE_DB_RECOVERY_POLL_MS", 15_000, 2_000)
    } else {
        ("PM_RESEARCH_TASK_STREAM_DB_POLL_MS", 2_500, 500)
    };
    Duration::from_millis(
        env::var(key)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value >= minimum_ms)
            .unwrap_or(default_ms),
    )
}

fn pm_task_image_max_total_bytes() -> usize {
    pm_env_usize("PM_TASK_IMAGE_MAX_TOTAL_BYTES", 24 * 1024 * 1024).max(pm_task_image_max_bytes())
}

fn pm_task_image_summary_max_tokens() -> u32 {
    u32::try_from(pm_env_usize("PM_TASK_IMAGE_SUMMARY_MAX_TOKENS", 1200))
        .unwrap_or(1200)
        .max(256)
}

fn pm_task_image_summary_timeout_secs() -> u64 {
    pm_env_u64("PM_TASK_IMAGE_SUMMARY_TIMEOUT_SECS", 60).max(15)
}

fn pm_task_document_max_count() -> usize {
    pm_env_usize("PM_TASK_DOCUMENT_MAX_COUNT", 8).max(1)
}

fn pm_task_document_max_bytes() -> usize {
    pm_env_usize("PM_TASK_DOCUMENT_MAX_BYTES", 10 * 1024 * 1024).max(64 * 1024)
}

fn pm_task_document_max_total_chars() -> usize {
    pm_env_usize("PM_TASK_DOCUMENT_MAX_TOTAL_CHARS", 48_000).max(4_000)
}

fn pm_task_document_max_chars_per_file() -> usize {
    pm_env_usize("PM_TASK_DOCUMENT_MAX_CHARS_PER_FILE", 16_000).max(2_000)
}

fn pm_task_csv_profile_max_rows() -> usize {
    pm_env_usize("PM_TASK_CSV_PROFILE_MAX_ROWS", 2_000)
        .max(50)
        .min(20_000)
}

pub(super) fn pm_is_supported_image_media_type(media_type: &str) -> bool {
    matches!(
        media_type.trim().to_ascii_lowercase().as_str(),
        "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/gif"
            | "image/webp"
            | "image/heic"
            | "image/heif"
            | "image/bmp"
    )
}

pub(super) fn pm_infer_image_media_type_from_filename(filename: &str) -> Option<&'static str> {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "heic" => Some("image/heic"),
        "heif" => Some("image/heif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

pub(super) fn pm_infer_document_media_type_from_filename(filename: &str) -> Option<&'static str> {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" => Some("text/plain"),
        "md" | "markdown" => Some("text/markdown"),
        "csv" => Some("text/csv"),
        "sql" => Some("application/sql"),
        "rtf" => Some("text/rtf"),
        "html" | "htm" => Some("text/html"),
        "css" => Some("text/css"),
        "js" | "mjs" | "ts" | "tsx" | "jsx" => Some("text/javascript"),
        "json" => Some("application/json"),
        "xml" => Some("application/xml"),
        "doc" => Some("application/msword"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "odt" => Some("application/vnd.oasis.opendocument.text"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}

pub(super) fn pm_is_supported_document_media_type(media_type: &str) -> bool {
    matches!(
        media_type.trim().to_ascii_lowercase().as_str(),
        "text/plain"
            | "text/markdown"
            | "text/csv"
            | "application/sql"
            | "text/rtf"
            | "text/html"
            | "text/css"
            | "text/javascript"
            | "application/json"
            | "application/xml"
            | "text/xml"
            | "application/rtf"
            | "application/msword"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.oasis.opendocument.text"
            | "application/pdf"
            | "application/octet-stream"
    )
}

pub(super) fn pm_parse_upload_asset_ref(raw_url: &str) -> Option<(String, String)> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let url_without_query = trimmed.split('?').next().unwrap_or(trimmed);
    let marker = "/api/v1/uploads/";
    let pos = url_without_query.find(marker)?;
    let tail = &url_without_query[(pos + marker.len())..];
    let mut segs = tail.split('/');
    let user_id = segs.next()?.trim();
    let filename = segs.next()?.trim();
    if user_id.is_empty() || filename.is_empty() || segs.next().is_some() {
        return None;
    }
    if filename.contains("..") || filename.contains('\\') {
        return None;
    }
    Some((user_id.to_string(), filename.to_string()))
}

pub(super) fn pm_upload_file_path(data_dir: &FsPath, user_id: &str, filename: &str) -> PathBuf {
    data_dir
        .join(".aos")
        .join("uploads")
        .join(user_id)
        .join(filename)
}

pub(crate) fn normalize_pm_task_input_context(
    images: &[PmTaskImageInput],
    documents: &[PmTaskDocumentInput],
    expected_user_id: &str,
) -> Result<Option<PmTaskInputContext>, AppError> {
    if images.is_empty() && documents.is_empty() {
        return Ok(None);
    }
    let max_count = pm_task_image_max_count();
    if images.len() > max_count {
        return Err(AppError::ValidationError(format!(
            "too many images: maximum {max_count}"
        )));
    }

    let mut normalized = Vec::<PmTaskImageInput>::with_capacity(images.len());
    for image in images {
        let url = image.url.trim();
        if url.is_empty() {
            return Err(AppError::ValidationError(
                "image url cannot be empty".to_string(),
            ));
        }
        let (url_user_id, filename) = pm_parse_upload_asset_ref(url).ok_or_else(|| {
            AppError::ValidationError(
                "image url must be an uploaded asset path under /api/v1/uploads/{user_id}/..."
                    .to_string(),
            )
        })?;
        if url_user_id != expected_user_id {
            return Err(AppError::ValidationError(
                "image url does not belong to current user".to_string(),
            ));
        }
        let normalized_media_type = image
            .media_type
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());
        if let Some(media_type) = normalized_media_type.as_deref() {
            if !pm_is_supported_image_media_type(media_type) {
                return Err(AppError::ValidationError(format!(
                    "unsupported image media type: {media_type}"
                )));
            }
        }
        let canonical_url = format!("/api/v1/uploads/{url_user_id}/{filename}");
        normalized.push(PmTaskImageInput {
            url: canonical_url,
            file_id: image.file_id.clone(),
            media_type: normalized_media_type,
            name: image.name.clone(),
            size_bytes: image.size_bytes,
        });
    }

    let max_document_count = pm_task_document_max_count();
    if documents.len() > max_document_count {
        return Err(AppError::ValidationError(format!(
            "too many documents: maximum {max_document_count}"
        )));
    }
    let mut normalized_documents = Vec::<PmTaskDocumentInput>::with_capacity(documents.len());
    for document in documents {
        let url = document.url.trim();
        if url.is_empty() {
            return Err(AppError::ValidationError(
                "document url cannot be empty".to_string(),
            ));
        }
        let (url_user_id, filename) = pm_parse_upload_asset_ref(url).ok_or_else(|| {
            AppError::ValidationError(
                "document url must be an uploaded asset path under /api/v1/uploads/{user_id}/..."
                    .to_string(),
            )
        })?;
        if url_user_id != expected_user_id {
            return Err(AppError::ValidationError(
                "document url does not belong to current user".to_string(),
            ));
        }
        let normalized_media_type = document
            .media_type
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .or_else(|| pm_infer_document_media_type_from_filename(&filename).map(str::to_string))
            .unwrap_or_else(|| "application/octet-stream".to_string());
        if !pm_is_supported_document_media_type(&normalized_media_type) {
            return Err(AppError::ValidationError(format!(
                "unsupported document media type: {normalized_media_type}"
            )));
        }
        let canonical_url = format!("/api/v1/uploads/{url_user_id}/{filename}");
        normalized_documents.push(PmTaskDocumentInput {
            url: canonical_url,
            file_id: document.file_id.clone(),
            media_type: Some(normalized_media_type),
            name: document.name.clone().or(Some(filename)),
            size_bytes: document.size_bytes,
        });
    }

    Ok(Some(PmTaskInputContext {
        images: normalized,
        documents: normalized_documents,
        visible_message: None,
    }))
}

pub(super) async fn pm_read_task_image_bytes(
    data_dir: &FsPath,
    expected_user_id: &str,
    image: &PmTaskImageInput,
    max_bytes: usize,
) -> Result<(Vec<u8>, String, String), String> {
    let (url_user_id, filename) =
        pm_parse_upload_asset_ref(&image.url).ok_or_else(|| "invalid upload url".to_string())?;
    if url_user_id != expected_user_id {
        return Err("upload url user mismatch".to_string());
    }
    let path = pm_upload_file_path(data_dir, &url_user_id, &filename);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|error| format!("read image metadata failed: {error}"))?;
    if usize::try_from(meta.len()).unwrap_or(usize::MAX) > max_bytes {
        return Err(format!(
            "image too large: {} bytes (max {max_bytes})",
            meta.len()
        ));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| format!("read image file failed: {error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "image too large after load: {} bytes (max {max_bytes})",
            bytes.len()
        ));
    }
    let media_type = image
        .media_type
        .clone()
        .or_else(|| pm_infer_image_media_type_from_filename(&filename).map(str::to_string))
        .unwrap_or_else(|| "image/png".to_string());
    if !pm_is_supported_image_media_type(&media_type) {
        return Err(format!("unsupported image media type: {media_type}"));
    }
    Ok((bytes, media_type, filename))
}

fn pm_strip_basic_markup(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut entity = String::new();
    for ch in input.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                out.push(' ');
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch == '&' {
            entity.clear();
            entity.push(ch);
            continue;
        }
        if !entity.is_empty() {
            entity.push(ch);
            if ch == ';' || entity.len() > 12 {
                match entity.as_str() {
                    "&amp;" => out.push('&'),
                    "&lt;" => out.push('<'),
                    "&gt;" => out.push('>'),
                    "&quot;" => out.push('"'),
                    "&apos;" | "&#39;" => out.push('\''),
                    "&nbsp;" => out.push(' '),
                    _ => out.push_str(&entity),
                }
                entity.clear();
            }
            continue;
        }
        out.push(ch);
    }
    if !entity.is_empty() {
        out.push_str(&entity);
    }
    out
}

fn pm_strip_rtf(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut depth = 0usize;
    while let Some(ch) = chars.next() {
        match ch {
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            '\\' => {
                while let Some(next) = chars.peek().copied() {
                    if next.is_alphabetic() || next == '-' || next.is_ascii_digit() {
                        let _ = chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek().is_some_and(|next| *next == ' ') {
                    let _ = chars.next();
                }
                if depth <= 2 {
                    out.push(' ');
                }
            }
            '\n' | '\r' => out.push('\n'),
            other if depth <= 2 => out.push(other),
            _ => {}
        }
    }
    out
}

fn pm_extract_docx_text(bytes: &[u8]) -> Result<String, String> {
    let reader = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(reader).map_err(|error| format!("open docx failed: {error}"))?;
    let mut document = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|error| format!("read docx document.xml failed: {error}"))?
        .read_to_string(&mut document)
        .map_err(|error| format!("read docx xml text failed: {error}"))?;
    Ok(pm_strip_basic_markup(&document))
}

fn pm_document_bytes_to_text(
    media_type: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, String> {
    let normalized = media_type.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "text/plain"
        | "text/markdown"
        | "text/csv"
        | "application/sql"
        | "text/css"
        | "text/javascript"
        | "application/json"
        | "application/xml"
        | "text/xml" => String::from_utf8(bytes.to_vec())
            .map_err(|error| format!("document is not valid utf-8: {error}")),
        "text/html" => String::from_utf8(bytes.to_vec())
            .map(|raw| pm_strip_basic_markup(&raw))
            .map_err(|error| format!("html document is not valid utf-8: {error}")),
        "text/rtf" | "application/rtf" => String::from_utf8(bytes.to_vec())
            .map(|raw| pm_strip_rtf(&raw))
            .map_err(|error| format!("rtf document is not valid utf-8: {error}")),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            pm_extract_docx_text(bytes)
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            crate::routes::chat_intelligence::extract_xlsx_text(bytes)
        }
        "application/msword" => Err(
            "legacy .doc binary text extraction is not available; please upload .docx/.txt/.md for full-text analysis"
                .to_string(),
        ),
        "application/pdf" => Err(
            "pdf text extraction is not available in this chat path; please upload txt/md/docx for full-text analysis"
                .to_string(),
        ),
        "application/octet-stream" => {
            if filename.ends_with(".docx") {
                pm_extract_docx_text(bytes)
            } else if filename.ends_with(".xlsx") {
                crate::routes::chat_intelligence::extract_xlsx_text(bytes)
            } else {
                String::from_utf8(bytes.to_vec())
                    .map_err(|_| "binary document cannot be decoded as utf-8".to_string())
            }
        }
        _ => Err(format!("unsupported document media type: {media_type}")),
    }
}

fn pm_parse_csv_rows(text: &str, max_rows: usize) -> Vec<Vec<String>> {
    let mut rows = Vec::<Vec<String>>::new();
    let mut row = Vec::<String>::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                field.push('"');
                let _ = chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                row.push(field.trim().trim_start_matches('\u{feff}').to_string());
                field.clear();
            }
            '\n' if !in_quotes => {
                row.push(field.trim().trim_start_matches('\u{feff}').to_string());
                field.clear();
                if row.iter().any(|value| !value.trim().is_empty()) {
                    rows.push(row);
                    if rows.len() >= max_rows {
                        return rows;
                    }
                }
                row = Vec::new();
            }
            '\r' if !in_quotes => {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                row.push(field.trim().trim_start_matches('\u{feff}').to_string());
                field.clear();
                if row.iter().any(|value| !value.trim().is_empty()) {
                    rows.push(row);
                    if rows.len() >= max_rows {
                        return rows;
                    }
                }
                row = Vec::new();
            }
            other => field.push(other),
        }
    }

    if !field.is_empty() || !row.is_empty() {
        row.push(field.trim().trim_start_matches('\u{feff}').to_string());
        if row.iter().any(|value| !value.trim().is_empty()) {
            rows.push(row);
        }
    }
    rows
}

fn pm_parse_number(raw: &str) -> Option<f64> {
    let cleaned = raw
        .trim()
        .trim_matches('"')
        .replace(['$', '%', ','], "")
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return None;
    }
    cleaned
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn pm_format_number(value: f64) -> String {
    if !value.is_finite() {
        return "n/a".to_string();
    }
    let abs = value.abs();
    if abs >= 1000.0 {
        format!("{value:.2}")
    } else if abs >= 10.0 {
        format!("{value:.3}")
    } else {
        format!("{value:.4}")
    }
    .trim_end_matches('0')
    .trim_end_matches('.')
    .to_string()
}

fn pm_csv_safe_cell(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        compact
    } else {
        let mut out = compact
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        out.push('…');
        out
    }
}

fn pm_markdown_table(headers: &[String], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let header = format!("| {} |", headers.join(" | "));
    let sep = format!(
        "| {} |",
        headers
            .iter()
            .map(|_| "---".to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let mut lines = vec![header, sep];
    for row in rows {
        let mut cells = Vec::with_capacity(headers.len());
        for idx in 0..headers.len() {
            cells.push(pm_csv_safe_cell(
                row.get(idx).map(String::as_str).unwrap_or(""),
                80,
            ));
        }
        lines.push(format!("| {} |", cells.join(" | ")));
    }
    lines.join("\n")
}

fn pm_csv_dimension_priority(header: &str, distinct_count: usize) -> usize {
    let lower = header.trim().to_ascii_lowercase();
    let semantic_bonus = if lower.contains("group")
        || lower.contains("segment")
        || lower.contains("cohort")
        || lower.contains("type")
        || lower.ends_with("_id")
        || lower == "id"
    {
        0
    } else if lower.contains("date") || lower == "dt" || lower.contains("day") {
        20
    } else {
        10
    };
    semantic_bonus + distinct_count
}

fn pm_csv_header_looks_like_dimension(header: &str) -> bool {
    let lower = header.trim().to_ascii_lowercase();
    lower.contains("group")
        || lower.contains("segment")
        || lower.contains("cohort")
        || lower.contains("type")
        || lower.ends_with("_id")
        || lower == "id"
        || lower.contains("date")
        || lower == "dt"
        || lower.contains("day")
}

fn pm_csv_metric_priority(header: &str, question: &str, stat: &impl Fn() -> (usize, f64)) -> usize {
    let lower_header = header.trim().to_ascii_lowercase();
    let lower_question = question.trim().to_ascii_lowercase();
    let direct_match = (!lower_header.is_empty() && lower_question.contains(&lower_header))
        || lower_header
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .filter(|part| part.len() >= 3)
            .any(|part| lower_question.contains(part));
    let semantic_match_rank = [
        ("收入", ["rev", "revenue", "income", "sales"].as_slice()),
        ("成本", ["cost", "expense", "spend"].as_slice()),
        ("时长", ["duration", "time", "session"].as_slice()),
        ("启动", ["launch", "luanch", "start"].as_slice()),
        ("展示", ["show", "impression", "pv"].as_slice()),
        ("用户", ["dau", "uv", "user"].as_slice()),
        ("aipu", ["dau", "show", "show_pv", "pv", "uv"].as_slice()),
        ("ecpm", ["ecpm", "cpm"].as_slice()),
        ("roi", ["roi", "roas"].as_slice()),
    ]
    .iter()
    .position(|(cn, aliases)| {
        lower_question.contains(cn)
            && aliases
                .iter()
                .any(|alias| lower_header.contains(&alias.to_ascii_lowercase()))
    });
    let (count, span) = stat();
    let constant_penalty = usize::from(span.abs() < f64::EPSILON) * 200;
    let sparsity_penalty = usize::from(count == 0) * 500;
    if direct_match {
        sparsity_penalty + constant_penalty
    } else if let Some(rank) = semantic_match_rank {
        rank + sparsity_penalty + constant_penalty
    } else {
        40 + sparsity_penalty + constant_penalty
    }
}

fn pm_build_csv_profile(filename: &str, text: &str, question: &str) -> Option<String> {
    let rows = pm_parse_csv_rows(text, pm_task_csv_profile_max_rows().saturating_add(1));
    if rows.len() < 2 {
        return None;
    }
    let headers = rows[0]
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let value = value.trim();
            if value.is_empty() {
                format!("column_{}", idx + 1)
            } else {
                value.to_string()
            }
        })
        .collect::<Vec<_>>();
    let data_rows = rows[1..].to_vec();
    if data_rows.is_empty() || headers.is_empty() {
        return None;
    }

    #[derive(Debug, Clone, Default)]
    struct NumStat {
        count: usize,
        sum: f64,
        min: f64,
        max: f64,
    }
    impl NumStat {
        fn add(&mut self, value: f64) {
            if self.count == 0 {
                self.min = value;
                self.max = value;
            } else {
                self.min = self.min.min(value);
                self.max = self.max.max(value);
            }
            self.count += 1;
            self.sum += value;
        }
        fn avg(&self) -> f64 {
            if self.count == 0 {
                0.0
            } else {
                self.sum / self.count as f64
            }
        }
    }

    let mut numeric_stats = vec![NumStat::default(); headers.len()];
    let mut distinct_values = vec![BTreeMap::<String, usize>::new(); headers.len()];
    for row in &data_rows {
        for (idx, header) in headers.iter().enumerate() {
            let raw = row.get(idx).map(String::as_str).unwrap_or("").trim();
            if raw.is_empty() {
                continue;
            }
            if let Some(value) = pm_parse_number(raw) {
                numeric_stats[idx].add(value);
            }
            let values = &mut distinct_values[idx];
            if values.len() < 60 || values.contains_key(raw) {
                *values.entry(raw.to_string()).or_insert(0) += 1;
            }
            let _ = header;
        }
    }

    let all_numeric_indices = numeric_stats
        .iter()
        .enumerate()
        .filter_map(|(idx, stat)| {
            (stat.count >= data_rows.len().saturating_div(2).max(1)).then_some(idx)
        })
        .collect::<Vec<_>>();
    let mut dimension_candidates = distinct_values
        .iter()
        .enumerate()
        .filter_map(|(idx, values)| {
            let distinct = values.len();
            (pm_csv_header_looks_like_dimension(&headers[idx])
                && distinct >= 2
                && distinct <= 12
                && distinct < data_rows.len())
            .then_some((idx, pm_csv_dimension_priority(&headers[idx], distinct)))
        })
        .collect::<Vec<_>>();
    dimension_candidates.sort_by_key(|(_, score)| *score);
    let dimension_indices = dimension_candidates
        .into_iter()
        .map(|(idx, _)| idx)
        .take(2)
        .collect::<Vec<_>>();
    let numeric_indices = all_numeric_indices
        .iter()
        .copied()
        .filter(|idx| !dimension_indices.contains(idx))
        .collect::<Vec<_>>();
    let mut numeric_indices = numeric_indices
        .into_iter()
        .map(|idx| {
            let stat = &numeric_stats[idx];
            (
                idx,
                pm_csv_metric_priority(&headers[idx], question, &|| {
                    (stat.count, stat.max - stat.min)
                }),
            )
        })
        .collect::<Vec<_>>();
    numeric_indices.sort_by_key(|(_, score)| *score);
    let numeric_indices = numeric_indices
        .into_iter()
        .map(|(idx, _)| idx)
        .take(10)
        .collect::<Vec<_>>();

    let numeric_summary_rows = numeric_indices
        .iter()
        .map(|idx| {
            let stat = &numeric_stats[*idx];
            vec![
                headers[*idx].clone(),
                stat.count.to_string(),
                pm_format_number(stat.sum),
                pm_format_number(stat.avg()),
                pm_format_number(stat.min),
                pm_format_number(stat.max),
            ]
        })
        .collect::<Vec<_>>();

    let mut sections = Vec::<String>::new();
    sections.push(format!(
        "#### CSV 数据画像: {filename}\n- 数据行数: {}\n- 字段数: {}\n- 字段列表: {}",
        data_rows.len(),
        headers.len(),
        headers.join(", ")
    ));
    if !numeric_summary_rows.is_empty() {
        sections.push(format!(
            "#### 数值字段概览\n{}",
            pm_markdown_table(
                &[
                    "字段".to_string(),
                    "非空数值数".to_string(),
                    "sum".to_string(),
                    "avg".to_string(),
                    "min".to_string(),
                    "max".to_string(),
                ],
                &numeric_summary_rows,
            )
        ));
    }

    for dim_idx in dimension_indices {
        let mut groups = BTreeMap::<String, (usize, Vec<NumStat>)>::new();
        for row in &data_rows {
            let key = row
                .get(dim_idx)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .unwrap_or("(blank)")
                .to_string();
            let entry = groups
                .entry(key)
                .or_insert_with(|| (0, vec![NumStat::default(); numeric_indices.len()]));
            entry.0 += 1;
            for (pos, numeric_idx) in numeric_indices.iter().enumerate() {
                if let Some(value) = row
                    .get(*numeric_idx)
                    .and_then(|raw| pm_parse_number(raw.as_str()))
                {
                    entry.1[pos].add(value);
                }
            }
        }
        let selected_numeric = numeric_indices.iter().take(10).copied().collect::<Vec<_>>();
        let mut table_headers = vec![headers[dim_idx].clone(), "rows".to_string()];
        for idx in &selected_numeric {
            table_headers.push(format!("sum({})", headers[*idx]));
            table_headers.push(format!("avg({})", headers[*idx]));
        }
        let mut rows = groups
            .into_iter()
            .map(|(key, (count, stats))| {
                let mut row = vec![key, count.to_string()];
                for pos in 0..selected_numeric.len() {
                    let stat = stats.get(pos).cloned().unwrap_or_default();
                    if stat.count == 0 {
                        row.push("n/a".to_string());
                        row.push("n/a".to_string());
                    } else {
                        row.push(pm_format_number(stat.sum));
                        row.push(pm_format_number(stat.avg()));
                    }
                }
                row
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            let left = a.get(1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            let right = b.get(1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            right.cmp(&left)
        });
        rows.truncate(8);
        sections.push(format!(
            "#### 按 `{}` 分组聚合\n{}",
            headers[dim_idx],
            pm_markdown_table(&table_headers, &rows)
        ));
    }

    let sample_count = data_rows.len().min(3);
    if sample_count > 0 {
        sections.push(format!(
            "#### 样例行（前 {} 行）\n{}",
            sample_count,
            pm_markdown_table(&headers, &data_rows[..sample_count])
        ));
    }

    Some(sections.join("\n\n"))
}

pub(super) async fn pm_read_task_document_text(
    data_dir: &FsPath,
    expected_user_id: &str,
    document: &PmTaskDocumentInput,
    max_bytes: usize,
    max_chars: usize,
) -> Result<(String, String, String, usize), String> {
    let (url_user_id, filename) =
        pm_parse_upload_asset_ref(&document.url).ok_or_else(|| "invalid upload url".to_string())?;
    if url_user_id != expected_user_id {
        return Err("upload url user mismatch".to_string());
    }
    let path = pm_upload_file_path(data_dir, &url_user_id, &filename);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|error| format!("read document metadata failed: {error}"))?;
    if usize::try_from(meta.len()).unwrap_or(usize::MAX) > max_bytes {
        return Err(format!(
            "document too large: {} bytes (max {max_bytes})",
            meta.len()
        ));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| format!("read document file failed: {error}"))?;
    let media_type = document
        .media_type
        .clone()
        .or_else(|| pm_infer_document_media_type_from_filename(&filename).map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    if !pm_is_supported_document_media_type(&media_type) {
        return Err(format!("unsupported document media type: {media_type}"));
    }
    let raw_text = pm_document_bytes_to_text(&media_type, &filename, &bytes)?;
    let normalized = raw_text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if normalized.is_empty() {
        return Err("document contains no extractable text".to_string());
    }
    let original_chars = normalized.chars().count();
    let text = normalized.chars().take(max_chars).collect::<String>();
    Ok((text, media_type, filename, original_chars))
}

pub(super) async fn build_pm_document_context_summary(
    data_dir: &FsPath,
    user_id: &str,
    input_context: &PmTaskInputContext,
    user_message: &str,
) -> Option<(String, serde_json::Value)> {
    if input_context.documents.is_empty() {
        return None;
    }
    let max_bytes = pm_task_document_max_bytes();
    let max_chars_per_file = pm_task_document_max_chars_per_file();
    let max_total_chars = pm_task_document_max_total_chars();
    let mut remaining_chars = max_total_chars;
    let mut sections = Vec::<String>::new();
    let mut accepted = Vec::<serde_json::Value>::new();
    let mut skipped = Vec::<serde_json::Value>::new();

    for (idx, document) in input_context.documents.iter().enumerate() {
        if remaining_chars == 0 {
            skipped.push(serde_json::json!({
                "index": idx + 1,
                "url": document.url,
                "reason": format!("total document chars exceeded max {max_total_chars}"),
            }));
            continue;
        }
        let per_file_limit = max_chars_per_file.min(remaining_chars);
        match pm_read_task_document_text(data_dir, user_id, document, max_bytes, per_file_limit)
            .await
        {
            Ok((text, media_type, filename, original_chars)) => {
                let csv_profile = if media_type.eq_ignore_ascii_case("text/csv")
                    || filename.to_ascii_lowercase().ends_with(".csv")
                {
                    pm_build_csv_profile(&filename, &text, user_message)
                } else {
                    None
                };
                let body = if let Some(profile) = csv_profile.as_ref() {
                    let raw_preview = text.lines().take(8).collect::<Vec<_>>().join("\n");
                    format!(
                        "{profile}\n\n#### 原始 CSV 预览（前 8 行，用于核对字段和样例）\n```csv\n{raw_preview}\n```"
                    )
                } else {
                    text.clone()
                };
                let extracted_chars = body.chars().count();
                remaining_chars = remaining_chars.saturating_sub(extracted_chars);
                let line_count = body.lines().count().max(1);
                sections.push(format!(
                    "### [file:{}] {}\n媒体类型: {}\n行号范围: 1-{}\n字符数: {}{}\n引用要求: 回答中引用本文件时使用 [file:{}]，必要时补充行号如 [file:{} L1-L{}]。\n\n{}",
                    idx + 1,
                    document.name.as_deref().unwrap_or(&filename),
                    media_type,
                    line_count,
                    extracted_chars,
                    if csv_profile.is_some() {
                        "（已结构化画像）"
                    } else if original_chars > extracted_chars {
                        "（已截取）"
                    } else {
                        ""
                    },
                    idx + 1,
                    idx + 1,
                    line_count,
                    body,
                ));
                accepted.push(serde_json::json!({
                    "index": idx + 1,
                    "citationId": format!("file:{}", idx + 1),
                    "filename": filename,
                    "name": document.name,
                    "mediaType": media_type,
                    "lineStart": 1,
                    "lineEnd": line_count,
                    "extractedChars": extracted_chars,
                    "originalChars": original_chars,
                    "truncated": original_chars > extracted_chars,
                    "structuredProfile": csv_profile.is_some(),
                }));
            }
            Err(reason) => skipped.push(serde_json::json!({
                "index": idx + 1,
                "url": document.url,
                "name": document.name,
                "mediaType": document.media_type,
                "reason": reason,
            })),
        }
    }

    if sections.is_empty() {
        let diag = serde_json::json!({
            "documentCount": input_context.documents.len(),
            "usableDocumentCount": 0,
            "usableDocuments": accepted,
            "skippedDocuments": skipped,
            "maxTotalChars": max_total_chars,
        });
        return Some((String::new(), diag));
    }

    let context = format!(
        "[附件文档上下文]\n以下内容由系统从用户上传的文档中提取。回答时必须引用具体文件标记（例如 [file:1] 或 [file:1 L3-L8]），不要编造未出现的信息。\n\n{}\n[/附件文档上下文]",
        sections.join("\n\n---\n\n")
    );
    let diag = serde_json::json!({
        "documentCount": input_context.documents.len(),
        "usableDocumentCount": accepted.len(),
        "usableDocuments": accepted,
        "skippedDocuments": skipped,
        "maxTotalChars": max_total_chars,
        "remainingChars": remaining_chars,
    });
    Some((context, diag))
}

pub(super) fn merge_pm_user_message_with_document_context(
    user_message: &str,
    document_context: &str,
) -> String {
    let base = if user_message.trim().is_empty() {
        "请基于上传文档完成分析并输出结论。".to_string()
    } else {
        user_message.trim().to_string()
    };
    if document_context.trim().is_empty() {
        base
    } else {
        format!("{base}\n\n{document_context}")
    }
}

fn push_image_summary_model_candidate(models: &mut Vec<String>, raw: Option<&str>) {
    let Some(model) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return;
    };
    if !models
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(model))
    {
        models.push(model.to_string());
    }
}

async fn resolve_pm_image_summary_key_candidates(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
) -> Result<Vec<agent_gateway::ApiKeyEntry>, AppError> {
    let config_registry = state
        .config_registry
        .as_ref()
        .ok_or_else(|| AppError::Internal("config registry not initialized".to_string()))?;

    let mut runtime_config = config_registry
        .load_user_config(tenant_id, user_id, Some("pm"))
        .await
        .map_err(|e| AppError::Internal(format!("failed to load PM runtime config: {e}")))?;

    if runtime_config.api_keys.is_empty() {
        return Err(AppError::ValidationError(
            "no chat API keys available; please configure a PM key".to_string(),
        ));
    }

    let pm_key_ids: HashSet<String> = sqlx::query_scalar(
        "SELECT id FROM api_keys
         WHERE tenant_id = ? AND enabled = 1 AND model_type = 'chat'
           AND EXISTS (SELECT 1 FROM json_each(COALESCE(scenarios, '[]')) WHERE json_each.value = ?)
         ORDER BY priority ASC, created_at ASC",
    )
    .bind(tenant_id)
    .bind("pm")
    .fetch_all(state.control_db())
    .await?
    .into_iter()
    .collect();

    let mut pm_scoped = Vec::new();
    let mut fallback = Vec::new();
    for entry in runtime_config.api_keys.drain(..) {
        if pm_key_ids.contains(&entry.id) {
            pm_scoped.push(entry);
        } else {
            fallback.push(entry);
        }
    }

    let mut candidates = Vec::with_capacity(pm_scoped.len() + fallback.len());
    candidates.extend(pm_scoped);
    candidates.extend(fallback);
    Ok(candidates)
}

pub(crate) async fn build_pm_task_image_summary(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_model: &str,
    user_message: &str,
    input_context: &PmTaskInputContext,
) -> Result<(String, serde_json::Value), String> {
    let max_bytes = pm_task_image_max_bytes();
    let max_total = pm_task_image_max_total_bytes();
    let mut total_loaded = 0usize;
    let mut image_blocks = Vec::<api::InputContentBlock>::new();
    let mut accepted_labels = Vec::<String>::new();
    let mut skipped = Vec::<serde_json::Value>::new();

    for (idx, image) in input_context.images.iter().enumerate() {
        match pm_read_task_image_bytes(&state.data_dir, user_id, image, max_bytes).await {
            Ok((bytes, media_type, filename)) => {
                if total_loaded.saturating_add(bytes.len()) > max_total {
                    skipped.push(serde_json::json!({
                        "index": idx + 1,
                        "url": image.url,
                        "reason": format!("total image bytes exceeded max {max_total}")
                    }));
                    continue;
                }
                total_loaded = total_loaded.saturating_add(bytes.len());
                image_blocks.push(api::InputContentBlock::Image {
                    media_type: media_type.clone(),
                    source_type: api::ImageSourceType::Base64,
                    data: BASE64.encode(bytes),
                });
                accepted_labels.push(format!("#{} {}", idx + 1, filename));
            }
            Err(reason) => skipped.push(serde_json::json!({
                "index": idx + 1,
                "url": image.url,
                "reason": reason
            })),
        }
    }

    if image_blocks.is_empty() {
        return Err("no usable images after validation".to_string());
    }

    let summary_model = pm_task_image_summary_model(session_model);
    let requested_model = summary_model.trim();
    if requested_model.is_empty() {
        return Err("image summary model cannot be empty".to_string());
    }
    let key_candidates = resolve_pm_image_summary_key_candidates(state, tenant_id, user_id)
        .await
        .map_err(|error| error.to_string())?;

    let question = if user_message.trim().is_empty() {
        "请基于上传图片提炼关键事实并输出结论。".to_string()
    } else {
        user_message.trim().to_string()
    };
    let mut content = Vec::<api::InputContentBlock>::new();
    content.push(api::InputContentBlock::Text {
        text: format!(
            "你是产运分析助手，请基于图片内容做事实摘要。\n用户问题：{question}\n输出要求：\n1) 按图片编号给出可见事实；\n2) 给出跨图综合结论；\n3) 单列不确定项；\n4) 不要编造不可见信息。"
        ),
    });
    content.extend(image_blocks);
    let timeout_secs = pm_task_image_summary_timeout_secs();
    let mut summary: Option<String> = None;
    let mut used_provider = "env-fallback".to_string();
    let mut used_model = requested_model.to_string();
    let mut used_api_key_id = "env-fallback".to_string();
    let mut attempt_logs = Vec::<serde_json::Value>::new();
    let mut attempted_pairs = HashSet::<String>::new();
    let mut last_error: Option<String> = None;

    for entry in &key_candidates {
        let mut model_candidates = Vec::<String>::new();
        push_image_summary_model_candidate(&mut model_candidates, Some(requested_model));
        push_image_summary_model_candidate(&mut model_candidates, entry.model.as_deref());

        for candidate_model in model_candidates {
            let pair_key = format!("{}::{candidate_model}", entry.id);
            if !attempted_pairs.insert(pair_key) {
                continue;
            }

            let provider = match api::build_provider(
                &entry.provider,
                &candidate_model,
                &entry.key,
                entry.base_url.as_deref(),
            ) {
                Ok(client) => client,
                Err(error) => {
                    let msg = format!(
                        "provider init failed (key={}, provider={}, model={}): {error}",
                        entry.id, entry.provider, candidate_model
                    );
                    last_error = Some(msg.clone());
                    attempt_logs.push(serde_json::json!({
                        "keyId": entry.id,
                        "provider": entry.provider,
                        "model": candidate_model,
                        "status": "init_failed",
                        "error": msg,
                    }));
                    continue;
                }
            };

            let api_req = api::MessageRequest {
                model: candidate_model.clone(),
                max_tokens: pm_task_image_summary_max_tokens(),
                messages: vec![api::InputMessage {
                    role: "user".to_string(),
                    content: content.clone(),
                }],
                system: Some(
                    "You are a careful vision analyst. Keep outputs concise, factual, and uncertainty-aware."
                        .to_string(),
                ),
                tools: None,
                tool_choice: None,
                stream: false,
                temperature: None,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
                stop: None,
                reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
            };
            let response = match timeout(
                Duration::from_secs(timeout_secs),
                provider.send_message(&api_req),
            )
            .await
            {
                Ok(Ok(resp)) => resp,
                Ok(Err(error)) => {
                    let msg = format!("image summary model call failed: {error}");
                    last_error = Some(msg.clone());
                    attempt_logs.push(serde_json::json!({
                        "keyId": entry.id,
                        "provider": entry.provider,
                        "model": candidate_model,
                        "status": "call_failed",
                        "error": msg,
                    }));
                    continue;
                }
                Err(_) => {
                    let msg = format!("image summary model call timed out after {timeout_secs}s");
                    last_error = Some(msg.clone());
                    attempt_logs.push(serde_json::json!({
                        "keyId": entry.id,
                        "provider": entry.provider,
                        "model": candidate_model,
                        "status": "timeout",
                        "error": msg,
                    }));
                    continue;
                }
            };
            let candidate_summary = response
                .content
                .iter()
                .filter_map(|block| match block {
                    api::OutputContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string();
            if candidate_summary.is_empty() {
                let msg = "image summary model returned empty text".to_string();
                last_error = Some(msg.clone());
                attempt_logs.push(serde_json::json!({
                    "keyId": entry.id,
                    "provider": entry.provider,
                    "model": candidate_model,
                    "status": "empty",
                    "error": msg,
                }));
                continue;
            }
            if summary_text_indicates_no_vision(&candidate_summary) {
                let msg = format!(
                    "image summary indicates no vision capability (model={candidate_model})"
                );
                last_error = Some(msg.clone());
                attempt_logs.push(serde_json::json!({
                    "keyId": entry.id,
                    "provider": entry.provider,
                    "model": candidate_model,
                    "status": "no_vision",
                    "error": msg,
                }));
                continue;
            }

            used_provider = entry.provider.clone();
            used_model = candidate_model;
            used_api_key_id = entry.id.clone();
            summary = Some(candidate_summary);
            attempt_logs.push(serde_json::json!({
                "keyId": entry.id,
                "provider": entry.provider,
                "model": used_model.clone(),
                "status": "ok",
            }));
            break;
        }

        if summary.is_some() {
            break;
        }
    }

    if summary.is_none()
        && runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK")
    {
        let provider = api::ProviderClient::from_model(requested_model)
            .map_err(|error| format!("image summary env fallback init failed: {error}"))?;
        let api_req = api::MessageRequest {
            model: requested_model.to_string(),
            max_tokens: pm_task_image_summary_max_tokens(),
            messages: vec![api::InputMessage {
                role: "user".to_string(),
                content: content.clone(),
            }],
            system: Some(
                "You are a careful vision analyst. Keep outputs concise, factual, and uncertainty-aware."
                    .to_string(),
            ),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
        };
        let response = timeout(
            Duration::from_secs(timeout_secs),
            provider.send_message(&api_req),
        )
        .await
        .map_err(|_| format!("image summary model call timed out after {timeout_secs}s"))?
        .map_err(|error| format!("image summary model call failed: {error}"))?;
        let candidate_summary = response
            .content
            .iter()
            .filter_map(|block| match block {
                api::OutputContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();
        if candidate_summary.is_empty() {
            return Err(
                last_error.unwrap_or_else(|| "image summary model returned empty text".to_string())
            );
        }
        if summary_text_indicates_no_vision(&candidate_summary) {
            return Err(last_error.unwrap_or_else(|| {
                format!("image summary indicates no vision capability (model={requested_model})")
            }));
        }
        summary = Some(candidate_summary);
        used_provider = "env-fallback".to_string();
        used_model = requested_model.to_string();
        used_api_key_id = "env-fallback".to_string();
        attempt_logs.push(serde_json::json!({
            "keyId": "env-fallback",
            "provider": "env-fallback",
            "model": requested_model,
            "status": "ok",
        }));
    }

    let attempt_count = attempt_logs.len();
    let summary = summary.ok_or_else(|| {
        let base =
            last_error.unwrap_or_else(|| "image summary model candidates exhausted".to_string());
        format!("image summary failed after {attempt_count} attempts: {base}")
    })?;

    let diag = serde_json::json!({
        "imageCount": input_context.images.len(),
        "usableImageCount": accepted_labels.len(),
        "usableImages": accepted_labels,
        "skippedImages": skipped,
        "summaryChars": summary.chars().count(),
        "summaryModel": used_model,
        "summaryModelRequested": summary_model,
        "summaryProvider": used_provider,
        "summaryApiKeyId": used_api_key_id,
        "loadedBytesTotal": total_loaded,
        "summaryAttempts": attempt_logs,
    });
    Ok((summary, diag))
}

fn merge_pm_user_message_with_image_summary(
    user_message: &str,
    image_summary: &str,
    image_count: usize,
) -> String {
    let base = if user_message.trim().is_empty() {
        "请基于上传图片完成分析并输出结论。".to_string()
    } else {
        user_message.trim().to_string()
    };
    let capped_summary: String = image_summary.chars().take(5000).collect();
    format!(
        "{base}\n\n[图片理解上下文]\n图片数量: {image_count}\n以下内容由系统对用户上传图片提炼，可作为回答依据：\n{capped_summary}\n[/图片理解上下文]"
    )
}

fn pm_notification_preview(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

fn spawn_pm_answer_completed_notification(
    state: AppState,
    tenant_id: String,
    task_id: String,
    question: String,
    answer_text: String,
) {
    let question_preview = pm_notification_preview(&question, 800);
    let answer_preview =
        pm_notification_preview(&extract_pm_visible_answer_text(&answer_text), 3500);
    let text = format!(
        "产运助手问答已完成\n\n任务ID: {task_id}\n\n问题:\n{question_preview}\n\n回答摘要:\n{answer_preview}"
    );
    tokio::spawn(async move {
        if !crate::routes::task_control_worker::legacy_capability_notifications_enabled(
            &state, &tenant_id,
        )
        .await
        {
            return;
        }
        match crate::routes::bot_agents_outbound::notify_capability_event(
            &state,
            &tenant_id,
            "pm_assistant",
            "pm_assistant.answer_completed",
            crate::routes::bot_agents_outbound::BotOutboundMessage {
                title: Some("产运助手问答完成".to_string()),
                text,
                external_conversation_id: None,
            },
        )
        .await
        {
            Ok(summary) if summary.attempted > 0 => {
                tracing::info!(
                    tenant_id = %tenant_id,
                    task_id = %task_id,
                    attempted = summary.attempted,
                    sent = summary.sent,
                    failed = summary.failed,
                    "pm answer completion bot notification dispatched"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    task_id = %task_id,
                    "pm answer completion bot notification skipped: {}",
                    error
                );
            }
        }
    });
}

fn trigger_pm_runtime_cycle(state: AppState, task_id: String, reason: &'static str) {
    pm_background_worker_runtime().spawn(async move {
        if let Err(error) = run_pm_background_runtime_cycle(&state).await {
            tracing::warn!(
                task_id = %task_id,
                error = %error,
                reason,
                "trigger pm runtime cycle failed"
            );
        }
    });
}

fn trigger_pm_runtime_cycle_after_task_release(state: AppState, task_id: String) {
    trigger_pm_runtime_cycle(state, task_id, "task_release");
}

fn pm_followup_needs_previous_answer_context(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return false;
    }
    let normalized = trimmed.to_ascii_lowercase();
    let chars = trimmed.chars().count();
    let explicit_context_markers = [
        "上面",
        "上述",
        "上一条",
        "刚才",
        "前面",
        "上一轮",
        "前一条",
        "前文",
        "这段",
        "这份",
        "这个回答",
        "above",
        "previous",
        "last answer",
        "previous answer",
        "that answer",
        "this answer",
        "the above",
    ];
    if explicit_context_markers
        .iter()
        .any(|marker| normalized.contains(marker) || trimmed.contains(marker))
    {
        return true;
    }
    if pm_followup_message_has_explicit_transform_payload(trimmed) {
        return false;
    }

    let transform_only_markers = [
        "翻译成",
        "译成",
        "总结一下",
        "概括一下",
        "整理一下",
        "改写成",
        "润色一下",
        "扩写一下",
        "精简一下",
        "translate it",
        "translate this",
        "summarize it",
        "summarize this",
        "rewrite it",
        "rewrite this",
        "polish it",
        "polish this",
        "make it",
        "turn it into",
    ];
    if chars <= 80
        && transform_only_markers
            .iter()
            .any(|marker| normalized.contains(marker) || trimmed.contains(marker))
    {
        return true;
    }

    let bare_followups = [
        "翻译",
        "总结",
        "概括",
        "整理",
        "改写",
        "润色",
        "扩写",
        "精简",
        "中文",
        "英文",
        "english",
        "translate",
        "summarize",
        "summary",
        "rewrite",
        "polish",
    ];
    chars <= 24
        && bare_followups
            .iter()
            .any(|marker| normalized == marker.to_ascii_lowercase() || trimmed == *marker)
}

fn pm_followup_message_has_explicit_transform_payload(message: &str) -> bool {
    let trimmed = message.trim();
    let lower = trimmed.to_ascii_lowercase();
    let has_transform_intent = [
        "翻译",
        "译成",
        "总结",
        "概括",
        "整理",
        "改写",
        "润色",
        "translate",
        "summarize",
        "rewrite",
        "polish",
    ]
    .iter()
    .any(|marker| lower.contains(&marker.to_ascii_lowercase()) || trimmed.contains(marker));
    if !has_transform_intent {
        return false;
    }

    if ["“", "”", "‘", "’", "\"", "'", "`", "```", "：", ":", "\n"]
        .iter()
        .any(|marker| trimmed.contains(marker))
    {
        return true;
    }

    for verb in ["翻译", "译成", "总结", "概括", "整理", "改写", "润色"] {
        if let (Some(start), Some(end)) = (trimmed.find('把'), trimmed.find(verb)) {
            if start < end {
                let payload = trimmed[start + '把'.len_utf8()..end].trim();
                let meaningful_chars = payload
                    .chars()
                    .filter(|ch| !ch.is_whitespace() && !"，。,.、；;：:".contains(*ch))
                    .count();
                if meaningful_chars >= 2 {
                    return true;
                }
            }
        }
    }

    if lower.starts_with("translate ") && lower.contains(" into ") {
        let source = lower
            .strip_prefix("translate ")
            .and_then(|rest| rest.split(" into ").next())
            .unwrap_or("")
            .trim();
        if !matches!(source, "" | "it" | "this" | "that" | "above" | "the above") {
            return true;
        }
    }

    false
}

pub(super) async fn build_pm_effective_user_message_for_task(
    db: &sqlx::SqlitePool,
    session_id: &str,
    tenant_id: &str,
    user_id: &str,
    execution_message: &str,
    visible_message: &str,
) -> (String, Option<serde_json::Value>, Option<String>) {
    let trimmed = visible_message.trim();
    let mut memory_artifact = None;
    let mut memory_instruction = None;
    let effective = if pm_followup_needs_previous_answer_context(trimmed) {
        match load_latest_completed_pm_task_answer_from_db(db, session_id, tenant_id, user_id).await
        {
            Ok(Some(previous_answer)) => {
                let previous_answer: String = previous_answer.chars().take(24_000).collect();
                format!(
                    "{PM_ORCH_INTERNAL_BEGIN}\n\
FOLLOWUP_CONTEXT: The user's latest request depends on the immediately previous PM answer.\n\
Use the previous answer below as the source content. Do not ask the user to resend it.\n\
Keep the visible response focused on satisfying the latest request.\n\
{PM_ORCH_INTERNAL_END}\n\n\
User question: {trimmed}\n\n\
Previous answer to transform/reference:\n{previous_answer}"
                )
            }
            Ok(None) => execution_message.to_string(),
            Err(error) => {
                tracing::warn!(
                    session_id,
                    tenant_id,
                    user_id,
                    error = %error,
                    "load latest completed pm answer failed for follow-up context"
                );
                execution_message.to_string()
            }
        }
    } else {
        execution_message.to_string()
    };

    let thread_memory_state = crate::routes::memory_continuity::get_thread_memory_state(
        db, tenant_id, user_id, session_id,
    )
    .await;

    let mut memory_prompts = Vec::<String>::new();
    let mut memory_artifacts = Vec::<serde_json::Value>::new();

    if let Some((memory_prompt, artifact)) =
        crate::routes::memory_continuity::build_unified_memory_prompt(
            db,
            tenant_id,
            user_id,
            Some(session_id),
            "pm",
            visible_message,
            "auto",
        )
        .await
    {
        memory_prompts.push(memory_prompt);
        memory_artifacts.push(serde_json::json!({
            "kind": "unified_memory",
            "artifact": artifact,
        }));
    }

    if thread_memory_state.use_memories && thread_memory_state.pollution_state != "disabled" {
        if let Some((memory_prompt, artifact)) =
            build_pm_session_memory_prompt(db, tenant_id, user_id, session_id, visible_message)
                .await
        {
            memory_prompts.push(memory_prompt);
            memory_artifacts.push(serde_json::json!({
                "kind": "pm_session_memory",
                "artifact": artifact,
            }));
        }
    }

    if thread_memory_state.use_memories && thread_memory_state.pollution_state != "disabled" {
        if let Some((recent_prompt, artifact)) = build_pm_recent_history_context_prompt(
            db,
            tenant_id,
            user_id,
            session_id,
            visible_message,
        )
        .await
        {
            memory_prompts.push(recent_prompt);
            memory_artifacts.push(serde_json::json!({
                "kind": "recent_history_context",
                "artifact": artifact,
            }));
        }
    }

    if !memory_prompts.is_empty() {
        memory_instruction = Some(memory_prompts.join("\n\n"));
        memory_artifact = Some(serde_json::json!({
            "mode": "pm_combined_memory_context",
            "count": memory_artifacts.len(),
            "sources": memory_artifacts,
        }));
    }

    (effective, memory_artifact, memory_instruction)
}

async fn wait_pm_research_task_cancel(
    db: sqlx::SqlitePool,
    task_id: String,
    manager: PmResearchTaskManager,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if manager.is_cancel_requested(&task_id).await {
            return;
        }
        let cancel_requested = sqlx::query_scalar::<_, bool>(
            "SELECT cancel_requested
             FROM pm_research_tasks
             WHERE task_id = ?
             LIMIT 1",
        )
        .bind(&task_id)
        .fetch_optional(&db)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
        if cancel_requested {
            return;
        }
    }
}

async fn run_pm_background_last_chance_answer(
    manager: Arc<AgentSessionManager>,
    tenant_id: &str,
    user_id: &str,
    model: &str,
    session_id: &str,
    session_source: &str,
    user_message: &str,
    reason: &str,
    preserved_answer: &str,
) -> Result<TurnResult, GatewayError> {
    let handle = manager.get_session(session_id).await;
    let model_hint = if model.trim().is_empty() {
        handle
            .as_ref()
            .map(|session| session.model.as_str())
            .filter(|value| !value.trim().is_empty())
    } else {
        Some(model.trim())
    };
    let transient_session = match manager
        .create_session(
            user_id,
            tenant_id,
            None,
            model_hint,
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            Some("pm"),
            None,
            None,
        )
        .await
    {
        Ok(session) => session,
        Err(error) if !preserved_answer.trim().is_empty() => {
            tracing::warn!(
                session_id,
                error = %error,
                preserved_chars = preserved_answer.chars().count(),
                "last-chance session creation failed; delivering preserved streamed answer"
            );
            return Ok(build_pm_background_preserved_turn(
                session_id,
                model_hint.unwrap_or("pm-preserved-partial"),
                preserved_answer.to_string(),
            ));
        }
        Err(error) => {
            return Err(GatewayError::RuntimeExecution(format!(
                "create transient last-chance PM session failed: {error}"
            )));
        }
    };
    let transient_session_id = transient_session.session_id.clone();
    let transient_session_guard =
        PmTransientSessionGuard::new(manager.clone(), transient_session_id.clone());
    let prompt = wrap_pm_research_prompt(
        session_source,
        if preserved_answer.trim().is_empty() {
            build_pm_expert_only_final_prompt(
            user_message,
            "No admitted external evidence is guaranteed available for this terminal recovery. Use the user's first-party report as the primary evidence and produce a complete decision-grade PM/operations answer.",
            reason,
            1,
        )
        } else {
            build_pm_synthesis_continuation_prompt(
            user_message,
            "Complete the remaining decision-grade conclusions from the user question and the already emitted report. Do not invent unavailable external evidence.",
            preserved_answer,
        )
        },
    );
    let timeout_secs = pm_env_u64("PM_BACKGROUND_LAST_CHANCE_TIMEOUT_SECS", 90).clamp(30, 180);
    let mut options = agent_gateway::AgentTurnOptions {
        reasoning_budget: agent_gateway::InternalReasoningBudget::Fast,
        suppress_native_web_search: true,
        prefer_native_web_search: false,
        stream_timeout_secs: Some(timeout_secs),
        disable_tools: true,
        disable_provider_thinking: true,
        ..agent_gateway::AgentTurnOptions::default()
    };
    options
        .blocked_tools
        .extend(pm_blocked_non_search_research_tools());
    options.blocked_tools.extend(
        [
            "WebSearch",
            "WebFetch",
            "ToolSearch",
            "ListMcpResources",
            "ReadMcpResource",
        ]
        .iter()
        .map(|tool| (*tool).to_string()),
    );
    options.blocked_tools.sort();
    options.blocked_tools.dedup();
    let (result, captured_continuation) =
        run_pm_user_visible_answer_streaming_turn_preserving_partial(
            manager.clone(),
            transient_session_id.clone(),
            prompt,
            timeout_secs,
            "background last-chance expert synthesis turn",
            options,
            |_| {},
        )
        .await;
    transient_session_guard.finish().await;
    match result {
        Ok(mut turn) => {
            turn.session_id = session_id.to_string();
            if !preserved_answer.trim().is_empty() {
                turn.text = merge_pm_streamed_answer_parts(preserved_answer, &turn.text);
            }
            Ok(turn)
        }
        Err(error)
            if !preserved_answer.trim().is_empty() || !captured_continuation.trim().is_empty() =>
        {
            tracing::warn!(
                session_id,
                error = %error,
                preserved_chars = preserved_answer.chars().count(),
                continuation_chars = captured_continuation.chars().count(),
                "last-chance turn failed after visible output; delivering all preserved text"
            );
            Ok(build_pm_background_preserved_turn(
                session_id,
                model_hint.unwrap_or("pm-preserved-partial"),
                merge_pm_streamed_answer_parts(preserved_answer, &captured_continuation),
            ))
        }
        Err(error) => Err(error),
    }
}

fn build_pm_background_preserved_turn(session_id: &str, model: &str, text: String) -> TurnResult {
    TurnResult {
        session_id: session_id.to_string(),
        text,
        thinking: None,
        tool_calls: Vec::new(),
        usage: TokenUsageRecord {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            total_tokens: 0,
            estimated_cost_usd: 0.0,
            model: model.to_string(),
        },
        compacted: None,
        iterations: 1,
        metadata: None,
        hot_reloaded: false,
    }
}

fn best_pm_captured_answer(parts: &HashMap<String, String>) -> String {
    if let Some(final_candidate) = parts
        .get("final_candidate")
        .filter(|text| !text.trim().is_empty())
    {
        return final_candidate.clone();
    }
    let primary = parts.get("synthesize").map(String::as_str).unwrap_or("");
    let continuation = parts
        .get("synthesize_continuation")
        .map(String::as_str)
        .unwrap_or("");
    let synthesized = merge_pm_streamed_answer_parts(primary, continuation);
    let mut candidates = vec![synthesized];
    for stage in [
        "last_chance_synthesize",
        "shared_chat",
        "shared_chat_fallback",
        "shared_chat_scratch",
    ] {
        if let Some(text) = parts.get(stage) {
            candidates.push(text.clone());
        }
    }
    candidates
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .max_by_key(|text| text.chars().count())
        .unwrap_or_default()
}

fn preserve_pm_background_visible_answer(
    turn: &mut TurnResult,
    quality: &mut PmAnswerQualityDto,
    preserved_streamed_answer: &str,
) -> bool {
    let has_answer_text = !turn.text.trim().is_empty();
    let recovered_streamed_answer =
        !has_answer_text && !preserved_streamed_answer.trim().is_empty();
    if recovered_streamed_answer {
        turn.text = preserved_streamed_answer.to_string();
        *quality = degrade_pm_quality_with_reason(
            evaluate_pm_answer_quality(turn),
            "quality_gate_preserved_streamed_answer",
            "The completed model turn was unavailable, so AOS delivered every user-visible token captured during streaming.",
        );
        quality.deliverable = true;
    } else if has_answer_text && !pm_is_soft_deliverable_quality(quality) {
        // Quality checks can annotate a report, but never replace visible
        // model output that has already reached the user-facing stream.
        *quality = degrade_pm_quality_with_reason(
            quality.clone(),
            "quality_gate_preserved_model_answer",
            "AOS preserved the complete model answer and marked unresolved quality issues instead of replacing its contents.",
        );
        quality.deliverable = true;
    }
    recovered_streamed_answer
}

async fn complete_pm_background_recovery_with_turn(
    db: &sqlx::SqlitePool,
    telemetry: &PmTelemetrySink,
    manager: &Arc<AgentSessionManager>,
    task_manager: &PmResearchTaskManager,
    task_id: &str,
    claims: &Claims,
    session_id: &str,
    user_message: &str,
    mut turn: TurnResult,
    mut quality: PmAnswerQualityDto,
    reason: &str,
    recovery_mode: &str,
) {
    turn.session_id = session_id.to_string();
    let quality_score = pm_quality_delivery_score(&quality);
    persist_pm_run_finish(
        db,
        task_id,
        &PmRunFinishPayload {
            status: "completed".to_string(),
            current_stage: Some("synthesize".to_string()),
            attempt: Some(turn.iterations.max(1)),
            total_elapsed_ms: None,
            error_code: Some(format!("{recovery_mode}_completed")),
            error_message: Some(reason.to_string()),
            final_quality_score: Some(quality_score),
            metadata: Some(serde_json::json!({
                "backgroundRecovery": true,
                "answerTextPresent": !turn.text.trim().is_empty(),
                "responseMode": "llm_reply",
                "recoveryMode": recovery_mode,
                "degradedDelivery": !quality.passed,
                "citationCount": quality.citation_count,
                "domainCount": quality.domain_count,
            })),
        },
    )
    .await;
    persist_pm_session_summary_after_turn(
        db,
        &claims.tenant_id,
        &claims.sub,
        session_id,
        task_id,
        user_message,
        &turn.text,
        &quality,
        turn.compacted.as_ref(),
    )
    .await;
    crate::routes::memory_continuity::record_memory_tool_citations(
        db,
        &claims.tenant_id,
        &claims.sub,
        session_id,
        Some(task_id),
        &turn.tool_calls,
    )
    .await;
    crate::routes::memory_continuity::persist_agent_turn_exact_archive(
        db,
        &claims.tenant_id,
        &claims.sub,
        session_id,
        "pm",
        task_id,
        user_message,
        None,
        &turn.text,
        &turn.tool_calls,
        Some(serde_json::json!({
            "source": "pm_background_recovery",
            "taskId": task_id,
            "recoveryMode": recovery_mode,
            "model": turn.usage.model.clone(),
            "iterations": turn.iterations
        })),
    )
    .await;
    quality.deliverable = true;
    let response = turn_to_run_turn_response(turn, Some(quality), Some(user_message));
    persist_pm_terminal_answer_to_runtime_session(
        manager,
        &claims.tenant_id,
        &claims.sub,
        session_id,
        task_id,
        &response.text,
    )
    .await;
    task_manager
        .publish_completed(db, telemetry, task_id, response)
        .await;
}

pub(super) fn pm_task_archival_user_message(
    input_context: Option<&PmTaskInputContext>,
    execution_message: &str,
) -> String {
    input_context
        .and_then(|context| context.visible_message.as_deref())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| execution_message.trim())
        .to_string()
}

fn pm_runtime_assistant_text(message: &runtime::ConversationMessage) -> Option<String> {
    if message.role != runtime::MessageRole::Assistant {
        return None;
    }
    let text = message
        .blocks
        .iter()
        .filter_map(|block| match block {
            runtime::ContentBlock::Text { text } => Some(text.trim()),
            runtime::ContentBlock::ToolUse { .. } | runtime::ContentBlock::ToolResult { .. } => {
                None
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn pm_runtime_session_has_assistant_text(session: &runtime::Session, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return false;
    }
    for message in session.messages.iter().rev() {
        match message.role {
            runtime::MessageRole::Assistant => {
                return pm_runtime_assistant_text(message)
                    .is_some_and(|text| text.trim() == expected);
            }
            runtime::MessageRole::User => return false,
            runtime::MessageRole::System | runtime::MessageRole::Tool => {}
        }
    }
    false
}

fn pm_session_owns_terminal_answer(source: &str) -> bool {
    !matches!(
        source.trim().to_ascii_lowercase().as_str(),
        "super_assistant" | "super-assistant"
    )
}

pub(super) async fn persist_pm_terminal_answer_to_runtime_session(
    manager: &Arc<AgentSessionManager>,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    task_id: &str,
    answer_text: &str,
) {
    let visible_text = extract_pm_visible_answer_text(answer_text);
    let visible_text = visible_text.trim();
    if visible_text.is_empty() {
        return;
    }

    if manager
        .get_session(session_id)
        .await
        .as_ref()
        .is_some_and(|handle| !pm_session_owns_terminal_answer(&handle.source))
    {
        return;
    }

    if manager
        .get_session_messages(session_id, Some(tenant_id), Some(user_id))
        .await
        .as_ref()
        .is_some_and(|session| pm_runtime_session_has_assistant_text(session, visible_text))
    {
        return;
    }

    if let Err(error) = manager
        .append_visible_message_when_idle(
            session_id,
            tenant_id,
            user_id,
            runtime::MessageRole::Assistant,
            visible_text.to_string(),
            Duration::from_secs(120),
        )
        .await
    {
        tracing::warn!(
            task_id,
            session_id,
            tenant_id,
            user_id,
            error = %error,
            "failed to persist PM terminal answer to parent runtime session"
        );
    }
}

fn spawn_pm_task_heartbeat(
    db: sqlx::SqlitePool,
    task_id: String,
    worker_id: String,
    cfg: PmResearchTaskConfig,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let lease_secs = cfg.lease_secs;
    let heartbeat_interval = cfg.heartbeat_interval;
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(heartbeat_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    match touch_pm_task_lease(&db, &task_id, &worker_id, lease_secs).await {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::info!(
                                task_id = %task_id,
                                worker_id = %worker_id,
                                "pm task heartbeat stopped because lease is no longer renewable"
                            );
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(
                                task_id = %task_id,
                                worker_id = %worker_id,
                                error = %error,
                                "pm task heartbeat failed to renew lease"
                            );
                        }
                    }
                    pm_research_task_manager()
                        .publish_heartbeat(&db, &task_id)
                        .await;
                }
            }
        }
    });
    (shutdown_tx, handle)
}

async fn stop_pm_task_heartbeat(
    heartbeat_shutdown_tx: tokio::sync::watch::Sender<bool>,
    heartbeat_handle: tokio::task::JoinHandle<()>,
) {
    let _ = heartbeat_shutdown_tx.send(true);
    match tokio::time::timeout(Duration::from_secs(5), heartbeat_handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(
                error = %error,
                "pm task heartbeat worker exited with join error"
            );
        }
        Err(_) => {
            tracing::warn!("pm task heartbeat worker did not stop within 5s");
        }
    }
}

pub(super) fn spawn_pm_research_task(
    state: AppState,
    claims: Claims,
    task_id: String,
    session_id: String,
    user_message: String,
    input_context: Option<PmTaskInputContext>,
    session_source: String,
    session_model: String,
    run_slot: PmResearchRunPermit,
    resume_checkpoint: Option<PmResumeCheckpoint>,
) {
    tracing::info!(
        task_id = %task_id,
        session_id = %session_id,
        tenant_id = %claims.tenant_id,
        user_id = %claims.sub,
        "submitting PM research task to dedicated PM background worker runtime"
    );
    pm_background_worker_runtime().spawn(run_pm_research_task_worker(
        state,
        claims,
        task_id,
        session_id,
        user_message,
        input_context,
        session_source,
        session_model,
        run_slot,
        resume_checkpoint,
    ));
}

#[allow(clippy::too_many_arguments)]
async fn run_pm_research_task_worker(
    state: AppState,
    claims: Claims,
    task_id: String,
    session_id: String,
    user_message: String,
    input_context: Option<PmTaskInputContext>,
    session_source: String,
    session_model: String,
    run_slot: PmResearchRunPermit,
    resume_checkpoint: Option<PmResumeCheckpoint>,
) {
    let panic_db = state.control_db.clone();
    let panic_task_id = task_id.clone();
    let panic_session_id = session_id.clone();
    let panic_tenant_id = claims.tenant_id.clone();
    let panic_worker_id = pm_task_worker_id().to_string();
    let panic_cycle_state = state.clone();
    let panic_result = std::panic::AssertUnwindSafe(run_pm_research_task_inner(
        state,
        claims,
        task_id,
        session_id,
        user_message,
        input_context,
        session_source,
        session_model,
        run_slot,
        resume_checkpoint,
    ))
    .catch_unwind()
    .await;

    if let Err(panic_payload) = panic_result {
        let panic_msg = format_panic_payload(panic_payload.as_ref());
        tracing::error!(
            task_id = %panic_task_id,
            session_id = %panic_session_id,
            tenant_id = %panic_tenant_id,
            panic = %panic_msg,
            "pm background worker panicked; delivering local recovery answer"
        );
        let task_manager = pm_research_task_manager().clone();
        task_manager
            .mark_execution_active(&panic_task_id, false)
            .await;
        let recovery_reason = format!("pm worker panic: {panic_msg}");
        if let Err(error) = complete_pm_task_with_local_recovery(
            &panic_cycle_state,
            &panic_task_id,
            &recovery_reason,
            "worker_panic_local_first_party_synthesis",
        )
        .await
        {
            tracing::error!(
                task_id = %panic_task_id,
                session_id = %panic_session_id,
                tenant_id = %panic_tenant_id,
                error = %error,
                "pm background worker panic local recovery failed"
            );
        }
        let _ = release_pm_task_lease(&panic_db, &panic_task_id, &panic_worker_id, false).await;
        trigger_pm_runtime_cycle_after_task_release(panic_cycle_state, panic_task_id.clone());
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_pm_research_task_inner(
    state: AppState,
    claims: Claims,
    task_id: String,
    session_id: String,
    user_message: String,
    input_context: Option<PmTaskInputContext>,
    session_source: String,
    session_model: String,
    run_slot: PmResearchRunPermit,
    resume_checkpoint: Option<PmResumeCheckpoint>,
) {
    let _run_slot = run_slot;
    let manager = get_agent_manager(&state).clone();
    let task_manager = pm_research_task_manager().clone();
    let db = state.control_db.clone();
    let telemetry = state.pm_telemetry.clone();
    let task_runtime_cfg = *pm_task_research_config();
    let lease_worker_id = pm_task_worker_id().to_string();
    let archival_user_message =
        pm_task_archival_user_message(input_context.as_ref(), &user_message);
    let (heartbeat_shutdown_tx, heartbeat_handle) = spawn_pm_task_heartbeat(
        db.clone(),
        task_id.clone(),
        lease_worker_id.clone(),
        task_runtime_cfg,
    );

    if task_manager.is_cancel_requested(&task_id).await {
        task_manager
            .publish_cancelled(&db, &telemetry, &task_id)
            .await;
        let _ = release_pm_task_lease(&db, &task_id, &lease_worker_id, false).await;
        stop_pm_task_heartbeat(heartbeat_shutdown_tx, heartbeat_handle).await;
        trigger_pm_runtime_cycle_after_task_release(state.clone(), task_id.clone());
        return;
    }

    if let Some(checkpoint) = resume_checkpoint.as_ref() {
        task_manager
            .publish_stage(
                &db,
                &telemetry,
                &task_id,
                "resume",
                "completed",
                checkpoint.attempt,
                Some("已从最近 checkpoint 恢复执行"),
                Some(serde_json::json!({
                    "previousTaskId": checkpoint.previous_task_id,
                    "fromStatus": checkpoint.status,
                    "fromStage": checkpoint.stage,
                    "fromAttempt": checkpoint.attempt,
                })),
            )
            .await;
    }

    let (contextual_user_message, memory_artifact, memory_instruction) =
        build_pm_effective_user_message_for_task(
            &db,
            &session_id,
            &claims.tenant_id,
            &claims.sub,
            &user_message,
            &archival_user_message,
        )
        .await;
    let mut effective_user_message = if contextual_user_message.trim().is_empty() {
        "请基于上传附件进行分析并输出结论。".to_string()
    } else {
        contextual_user_message
    };
    if let Some(artifact) = memory_artifact.as_ref() {
        crate::routes::memory_continuity::record_memory_citations(
            &db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            Some(&task_id),
            artifact,
        )
        .await;
        task_manager
            .publish_stage(
                &db,
                &telemetry,
                &task_id,
                "memory",
                "completed",
                resume_checkpoint
                    .as_ref()
                    .map(|cp| cp.attempt)
                    .unwrap_or(1)
                    .max(1),
                Some("已加载产运助手会话记忆和压缩摘要"),
                Some(serde_json::json!({
                    "event": "memory.loaded",
                    "app": "pm",
                    "sessionId": session_id,
                    "memory": artifact,
                })),
            )
            .await;
        record_pm_audit_event(
            &db,
            &claims.tenant_id,
            &claims.sub,
            &task_id,
            "pm_session_memory_loaded",
            "info",
            "pm session memory injected into task context",
            Some(artifact),
        )
        .await;
    }
    if let Some(context) = input_context
        .as_ref()
        .filter(|ctx| !ctx.documents.is_empty())
    {
        crate::routes::memory_continuity::mark_thread_memory_polluted(
            &db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            "pm_document_context",
            Some(serde_json::json!({ "documentCount": context.documents.len() })),
        )
        .await;
        if let Some((document_context, diag)) = build_pm_document_context_summary(
            &state.data_dir,
            &claims.sub,
            context,
            &effective_user_message,
        )
        .await
        {
            if !document_context.trim().is_empty() {
                effective_user_message = merge_pm_user_message_with_document_context(
                    &effective_user_message,
                    &document_context,
                );
            }
            record_pm_audit_event(
                &db,
                &claims.tenant_id,
                &claims.sub,
                &task_id,
                "pm_task_documents_processed",
                "info",
                "pm task document context extracted",
                Some(&diag),
            )
            .await;
        }
    }
    if let Some(context) = input_context.as_ref().filter(|ctx| !ctx.images.is_empty()) {
        crate::routes::memory_continuity::mark_thread_memory_polluted(
            &db,
            &claims.tenant_id,
            &claims.sub,
            &session_id,
            "pm_image_context",
            Some(serde_json::json!({ "imageCount": context.images.len() })),
        )
        .await;
        match build_pm_task_image_summary(
            &state,
            &claims.tenant_id,
            &claims.sub,
            &session_model,
            &effective_user_message,
            context,
        )
        .await
        {
            Ok((summary, diag)) => {
                effective_user_message = merge_pm_user_message_with_image_summary(
                    &effective_user_message,
                    &summary,
                    context.images.len(),
                );
                record_pm_audit_event(
                    &db,
                    &claims.tenant_id,
                    &claims.sub,
                    &task_id,
                    "pm_task_images_processed",
                    "info",
                    "pm task image context summary generated",
                    Some(&diag),
                )
                .await;
            }
            Err(error) => {
                let error_text = error.to_string();
                let warning_message = pm_image_context_warning_message(&error_text);
                let warning_detail = serde_json::json!({
                    "imageContextWarning": {
                        "code": "image_context_summary_skipped",
                        "message": warning_message,
                        "detail": error_text,
                        "imageCount": context.images.len(),
                    }
                });
                let warning_attempt = resume_checkpoint.as_ref().map(|cp| cp.attempt).unwrap_or(1);
                task_manager
                    .publish_stage(
                        &db,
                        &telemetry,
                        &task_id,
                        "preflight",
                        "running",
                        warning_attempt.max(1),
                        Some(warning_message),
                        Some(warning_detail.clone()),
                    )
                    .await;
                record_pm_audit_event(
                    &db,
                    &claims.tenant_id,
                    &claims.sub,
                    &task_id,
                    "pm_task_images_process_failed",
                    "warn",
                    "pm task image context summary skipped",
                    Some(&serde_json::json!({
                        "imageCount": context.images.len(),
                        "error": error,
                        "warning": warning_detail
                    })),
                )
                .await;
            }
        }
    }

    let primary_message = wrap_pm_research_prompt(&session_source, effective_user_message.clone());
    let (deadline_budget, _) = resolve_pm_budget_snapshot(&db, &claims.tenant_id).await;
    let hard_deadline_secs = pm_background_task_deadline_secs(&deadline_budget);
    let task_id_for_answer_stream = task_id.clone();
    let telemetry_for_answer_stream = state.pm_telemetry.clone();
    let captured_answer_parts = Arc::new(std::sync::Mutex::new(HashMap::<String, String>::new()));
    let captured_answer_parts_for_callback = captured_answer_parts.clone();
    let (answer_delta_tx, mut answer_delta_rx) =
        tokio::sync::mpsc::channel::<(String, String)>(512);
    let mut answer_delta_worker = tokio::spawn(async move {
        let mut pending = None;
        loop {
            let Some((stage, mut delta)) =
                pending.take().or_else(|| answer_delta_rx.try_recv().ok())
            else {
                let Some(next) = answer_delta_rx.recv().await else {
                    break;
                };
                pending = Some(next);
                continue;
            };
            // Coalesce both bursty and slowly emitted token deltas. The short
            // window keeps recovery current without turning tokens into rows.
            let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
            while delta.len() < 16 * 1024 {
                match tokio::time::timeout_at(deadline, answer_delta_rx.recv()).await {
                    Ok(Some((next_stage, next_delta))) if next_stage == stage => {
                        delta.push_str(&next_delta);
                    }
                    Ok(Some(next)) => {
                        pending = Some(next);
                        break;
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            pm_research_task_manager()
                .publish_answer_delta(
                    &telemetry_for_answer_stream,
                    &task_id_for_answer_stream,
                    &stage,
                    delta,
                )
                .await;
        }
    });
    let answer_delta: PmAnswerDeltaCallback = Arc::new(move |stage: &str, delta: String| {
        if let Ok(mut captured) = captured_answer_parts_for_callback.lock() {
            captured
                .entry(stage.to_string())
                .or_default()
                .push_str(&delta);
        }
        if answer_delta_tx
            .try_send((stage.to_string(), delta))
            .is_err()
        {
            tracing::debug!(
                "PM answer recovery queue is full; final response remains authoritative"
            );
        }
    });
    let task_id_for_stage = task_id.clone();
    let (stage_tx, mut stage_rx) =
        tokio::sync::mpsc::channel::<(String, String, usize, Option<serde_json::Value>)>(256);
    let stage_checkpoint_db = db.clone();
    let stage_worker_telemetry = telemetry.clone();
    let stage_worker_task_id = task_id_for_stage.clone();
    let mut stage_worker = tokio::spawn(async move {
        while let Some((stage, status, attempt, detail)) = stage_rx.recv().await {
            let detail_for_stage = detail.clone();
            pm_research_task_manager()
                .publish_stage(
                    &stage_checkpoint_db,
                    &stage_worker_telemetry,
                    &stage_worker_task_id,
                    &stage,
                    &status,
                    attempt,
                    pm_stage_user_message(&stage, &status),
                    detail_for_stage,
                )
                .await;
            stage_worker_telemetry
                .enqueue(PmTelemetryEvent::stage_attempt(
                    &stage_worker_task_id,
                    &stage,
                    attempt,
                    &status,
                    detail,
                    None,
                    None,
                    None,
                    None,
                    None,
                ))
                .await;
        }
    });
    let stage_callback_tx = stage_tx.clone();
    let mut on_stage =
        move |stage: &str, status: &str, attempt: usize, detail: Option<serde_json::Value>| {
            if stage_callback_tx
                .try_send((stage.to_string(), status.to_string(), attempt, detail))
                .is_err()
            {
                tracing::warn!(stage, status, attempt, "PM stage persistence queue is full");
            }
        };
    let run_future = run_pm_orchestrated_turn(
        &state,
        manager.clone(),
        &db,
        &session_id,
        &session_source,
        &effective_user_message,
        primary_message,
        &claims.sub,
        &claims.tenant_id,
        &session_model,
        Some(&task_id),
        Some(&task_id),
        resume_checkpoint.as_ref(),
        memory_instruction,
        &mut on_stage,
        Some(answer_delta),
    );
    let cancel_future =
        wait_pm_research_task_cancel(db.clone(), task_id.clone(), task_manager.clone());
    let deadline_future = tokio::time::sleep(Duration::from_secs(hard_deadline_secs));
    let run_outcome = tokio::select! {
        outcome = run_future => outcome,
        _ = deadline_future => {
            tracing::warn!(
                task_id = %task_id,
                session_id = %session_id,
                tenant_id = %claims.tenant_id,
                "pm background research task reached hard deadline; forcing terminal synthesis"
            );
            if let Err(error) = manager.hot_reload_session(&session_id).await {
                tracing::warn!(
                    task_id = %task_id,
                    session_id = %session_id,
                    "pm deadline hot_reload_session failed: {}",
                    error
                );
            }
            Err(GatewayError::RuntimeExecution(
                "pm_research_task_deadline_elapsed".to_string(),
            ))
        }
        _ = cancel_future => {
            tracing::info!(
                task_id = %task_id,
                session_id = %session_id,
                tenant_id = %claims.tenant_id,
                "pm background research task interrupted by cancel request"
            );
            if let Err(error) = manager.cancel_running_turn(&session_id).await {
                tracing::warn!(
                    task_id = %task_id,
                    session_id = %session_id,
                    "pm cancel_running_turn failed: {}",
                    error
                );
            }
            if let Err(error) = manager.hot_reload_session(&session_id).await {
                tracing::warn!(
                    task_id = %task_id,
                    session_id = %session_id,
                    "pm cancel hot_reload_session failed: {}",
                    error
                );
            }
            Err(GatewayError::RuntimeExecution(
                "pm_research_task_cancelled".to_string(),
            ))
        }
    };

    // The orchestrator no longer owns either callback after `run_future`
    // finishes (or is cancelled). Close and drain their persistence queues so
    // stage order is deterministic and no late event races the terminal row.
    drop(on_stage);
    drop(stage_tx);
    let drain_timeout = Duration::from_secs(15);
    let (stage_drain, answer_drain) = tokio::join!(
        tokio::time::timeout(drain_timeout, &mut stage_worker),
        tokio::time::timeout(drain_timeout, &mut answer_delta_worker),
    );
    if stage_drain.is_err() {
        tracing::warn!(task_id = %task_id, "pm stage persistence drain timed out");
        stage_worker.abort();
    }
    if answer_drain.is_err() {
        tracing::warn!(task_id = %task_id, "pm answer-delta persistence drain timed out");
        answer_delta_worker.abort();
    }
    let preserved_streamed_answer = captured_answer_parts
        .lock()
        .map(|parts| best_pm_captured_answer(&parts))
        .unwrap_or_default();

    match run_outcome {
        Ok((mut turn, mut quality)) => {
            if task_manager.is_cancel_requested(&task_id).await {
                task_manager
                    .publish_cancelled(&db, &telemetry, &task_id)
                    .await;
                persist_pm_run_finish(
                    &db,
                    &task_id,
                    &PmRunFinishPayload {
                        status: "cancelled".to_string(),
                        current_stage: Some("cancelled".to_string()),
                        attempt: Some(quality.tool_call_count.max(1)),
                        total_elapsed_ms: None,
                        error_code: Some("task_cancelled".to_string()),
                        error_message: Some("pm research task cancelled".to_string()),
                        final_quality_score: None,
                        metadata: None,
                    },
                )
                .await;
                let _ = release_pm_task_lease(&db, &task_id, &lease_worker_id, false).await;
                stop_pm_task_heartbeat(heartbeat_shutdown_tx, heartbeat_handle).await;
                trigger_pm_runtime_cycle_after_task_release(state.clone(), task_id.clone());
                return;
            }
            let initial_has_answer_text = !turn.text.trim().is_empty();
            let initial_deliverable_for_status = pm_is_soft_deliverable_quality(&quality);
            let recovered_streamed_answer = preserve_pm_background_visible_answer(
                &mut turn,
                &mut quality,
                &preserved_streamed_answer,
            );
            if turn.text.trim().is_empty() {
                turn = build_pm_local_strategy_synthesis_turn(
                    &session_id,
                    &session_model,
                    &effective_user_message,
                    "background quality gate produced no deliverable answer",
                    turn.iterations.max(1),
                    &turn.tool_calls,
                );
                quality = degrade_pm_quality_with_reason(
                            evaluate_pm_answer_quality(&turn),
                            "quality_gate_local_first_party_synthesis",
                            "Background quality gate had no deliverable answer; delivered local first-party strategy synthesis.",
                        );
                quality.deliverable = true;
            }
            let quality_score = pm_quality_delivery_score(&quality);
            upsert_pm_route_learning_feature(
                &db,
                &claims.tenant_id,
                "background.pm",
                Some("search"),
                pm_is_deliverable_quality(&quality),
                quality_score,
                0.0,
                0.0,
            )
            .await;
            upsert_pm_route_bandit_state(
                &db,
                &claims.tenant_id,
                "background.pm",
                Some("search"),
                quality_score,
                0.05,
            )
            .await;
            let has_answer_text = !turn.text.trim().is_empty();
            let deliverable_for_status = pm_is_soft_deliverable_quality(&quality);
            let completion_ready = has_answer_text || deliverable_for_status;
            let degraded_delivery = completion_ready && !quality.passed;
            let response_mode = if quality.citation_count > 0 || quality.domain_count > 0 {
                "deep_summary"
            } else {
                "llm_reply"
            };
            persist_pm_run_finish(
                &db,
                &task_id,
                &PmRunFinishPayload {
                    status: "completed".to_string(),
                    current_stage: Some("synthesize".to_string()),
                    attempt: Some(turn.iterations.max(1)),
                    total_elapsed_ms: None,
                    error_code: if initial_has_answer_text && initial_deliverable_for_status {
                        None
                    } else if recovered_streamed_answer {
                        Some("streamed_answer_preserved".to_string())
                    } else {
                        Some("quality_gate_soft_delivered".to_string())
                    },
                    error_message: None,
                    final_quality_score: Some(quality_score),
                    metadata: Some(serde_json::json!({
                        "citationCount": quality.citation_count,
                        "domainCount": quality.domain_count,
                        "triadCoverage": quality.triad_coverage,
                        "degradedDelivery": degraded_delivery,
                        "answerTextPresent": has_answer_text,
                        "responseMode": response_mode
                    })),
                },
            )
            .await;
            crate::routes::memory_continuity::record_memory_tool_citations(
                &db,
                &claims.tenant_id,
                &claims.sub,
                &session_id,
                Some(&task_id),
                &turn.tool_calls,
            )
            .await;
            crate::routes::memory_continuity::persist_agent_turn_exact_archive(
                &db,
                &claims.tenant_id,
                &claims.sub,
                &session_id,
                "pm",
                &task_id,
                &archival_user_message,
                Some(&effective_user_message),
                &turn.text,
                &turn.tool_calls,
                Some(serde_json::json!({
                    "source": "pm_background_task",
                    "taskId": task_id,
                    "responseMode": response_mode,
                    "qualityScore": quality_score,
                    "qualityLevel": quality.quality_level,
                    "deliverable": quality.deliverable,
                    "degradedDelivery": degraded_delivery,
                    "model": turn.usage.model.clone(),
                    "iterations": turn.iterations
                })),
            )
            .await;
            let memory_state = crate::routes::memory_continuity::get_thread_memory_state(
                &db,
                &claims.tenant_id,
                &claims.sub,
                &session_id,
            )
            .await;
            if memory_state.generate_memories
                && memory_state.pollution_state != "polluted"
                && memory_state.pollution_state != "disabled"
            {
                persist_pm_session_memory_candidate(
                    &db,
                    &claims.tenant_id,
                    &claims.sub,
                    &session_id,
                    &archival_user_message,
                )
                .await;
                crate::routes::memory_continuity::persist_unified_memory_candidate(
                    &db,
                    &claims.tenant_id,
                    &claims.sub,
                    Some(&session_id),
                    "pm",
                    &archival_user_message,
                )
                .await;
            }
            persist_pm_session_summary_after_turn(
                &db,
                &claims.tenant_id,
                &claims.sub,
                &session_id,
                &task_id,
                &archival_user_message,
                &turn.text,
                &quality,
                turn.compacted.as_ref(),
            )
            .await;
            let response = turn_to_run_turn_response(
                turn,
                Some(quality.clone()),
                Some(&archival_user_message),
            );
            let notification_answer_text = response.text.clone();
            persist_pm_terminal_answer_to_runtime_session(
                &manager,
                &claims.tenant_id,
                &claims.sub,
                &session_id,
                &task_id,
                &response.text,
            )
            .await;
            task_manager
                .publish_completed(&db, &telemetry, &task_id, response)
                .await;
            spawn_pm_answer_completed_notification(
                state.clone(),
                claims.tenant_id.clone(),
                task_id.clone(),
                archival_user_message.clone(),
                notification_answer_text,
            );
        }
        Err(e) => {
            if e.to_string().contains("pm_research_task_cancelled")
                || task_manager.is_cancel_requested(&task_id).await
            {
                task_manager
                    .publish_cancelled(&db, &telemetry, &task_id)
                    .await;
                persist_pm_run_finish(
                    &db,
                    &task_id,
                    &PmRunFinishPayload {
                        status: "cancelled".to_string(),
                        current_stage: Some("cancelled".to_string()),
                        attempt: Some(1),
                        total_elapsed_ms: None,
                        error_code: Some("task_cancelled".to_string()),
                        error_message: Some("pm research task cancelled".to_string()),
                        final_quality_score: None,
                        metadata: None,
                    },
                )
                .await;
            } else if e.to_string().contains("pm_research_task_deadline_elapsed") {
                let reason = format!(
                    "background PM research exceeded hard deadline after {hard_deadline_secs}s"
                );
                let last_chance = run_pm_background_last_chance_answer(
                    manager.clone(),
                    &claims.tenant_id,
                    &claims.sub,
                    &session_model,
                    &session_id,
                    &session_source,
                    &effective_user_message,
                    &reason,
                    &preserved_streamed_answer,
                )
                .await;
                match last_chance {
                    Ok(turn) if !turn.text.trim().is_empty() => {
                        let quality = evaluate_pm_answer_quality(&turn);
                        complete_pm_background_recovery_with_turn(
                            &db,
                            &telemetry,
                            &manager,
                            &task_manager,
                            &task_id,
                            &claims,
                            &session_id,
                            &archival_user_message,
                            turn,
                            quality,
                            &reason,
                            "model_last_chance",
                        )
                        .await;
                    }
                    Ok(_) => {
                        let turn = build_pm_local_strategy_synthesis_turn(
                            &session_id,
                            &session_model,
                            &effective_user_message,
                            "deadline last-chance synthesis returned empty text",
                            1,
                            &[],
                        );
                        let quality = degrade_pm_quality_with_reason(
                                    evaluate_pm_answer_quality(&turn),
                                    "deadline_local_first_party_synthesis",
                                    "Background research reached its hard deadline; delivered a local first-party strategy synthesis.",
                                );
                        complete_pm_background_recovery_with_turn(
                            &db,
                            &telemetry,
                            &manager,
                            &task_manager,
                            &task_id,
                            &claims,
                            &session_id,
                            &archival_user_message,
                            turn,
                            quality,
                            &reason,
                            "local_first_party_empty_last_chance",
                        )
                        .await;
                    }
                    Err(error) => {
                        let turn = build_pm_local_strategy_synthesis_turn(
                            &session_id,
                            &session_model,
                            &effective_user_message,
                            &format!("deadline last-chance synthesis failed: {error}"),
                            1,
                            &[],
                        );
                        let quality = degrade_pm_quality_with_reason(
                                    evaluate_pm_answer_quality(&turn),
                                    "deadline_local_first_party_synthesis",
                                    "Background research reached its hard deadline; delivered a local first-party strategy synthesis.",
                                );
                        complete_pm_background_recovery_with_turn(
                            &db,
                            &telemetry,
                            &manager,
                            &task_manager,
                            &task_id,
                            &claims,
                            &session_id,
                            &archival_user_message,
                            turn,
                            quality,
                            &reason,
                            "local_first_party_last_chance_error",
                        )
                        .await;
                    }
                }
            } else {
                upsert_pm_provider_health(
                    &db,
                    &claims.tenant_id,
                    &session_model,
                    "model",
                    false,
                    None,
                    Some("runtime_error"),
                )
                .await;
                let reason = format!("background PM runtime failed: {e}");
                let (turn, quality, recovery_mode) = if preserved_streamed_answer.trim().is_empty()
                {
                    let turn = build_pm_local_strategy_synthesis_turn(
                        &session_id,
                        &session_model,
                        &effective_user_message,
                        &reason,
                        1,
                        &[],
                    );
                    let quality = degrade_pm_quality_with_reason(
                        evaluate_pm_answer_quality(&turn),
                        "runtime_local_first_party_synthesis",
                        "Background research runtime failed; delivered a local first-party strategy synthesis.",
                    );
                    (turn, quality, "local_first_party_runtime_error")
                } else {
                    let turn = build_pm_background_preserved_turn(
                        &session_id,
                        &session_model,
                        preserved_streamed_answer.clone(),
                    );
                    let quality = degrade_pm_quality_with_reason(
                        evaluate_pm_answer_quality(&turn),
                        "runtime_preserved_streamed_answer",
                        "Background research ended unexpectedly; AOS delivered every user-visible token captured before the failure.",
                    );
                    (turn, quality, "preserved_streamed_runtime_error")
                };
                complete_pm_background_recovery_with_turn(
                    &db,
                    &telemetry,
                    &manager,
                    &task_manager,
                    &task_id,
                    &claims,
                    &session_id,
                    &archival_user_message,
                    turn,
                    quality,
                    &reason,
                    recovery_mode,
                )
                .await;
            }
        }
    }
    let _ = release_pm_task_lease(&db, &task_id, &lease_worker_id, false).await;
    stop_pm_task_heartbeat(heartbeat_shutdown_tx, heartbeat_handle).await;
    trigger_pm_runtime_cycle_after_task_release(state.clone(), task_id.clone());
}

/// POST `/api/v1/agent/sessions/{session_id}/pm-research-tasks` — start background PM research.
pub(super) async fn start_pm_research_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
    Json(req): Json<StartPmResearchTaskRequest>,
) -> impl IntoResponse {
    let Some(handle) = get_agent_manager(&state).get_session(&session_id).await else {
        return AppError::NotFound(format!("session {session_id} not found")).into_response();
    };
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return AppError::Forbidden.into_response();
    }
    if !handle.source.eq_ignore_ascii_case("pm") {
        return AppError::ValidationError("pm research tasks require a PM session".into())
            .into_response();
    }
    let input_context =
        match normalize_pm_task_input_context(&req.images, &req.documents, &claims.sub) {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    if req.message.trim().is_empty() && input_context.is_none() {
        return AppError::ValidationError("message cannot be empty".into()).into_response();
    }

    let user_message = if req.message.trim().is_empty() {
        String::new()
    } else {
        maybe_dispatch_skill_command(&req.message, &claims.tenant_id, &state.db)
            .await
            .unwrap_or(req.message)
    };
    let user_message = sanitize_pm_user_message(&handle.source, user_message);
    let task_id = format!("pm-research-task-{}", uuid::Uuid::new_v4());
    let agent_task = match crate::routes::agent_ops::create_task_with_outcome(
        &state,
        crate::routes::agent_ops::CreateAgentTaskInput {
            tenant_id: claims.tenant_id.clone(),
            source: "pm".to_string(),
            source_ref: Some(task_id.clone()),
            source_label: Some("产运深度研究".to_string()),
            capability_key: "deep_research".to_string(),
            agent_id: None,
            agent_name: Some("深度研究".to_string()),
            title: user_message.chars().take(80).collect(),
            summary: Some("产运多源研究任务".to_string()),
            owner_user_id: Some(claims.sub.clone()),
            correlation_id: Some(session_id.clone()),
            parent_task_id: None,
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
            external_message_id: None,
            idempotency_key: Some(format!("pm-research:{task_id}")),
            input_json: Some(serde_json::json!({
                "sessionId": &session_id,
                "messageChars": user_message.chars().count(),
                "imageCount": input_context.as_ref().map_or(0, |value| value.images.len()),
                "documentCount": input_context.as_ref().map_or(0, |value| value.documents.len()),
            })),
        },
    )
    .await
    {
        Ok(task) => task,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = crate::routes::agent_ops::link_task_resource(
        &state,
        &claims.tenant_id,
        &agent_task.id,
        "pm_research_task",
        &task_id,
    )
    .await
    {
        let _ = crate::routes::agent_ops::fail_task(
            &state,
            &claims.tenant_id,
            &agent_task.id,
            "pm_research_resource_link_failed",
            &error.to_string(),
        )
        .await;
        return error.into_response();
    }
    let manager = pm_research_task_manager().clone();
    if let Err(e) = manager
        .create_task(
            state.control_db(),
            state.pm_telemetry(),
            &task_id,
            &session_id,
            &user_message,
            input_context.clone(),
            &claims.tenant_id,
            &claims.sub,
        )
        .await
    {
        let _ = crate::routes::agent_ops::fail_task(
            &state,
            &claims.tenant_id,
            &agent_task.id,
            "pm_research_queue_rejected",
            &e.to_string(),
        )
        .await;
        return e.into_response();
    }
    record_pm_audit_event(
        state.telemetry_db(),
        &claims.tenant_id,
        &claims.sub,
        &task_id,
        "pm_task_created",
        "info",
        "pm background research task created",
        Some(&serde_json::json!({
            "sessionId": session_id,
            "model": handle.model.clone(),
            "source": handle.source.clone(),
            "imageCount": input_context
                .as_ref()
                .map(|ctx| ctx.images.len())
                .unwrap_or(0),
            "documentCount": input_context
                .as_ref()
                .map(|ctx| ctx.documents.len())
                .unwrap_or(0),
        })),
    )
    .await;

    trigger_pm_runtime_cycle(state.clone(), task_id.clone(), "task_enqueue");

    Json(StartPmResearchTaskResponse {
        task_id,
        status: "queued".to_string(),
    })
    .into_response()
}

#[cfg(feature = "bot-agents")]
pub(crate) async fn start_pm_research_task_from_bot(
    state: &AppState,
    claims: Claims,
    input: PmResearchBotTaskInput,
) -> Result<PmResearchBotTaskResult, AppError> {
    let message = input.message.trim();
    if message.is_empty() {
        return Err(AppError::ValidationError("message cannot be empty".into()));
    }

    let session_id = if let Some(session_id) = input
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let Some(handle) = get_agent_manager(state).get_session(session_id).await else {
            return Err(AppError::NotFound(format!(
                "pm session {session_id} not found"
            )));
        };
        if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
            return Err(AppError::Forbidden);
        }
        if !pm_research_bot_session_source_allowed(&handle.source) {
            return Err(AppError::ValidationError(
                "pm research tasks require a PM or unified Super Assistant session".to_string(),
            ));
        }
        session_id.to_string()
    } else {
        let handle = get_agent_manager(state)
            .create_session(
                &claims.sub,
                &claims.tenant_id,
                None,
                input.model.as_deref(),
                "pm",
                Some("pm"),
                None,
                None,
            )
            .await
            .map_err(|error| {
                AppError::Internal(format!("create PM bot session failed: {error}"))
            })?;
        handle.session_id
    };

    let user_message = maybe_dispatch_skill_command(message, &claims.tenant_id, &state.db)
        .await
        .unwrap_or_else(|| message.to_string());
    let user_message = sanitize_pm_user_message("pm", user_message);
    let visible_message = input
        .visible_message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| sanitize_pm_user_message("pm", user_message.clone()));
    let input_context = Some(PmTaskInputContext {
        visible_message: Some(visible_message),
        ..PmTaskInputContext::default()
    });
    let task_id = format!("pm-research-task-{}", uuid::Uuid::new_v4());
    let manager = pm_research_task_manager().clone();
    manager
        .create_task(
            state.control_db(),
            state.pm_telemetry(),
            &task_id,
            &session_id,
            &user_message,
            input_context,
            &claims.tenant_id,
            &claims.sub,
        )
        .await?;

    record_pm_audit_event(
        state.telemetry_db(),
        &claims.tenant_id,
        &claims.sub,
        &task_id,
        "pm_task_created_from_bot",
        "info",
        "pm background research task created from bot gateway",
        Some(&serde_json::json!({
            "sessionId": session_id,
            "source": "bot_gateway",
            "model": input.model,
        })),
    )
    .await;

    trigger_pm_runtime_cycle(state.clone(), task_id.clone(), "bot_task_enqueue");

    Ok(PmResearchBotTaskResult {
        task_id,
        session_id,
        status: "queued".to_string(),
    })
}

#[cfg(feature = "bot-agents")]
fn pm_research_bot_session_source_allowed(source: &str) -> bool {
    matches!(
        source.trim().to_ascii_lowercase().as_str(),
        "pm" | "chat" | "super_assistant" | "super-assistant"
    )
}

/// GET `/api/v1/agent/pm-research-tasks/{task_id}` — get background PM task status.
pub(super) async fn get_pm_research_task_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let (snapshot, cancel_requested) = match pm_refresh_nonterminal_task_state(
        &state,
        &task_id,
        &claims.tenant_id,
        &claims.sub,
        "PM research task deadline elapsed while status endpoint was refreshing the task",
    )
    .await
    {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return AppError::NotFound("pm research task not found".to_string()).into_response();
        }
        Err(e) => return e.into_response(),
    };

    if snapshot.task_id.is_empty() {
        return AppError::NotFound("pm research task not found".to_string()).into_response();
    }

    Json(PmResearchTaskStatusResponse {
        task_id: snapshot.task_id,
        session_id: snapshot.session_id,
        status: snapshot.status,
        stage: snapshot.stage,
        attempt: snapshot.attempt,
        message: snapshot.message,
        elapsed_ms: snapshot.elapsed_ms,
        stage_elapsed_ms: snapshot.stage_elapsed_ms,
        detail: snapshot.detail,
        response: snapshot.response,
        error: snapshot.error,
        cancel_requested,
    })
    .into_response()
}

/// GET `/api/v1/agent/pm-research-tasks/{task_id}/subtasks` — list subtask runtime snapshots.
pub(super) async fn get_pm_research_task_subtasks(
    State(_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let rows = list_pm_subtask_runs_by_task(
        _state.telemetry_db(),
        &claims.tenant_id,
        &claims.sub,
        &task_id,
        256,
    )
    .await;
    Json(serde_json::json!({
        "taskId": task_id,
        "items": rows,
        "count": rows.len(),
    }))
    .into_response()
}

/// GET `/api/v1/agent/pm-research-tasks/{task_id}/subtasks/{subtask_id}/attempts` — list subtask attempts.
pub(super) async fn get_pm_research_task_subtask_attempts(
    State(_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path((task_id, subtask_id)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let rows = list_pm_subtask_attempts_by_task_and_subtask(
        _state.telemetry_db(),
        &claims.tenant_id,
        &claims.sub,
        &task_id,
        &subtask_id,
        512,
    )
    .await;
    Json(serde_json::json!({
        "taskId": task_id,
        "subtaskId": subtask_id,
        "items": rows,
        "count": rows.len(),
    }))
    .into_response()
}

/// GET `/api/v1/agent/pm-research-tasks/{task_id}/events` — stream background PM task events.
pub(super) async fn stream_pm_research_task_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let manager = pm_research_task_manager().clone();
    let (snapshot, _cancel_requested) = match pm_refresh_nonterminal_task_state(
        &state,
        &task_id,
        &claims.tenant_id,
        &claims.sub,
        "PM research task deadline elapsed while event stream was opening the task",
    )
    .await
    {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return AppError::NotFound("pm research task not found".to_string()).into_response();
        }
        Err(e) => return e.into_response(),
    };

    if let Some(mut rx) = manager
        .subscribe(&task_id, &claims.tenant_id, &claims.sub)
        .await
    {
        let mut answer_rx = manager
            .subscribe_answer_stream(&task_id, &claims.tenant_id, &claims.sub)
            .await;
        let db = state.control_db.clone();
        let state_for_stream = state.clone();
        let task_id_for_stream = task_id.clone();
        let tenant_id = claims.tenant_id.clone();
        let user_id = claims.sub.clone();
        let stream = async_stream::stream! {
            let mut cursor: u64 = 0;
            let mut answer_db_cursor: u64 = 0;
            let mut last_answer_sequence: u64 = 0;
            let mut replayed_snapshot = false;
            let mut last_emitted = snapshot.clone();
            let mut last_runtime_refresh: Option<Instant> = None;
            let db_poll_interval = pm_task_stream_db_poll_interval(true);
            match load_pm_task_events_from_db(
                &db,
                &task_id_for_stream,
                &tenant_id,
                &user_id,
                &snapshot.session_id,
                0,
                512,
            ).await {
                Ok(events) if !events.is_empty() => {
                    let mut last_replayed: Option<PmResearchTaskEvent> = None;
                    for (id, evt) in events {
                        cursor = id;
                        last_replayed = Some(evt.clone());
                        last_emitted = evt.clone();
                        let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                            axum::response::sse::Event::default().event("task_event").data(payload),
                        );
                        if let Some(extra_event) = pm_extra_sse_event_name(&evt) {
                            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                axum::response::sse::Event::default().event(extra_event).data(payload),
                            );
                        }
                    }
                    if let Some(last) = last_replayed.as_ref() {
                        replayed_snapshot =
                            pm_task_event_is_terminal(last) ||
                            (last.status == snapshot.status &&
                                last.stage == snapshot.stage &&
                                last.attempt == snapshot.attempt &&
                                last.elapsed_ms == snapshot.elapsed_ms);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        task_id = %task_id_for_stream,
                        error = %error,
                        "pm task event catch-up replay failed; streaming latest snapshot only"
                    );
                }
            }

            match load_pm_task_stream_events_from_db(
                &db,
                &task_id_for_stream,
                &tenant_id,
                &user_id,
                0,
                512,
            ).await {
                Ok(events) => {
                    for (id, evt) in events {
                        answer_db_cursor = id;
                        last_answer_sequence = last_answer_sequence.max(evt.sequence);
                        let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                            axum::response::sse::Event::default().event("pm_answer_delta").data(payload),
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        task_id = %task_id_for_stream,
                        error = %error,
                        "pm answer delta DB replay failed; continuing with live stream"
                    );
                }
            }

            if !replayed_snapshot {
                let snapshot_payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
                yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                    axum::response::sse::Event::default().event("task_event").data(snapshot_payload),
                );
                last_emitted = snapshot.clone();
                if let Some(extra_event) = pm_extra_sse_event_name(&snapshot) {
                    let payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
                    yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                        axum::response::sse::Event::default().event(extra_event).data(payload),
                    );
                }
            }

            if !pm_task_event_is_terminal(&snapshot) {
                loop {
                    tokio::select! {
                        answer_result = async {
                            match answer_rx.as_mut() {
                                Some(rx) => rx.recv().await,
                                None => std::future::pending::<Result<PmResearchTaskStreamEvent, tokio::sync::broadcast::error::RecvError>>().await,
                            }
                        } => {
                            match answer_result {
                                Ok(evt) => {
                                    if evt.sequence <= last_answer_sequence {
                                        continue;
                                    }
                                    last_answer_sequence = evt.sequence;
                                    let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                                    yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                        axum::response::sse::Event::default().event("pm_answer_delta").data(payload),
                                    );
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                    tracing::warn!(
                                        task_id = %task_id_for_stream,
                                        skipped,
                                        "pm answer delta stream lagged; terminal answer will reconcile the visible text"
                                    );
                                    continue;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    tracing::debug!(
                                        task_id = %task_id_for_stream,
                                        "pm answer delta broadcaster closed; continuing task event stream"
                                    );
                                    answer_rx = None;
                                    continue;
                                }
                            }
                        }
                        task_result = rx.recv() => {
                            match task_result {
                                Ok(evt) => {
                            if evt.elapsed_ms < snapshot.elapsed_ms && cursor > 0 {
                                continue;
                            }
                            last_emitted = evt.clone();
                            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                axum::response::sse::Event::default().event("task_event").data(payload),
                            );
                            if let Some(extra_event) = pm_extra_sse_event_name(&evt) {
                                let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                                yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                    axum::response::sse::Event::default().event(extra_event).data(payload),
                                );
                            }
                            if pm_task_event_is_terminal(&evt) {
                                break;
                            }
                        }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                task_id = %task_id_for_stream,
                                skipped,
                                "pm task event stream lagged; continuing with latest available events"
                            );
                            continue;
                        }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::warn!(
                                task_id = %task_id_for_stream,
                                "pm task event broadcaster closed unexpectedly; falling back to latest snapshot"
                            );
                            loop {
                                tokio::time::sleep(db_poll_interval).await;
                                match pm_refresh_nonterminal_task_state(
                                    &state_for_stream,
                                    &task_id_for_stream,
                                    &tenant_id,
                                    &user_id,
                                    "PM research task deadline elapsed after event broadcaster closed",
                                ).await {
                                    Ok(Some((latest, _))) => {
                                        if !pm_task_event_same_view(&latest, &last_emitted) {
                                            last_emitted = latest.clone();
                                            let payload = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".to_string());
                                            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                                axum::response::sse::Event::default().event("task_event").data(payload),
                                            );
                                            if let Some(extra_event) = pm_extra_sse_event_name(&latest) {
                                                let payload = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".to_string());
                                                yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                                    axum::response::sse::Event::default().event(extra_event).data(payload),
                                                );
                                            }
                                        }
                                        if pm_task_event_is_terminal(&latest) {
                                            break;
                                        }
                                    }
                                    Ok(None) => {
                                        let evt = serde_json::json!({
                                            "task_id": task_id_for_stream,
                                            "session_id": snapshot.session_id,
                                            "status": "running",
                                            "stage": "stream_reconnect",
                                            "message": "事件流暂时中断，正在等待任务状态刷新",
                                            "warning": "pm task snapshot temporarily unavailable",
                                        });
                                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                            axum::response::sse::Event::default().event("stream_warning").data(evt.to_string()),
                                        );
                                    }
                                    Err(error) => {
                                        let evt = serde_json::json!({
                                            "task_id": task_id_for_stream,
                                            "session_id": snapshot.session_id,
                                            "status": "running",
                                            "stage": "stream_reconnect",
                                            "message": "事件流读取暂时失败，正在等待下一次状态刷新",
                                            "warning": error.to_string(),
                                        });
                                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                            axum::response::sse::Event::default().event("stream_warning").data(evt.to_string()),
                                        );
                                    }
                                }
                            }
                            break;
                        }
                            }
                        }
                        _ = tokio::time::sleep(db_poll_interval) => {
                            match load_pm_task_stream_events_from_db(
                                &db,
                                &task_id_for_stream,
                                &tenant_id,
                                &user_id,
                                answer_db_cursor,
                                128,
                            ).await {
                                Ok(events) => {
                                    for (id, evt) in events {
                                        answer_db_cursor = id;
                                        if evt.sequence <= last_answer_sequence {
                                            continue;
                                        }
                                        last_answer_sequence = evt.sequence;
                                        let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                            axum::response::sse::Event::default().event("pm_answer_delta").data(payload),
                                        );
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        task_id = %task_id_for_stream,
                                        error = %error,
                                        "pm answer delta DB poll failed"
                                    );
                                }
                            }
                            match load_pm_task_events_from_db(
                                &db,
                                &task_id_for_stream,
                                &tenant_id,
                                &user_id,
                                &snapshot.session_id,
                                cursor,
                                128,
                            ).await {
                                Ok(events) => {
                                    let mut db_task_terminal = false;
                                    for (id, evt) in events {
                                        cursor = id;
                                        if evt.elapsed_ms < snapshot.elapsed_ms && cursor > 0 {
                                            continue;
                                        }
                                        last_emitted = evt.clone();
                                        let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                            axum::response::sse::Event::default().event("task_event").data(payload),
                                        );
                                        if let Some(extra_event) = pm_extra_sse_event_name(&evt) {
                                            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                                            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                                axum::response::sse::Event::default().event(extra_event).data(payload),
                                            );
                                        }
                                        if pm_task_event_is_terminal(&evt) {
                                            db_task_terminal = true;
                                        }
                                    }
                                    if db_task_terminal {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        task_id = %task_id_for_stream,
                                        error = %error,
                                        "pm task event DB poll failed"
                                    );
                                }
                            }
                            let should_nudge = last_runtime_refresh
                                .map(|last| last.elapsed() >= Duration::from_secs(5))
                                .unwrap_or(true);
                            let latest_result = if should_nudge {
                                last_runtime_refresh = Some(Instant::now());
                                pm_refresh_nonterminal_task_state(
                                    &state_for_stream,
                                    &task_id_for_stream,
                                    &tenant_id,
                                    &user_id,
                                    "PM research task deadline elapsed while event stream was refreshing the task",
                                )
                                .await
                            } else {
                                pm_load_best_task_snapshot(
                                    &state_for_stream,
                                    &task_id_for_stream,
                                    &tenant_id,
                                    &user_id,
                                )
                                .await
                            };

                            match latest_result {
                                Ok(Some((latest, _))) => {
                                    if !pm_task_event_same_view(&latest, &last_emitted) {
                                        last_emitted = latest.clone();
                                        let payload = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".to_string());
                                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                            axum::response::sse::Event::default().event("task_event").data(payload),
                                        );
                                        if let Some(extra_event) = pm_extra_sse_event_name(&latest) {
                                            let payload = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".to_string());
                                            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                                axum::response::sse::Event::default().event(extra_event).data(payload),
                                            );
                                        }
                                    }
                                    if pm_task_event_is_terminal(&latest) {
                                        break;
                                    }
                                }
                                Ok(None) => {
                                    let evt = serde_json::json!({
                                        "task_id": task_id_for_stream,
                                        "session_id": snapshot.session_id,
                                        "status": "running",
                                        "stage": "stream_reconnect",
                                        "message": "事件流暂时中断，正在等待任务状态刷新",
                                        "warning": "pm task snapshot temporarily unavailable",
                                    });
                                    yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                        axum::response::sse::Event::default().event("stream_warning").data(evt.to_string()),
                                    );
                                }
                                Err(error) => {
                                    let evt = serde_json::json!({
                                        "task_id": task_id_for_stream,
                                        "session_id": snapshot.session_id,
                                        "status": "running",
                                        "stage": "stream_reconnect",
                                        "message": "事件流读取暂时失败，正在等待下一次状态刷新",
                                        "warning": error.to_string(),
                                    });
                                    yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                        axum::response::sse::Event::default().event("stream_warning").data(evt.to_string()),
                                    );
                                }
                            }
                        }
                    };
                }
            }
        };

        return axum::response::Sse::new(stream).into_response();
    }

    let db = state.control_db.clone();
    let state_for_poll = state.clone();
    let task_id_owned = task_id.clone();
    let tenant_id = claims.tenant_id.clone();
    let user_id = claims.sub.clone();

    let stream = async_stream::stream! {
        let mut last_emitted = snapshot.clone();
        let mut last_runtime_refresh: Option<Instant> = None;
        let snapshot_payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
            axum::response::sse::Event::default().event("task_event").data(snapshot_payload),
        );
        if let Some(extra_event) = pm_extra_sse_event_name(&snapshot) {
            let payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                axum::response::sse::Event::default().event(extra_event).data(payload),
            );
        }

        let mut cursor: u64 = 0;
        let mut answer_db_cursor: u64 = 0;
        let mut last_answer_sequence: u64 = 0;
        let mut terminal = pm_task_event_is_terminal(&snapshot);
        let db_poll_interval = pm_task_stream_db_poll_interval(false);
        while !terminal {
            match load_pm_task_stream_events_from_db(
                &db,
                &task_id_owned,
                &tenant_id,
                &user_id,
                answer_db_cursor,
                128,
            ).await {
                Ok(events) => {
                    for (id, evt) in events {
                        answer_db_cursor = id;
                        if evt.sequence <= last_answer_sequence {
                            continue;
                        }
                        last_answer_sequence = evt.sequence;
                        let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                            axum::response::sse::Event::default().event("pm_answer_delta").data(payload),
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        task_id = %task_id_owned,
                        error = %error,
                        "pm answer delta DB poll failed in DB-only task stream"
                    );
                }
            }
            match load_pm_task_events_from_db(
                &db,
                &task_id_owned,
                &tenant_id,
                &user_id,
                &snapshot.session_id,
                cursor,
                128,
            ).await {
                Ok(events) => {
                    if events.is_empty() {
                        tokio::time::sleep(db_poll_interval).await;
                        let should_nudge = last_runtime_refresh
                            .map(|last| last.elapsed() >= Duration::from_secs(5))
                            .unwrap_or(true);
                        let latest_result = if should_nudge {
                            last_runtime_refresh = Some(Instant::now());
                            pm_refresh_nonterminal_task_state(
                                &state_for_poll,
                                &task_id_owned,
                                &tenant_id,
                                &user_id,
                                "PM research task deadline elapsed while DB event stream was refreshing the task",
                            )
                            .await
                        } else {
                            pm_load_best_task_snapshot(
                                &state_for_poll,
                                &task_id_owned,
                                &tenant_id,
                                &user_id,
                            )
                            .await
                        };
                        match latest_result {
                            Ok(Some((latest, _))) => {
                                if !pm_task_event_same_view(&latest, &last_emitted) {
                                    last_emitted = latest.clone();
                                    let payload = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".to_string());
                                    yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                        axum::response::sse::Event::default().event("task_event").data(payload),
                                    );
                                    if let Some(extra_event) = pm_extra_sse_event_name(&latest) {
                                        let payload = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".to_string());
                                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                            axum::response::sse::Event::default().event(extra_event).data(payload),
                                        );
                                    }
                                }
                                terminal = pm_task_event_is_terminal(&latest);
                            }
                            Ok(None) => {
                                let evt = serde_json::json!({
                                    "task_id": task_id_owned,
                                    "session_id": snapshot.session_id,
                                    "status": "running",
                                    "stage": "stream_reconnect",
                                    "message": "事件流暂时中断，正在等待任务状态刷新",
                                    "warning": "pm task snapshot temporarily unavailable",
                                });
                                yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                    axum::response::sse::Event::default().event("stream_warning").data(evt.to_string()),
                                );
                            }
                            Err(error) => {
                                let evt = serde_json::json!({
                                    "task_id": task_id_owned,
                                    "session_id": snapshot.session_id,
                                    "status": "running",
                                    "stage": "stream_reconnect",
                                    "message": "事件流读取暂时失败，正在等待下一次状态刷新",
                                    "warning": error.to_string(),
                                });
                                yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                    axum::response::sse::Event::default().event("stream_warning").data(evt.to_string()),
                                );
                            }
                        }
                        continue;
                    }
                    for (id, evt) in events {
                        cursor = id;
                        last_emitted = evt.clone();
                        let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                        yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                            axum::response::sse::Event::default().event("task_event").data(payload),
                        );
                        if let Some(extra_event) = pm_extra_sse_event_name(&evt) {
                            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                axum::response::sse::Event::default().event(extra_event).data(payload),
                            );
                        }
                        if pm_task_event_is_terminal(&evt) {
                            terminal = true;
                            break;
                        }
                    }
                    if !terminal {
                        tokio::time::sleep(db_poll_interval).await;
                    }
                }
                Err(error) => {
                    let evt = serde_json::json!({
                        "task_id": task_id_owned,
                        "session_id": snapshot.session_id,
                        "status": "running",
                        "stage": "stream_reconnect",
                        "message": "事件流读取暂时失败，正在等待下一次状态刷新",
                        "warning": error.to_string(),
                    });
                    yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                        axum::response::sse::Event::default().event("stream_warning").data(evt.to_string()),
                    );
                    tokio::time::sleep(db_poll_interval).await;
                }
            }
        }
    };

    axum::response::Sse::new(stream).into_response()
}

/// POST `/api/v1/agent/pm-research-tasks/{task_id}/cancel` — cancel a running background PM task.
pub(super) async fn cancel_pm_research_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let cancel_requested = match request_pm_research_task_cancel_from_agent_ops(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &task_id,
    )
    .await
    {
        Ok(ok) => ok,
        Err(e) => return e.into_response(),
    };

    Json(CancelPmResearchTaskResponse {
        task_id,
        status: if cancel_requested {
            "cancelling".to_string()
        } else {
            "completed".to_string()
        },
        cancel_requested,
    })
    .into_response()
}

pub(crate) async fn request_pm_research_task_cancel_from_agent_ops(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
) -> Result<bool, AppError> {
    #[cfg(feature = "bot-agents")]
    {
        let delegated = sqlx::query(
            "SELECT u.email, u.role
             FROM pm_research_tasks p
             JOIN users u ON u.tenant_id = p.tenant_id AND u.id = p.user_id
             WHERE p.tenant_id = ? AND p.user_id = ? AND p.task_id = ?
               AND JSON_EXTRACT(p.detail_json, '$.executionEngine') = 'super_assistant'
             LIMIT 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(task_id)
        .fetch_optional(state.control_db())
        .await?;
        if let Some(user) = delegated {
            let claims = Claims::new(
                user_id,
                &user.get::<String, _>("email"),
                &user.get::<String, _>("role"),
                tenant_id,
            );
            let _ = Box::pin(crate::routes::super_assistant_parent::cancel_parent_turn(
                axum::extract::State(state.clone()),
                axum::extract::Extension(claims),
                axum::extract::Path(task_id.to_string()),
            ))
            .await?;
            sqlx::query(
                "UPDATE pm_research_tasks
                 SET status = 'cancelled', stage = 'super_assistant', cancel_requested = 1,
                     completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND user_id = ? AND task_id = ?",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(task_id)
            .execute(state.control_db())
            .await?;
            return Ok(true);
        }
    }
    pm_research_task_manager()
        .mark_cancel_requested(
            state.control_db(),
            state.pm_telemetry(),
            task_id,
            tenant_id,
            user_id,
        )
        .await
}

/// POST `/api/v1/agent/pm-research-tasks/{task_id}/resume` — restart from the same session/question.
pub(super) async fn resume_pm_research_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match resume_pm_research_task_inner(&state, claims, &task_id).await {
        Ok(result) => Json(ResumePmResearchTaskResponse {
            task_id: result.task_id,
            status: result.status,
            restarted: result.restarted,
        })
        .into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xlsx_attachment_is_accepted_with_standard_browser_media_type() {
        let document = PmTaskDocumentInput {
            url: "/api/v1/uploads/user-a/report.xlsx".to_string(),
            file_id: Some("file-xlsx".to_string()),
            media_type: Some(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
            ),
            name: Some("report.xlsx".to_string()),
            size_bytes: Some(1024),
        };

        let context = normalize_pm_task_input_context(&[], &[document], "user-a")
            .expect("standard XLSX attachment should be accepted")
            .expect("document context");

        assert_eq!(context.documents.len(), 1);
        assert_eq!(
            context.documents[0].media_type.as_deref(),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        );
    }

    #[test]
    fn xlsx_attachment_media_type_is_inferred_from_filename() {
        let document = PmTaskDocumentInput {
            url: "/api/v1/uploads/user-a/report.xlsx".to_string(),
            file_id: None,
            media_type: None,
            name: None,
            size_bytes: None,
        };

        let context = normalize_pm_task_input_context(&[], &[document], "user-a")
            .expect("XLSX extension should be recognized")
            .expect("document context");

        assert_eq!(
            context.documents[0].media_type.as_deref(),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        );
    }

    #[test]
    fn pm_task_archive_prefers_visible_message_over_execution_context() {
        let context = PmTaskInputContext {
            visible_message: Some("用户原始问题".to_string()),
            ..PmTaskInputContext::default()
        };

        assert_eq!(
            pm_task_archival_user_message(
                Some(&context),
                "共享会话背景：内部召回内容\n\n用户当前问题：用户原始问题"
            ),
            "用户原始问题"
        );
    }

    #[test]
    fn pm_task_archive_falls_back_to_execution_message_for_legacy_tasks() {
        assert_eq!(
            pm_task_archival_user_message(None, "  普通 PM 原始问题  "),
            "普通 PM 原始问题"
        );
        let context = PmTaskInputContext {
            visible_message: Some("   ".to_string()),
            ..PmTaskInputContext::default()
        };
        assert_eq!(
            pm_task_archival_user_message(Some(&context), "旧任务问题"),
            "旧任务问题"
        );
    }

    #[test]
    fn pm_runtime_terminal_answer_dedupes_existing_assistant_message() {
        let mut session = runtime::Session::new();
        session
            .messages
            .push(runtime::ConversationMessage::assistant(vec![
                runtime::ContentBlock::Text {
                    text: "最终研究结论\n第二行".to_string(),
                },
            ]));

        assert!(pm_runtime_session_has_assistant_text(
            &session,
            "最终研究结论\n第二行"
        ));
        assert!(!pm_runtime_session_has_assistant_text(
            &session,
            "另一份结论"
        ));
        assert!(!pm_runtime_session_has_assistant_text(&session, "   "));
    }

    #[test]
    fn super_assistant_parent_exclusively_owns_the_final_answer() {
        assert!(!pm_session_owns_terminal_answer("super_assistant"));
        assert!(!pm_session_owns_terminal_answer("super-assistant"));
        assert!(pm_session_owns_terminal_answer("pm"));
        assert!(pm_session_owns_terminal_answer("chat"));
        #[cfg(feature = "bot-agents")]
        assert!(pm_research_bot_session_source_allowed("super_assistant"));
    }

    #[test]
    fn pm_runtime_terminal_answer_does_not_match_user_or_tool_content() {
        let mut session = runtime::Session::new();
        session
            .messages
            .push(runtime::ConversationMessage::user_text("最终研究结论"));
        session.messages.push(runtime::ConversationMessage {
            role: runtime::MessageRole::Tool,
            blocks: vec![runtime::ContentBlock::ToolResult {
                tool_use_id: "tool-1".to_string(),
                tool_name: "search".to_string(),
                output: "最终研究结论".to_string(),
                is_error: false,
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        });

        assert!(!pm_runtime_session_has_assistant_text(
            &session,
            "最终研究结论"
        ));
    }

    #[test]
    fn pm_runtime_terminal_answer_does_not_dedupe_a_new_turn_with_same_text() {
        let mut session = runtime::Session::new();
        session
            .messages
            .push(runtime::ConversationMessage::assistant(vec![
                runtime::ContentBlock::Text {
                    text: "相同的最终研究结论".to_string(),
                },
            ]));
        session
            .messages
            .push(runtime::ConversationMessage::user_text("请重新研究一次"));

        assert!(!pm_runtime_session_has_assistant_text(
            &session,
            "相同的最终研究结论"
        ));
    }

    #[test]
    fn pm_followup_context_detects_translation_requests() {
        assert!(pm_followup_needs_previous_answer_context("翻译成中文"));
        assert!(pm_followup_needs_previous_answer_context(
            "把上面那份方案翻译成中文"
        ));
        assert!(pm_followup_needs_previous_answer_context(
            "translate the above into Chinese"
        ));
    }

    #[test]
    fn pm_followup_context_does_not_capture_explicit_new_translation_payload() {
        assert!(!pm_followup_needs_previous_answer_context(
            "把“我爱你、你好”翻译成英文、西班牙语、印尼语，直接给表格。"
        ));
        assert!(!pm_followup_needs_previous_answer_context(
            "Translate 'hello' and 'goodbye' into Indonesian."
        ));
    }

    #[test]
    fn pm_followup_context_does_not_capture_new_research_questions() {
        assert!(!pm_followup_needs_previous_answer_context(
            "印尼移动广告市场与激励广告商业化环境如何？"
        ));
        assert!(!pm_followup_needs_previous_answer_context(
            "中文休闲游戏在印尼市场怎么做？"
        ));
        assert!(!pm_followup_needs_previous_answer_context(
            "Analyze Indonesia rewarded ads market opportunities"
        ));
    }

    #[test]
    fn csv_profile_treats_low_cardinality_numeric_columns_as_dimensions() {
        let csv = "group_id,dau,show_pv,ad_rev,cost\n1,10,100,5,2\n1,20,140,9,4\n2,8,70,3,2\n2,12,90,4,3\n";
        let profile =
            pm_build_csv_profile("demo.csv", csv, "对比AIPU、收入、成本、ROI").expect("profile");

        assert!(profile.contains("CSV 数据画像"));
        assert!(profile.contains("按 `group_id` 分组聚合"));
        assert!(profile.contains("sum(dau)"));
        assert!(profile.contains("sum(show_pv)"));
        assert!(profile.contains("sum(ad_rev)"));
        assert!(profile.contains("sum(cost)"));
        assert!(profile.contains("| 1 | 2 |"));
        assert!(profile.contains("| 2 | 2 |"));
    }

    #[test]
    fn deadline_local_strategy_synthesis_is_user_deliverable() {
        let question = "当前 DAU 25352，收入 1369，成本 1108，ROI 1.235。目标是提升 ROI 和 ROAS，但 AIPU、游戏时长和留存不能下降。新用户低 AIPU ROI 很低，老用户整体健康。";
        let turn = build_pm_local_strategy_synthesis_turn(
            "s1",
            "test-model",
            question,
            "deadline last-chance synthesis returned empty text",
            1,
            &[],
        );

        assert!(turn.text.contains("## 先给可执行结论"));
        assert!(turn.text.contains("ROI"));
        assert!(turn.text.contains("AIPU"));
        assert!(turn.text.contains("对照组") || turn.text.contains("holdout"));
        assert!(!turn.text.contains("last-chance"));
        assert!(!turn.text.contains("deadline"));
        assert!(!turn.text.contains("请稍后重试"));
        assert!(!turn.text.contains("当前无法生成完整深度报告"));
    }

    #[test]
    fn quality_gate_local_strategy_synthesis_is_user_deliverable() {
        let question = "我们有一组运营数据，需要判断哪个人群优先优化。目标是提升收入和 ROI，但留存、时长、用户体验不能下降。";
        let turn = build_pm_local_strategy_synthesis_turn(
            "s1",
            "test-model",
            question,
            "background quality gate produced no deliverable answer",
            1,
            &[],
        );

        assert!(turn.text.contains("## 先给可执行结论"));
        assert!(turn.text.contains("收入") || turn.text.contains("ROI"));
        assert!(turn.text.contains("验证") || turn.text.contains("对照组"));
        assert!(!turn.text.contains("quality gate"));
        assert!(!turn.text.contains("no deliverable"));
        assert!(!turn.text.contains("后台研究质量未达标"));
    }

    #[test]
    fn captured_answer_prefers_complete_snapshot_and_merges_continuation() {
        let mut parts = HashMap::new();
        parts.insert("synthesize".to_string(), "正文开头，结论是".to_string());
        parts.insert(
            "synthesize_continuation".to_string(),
            "结论是可行。后续行动。".to_string(),
        );
        assert_eq!(
            best_pm_captured_answer(&parts),
            "正文开头，结论是可行。后续行动。"
        );

        parts.insert(
            "final_candidate".to_string(),
            "这是更完整且已经通过最终整理的研究报告正文。".to_string(),
        );
        parts.insert(
            "synthesize".to_string(),
            "这是字符数更长但尚未完成的流式正文。".repeat(20),
        );
        assert_eq!(
            best_pm_captured_answer(&parts),
            "这是更完整且已经通过最终整理的研究报告正文。"
        );
    }

    #[test]
    fn failed_quality_gate_never_replaces_non_empty_model_answer() {
        let original =
            "## 研究结论\n\n这是模型已经生成的完整正文，质量门只能标记问题，不能替换正文。";
        let mut turn = build_pm_background_preserved_turn(
            "session-1",
            "deepseek-v4-flash",
            original.to_string(),
        );
        let mut quality = build_pm_direct_answer_quality();
        quality.passed = false;
        quality.deliverable = false;
        quality.claim_count = 0;
        quality.triad_coverage = 0.0;

        let recovered = preserve_pm_background_visible_answer(
            &mut turn,
            &mut quality,
            "不应覆盖模型正文的流式候选",
        );

        assert!(!recovered);
        assert_eq!(turn.text, original);
        assert!(quality.deliverable);
        assert!(quality
            .missing
            .iter()
            .any(|item| item == "quality_gate_preserved_model_answer"));
    }

    #[test]
    fn completed_turn_without_text_recovers_every_streamed_character() {
        let captured = "## 已输出正文\n\n第一部分。\n\n第二部分仍然必须保留。";
        let mut turn =
            build_pm_background_preserved_turn("session-1", "deepseek-v4-flash", String::new());
        let mut quality = build_pm_direct_answer_quality();

        let recovered = preserve_pm_background_visible_answer(&mut turn, &mut quality, captured);

        assert!(recovered);
        assert_eq!(turn.text, captured);
        assert!(quality.deliverable);
        assert!(!quality.passed);
    }
}

async fn resume_pm_research_task_inner(
    state: &AppState,
    claims: Claims,
    task_id: &str,
) -> Result<PmResearchBotResumeResult, AppError> {
    let manager = pm_research_task_manager().clone();
    let (session_id, user_message_raw, input_context, status, resume_checkpoint) =
        match load_pm_task_resume_context_from_db(
            state.control_db(),
            task_id,
            &claims.tenant_id,
            &claims.sub,
        )
        .await
        {
            Ok(Some((session_id, message, input_context, status, checkpoint))) => {
                (session_id, message, input_context, status, checkpoint)
            }
            Ok(None) => match manager
                .resume_payload(task_id, &claims.tenant_id, &claims.sub)
                .await
            {
                Ok((session_id, message, input_context)) => (
                    session_id,
                    message,
                    input_context,
                    "failed".to_string(),
                    None,
                ),
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        };
    let status_norm = status.to_ascii_lowercase();
    if status_norm != "failed" && status_norm != "cancelled" && status_norm != "interrupted" {
        return Err(AppError::ValidationError(
            "only failed/cancelled/interrupted task can be resumed".to_string(),
        ));
    }

    let Some(handle) = get_agent_manager(&state).get_session(&session_id).await else {
        return Err(AppError::NotFound(format!(
            "session {session_id} not found"
        )));
    };
    if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }

    let user_message = sanitize_pm_user_message(&handle.source, user_message_raw);

    let new_task_id = format!("pm-research-task-{}", uuid::Uuid::new_v4());
    if let Err(e) = manager
        .create_task(
            state.control_db(),
            state.pm_telemetry(),
            &new_task_id,
            &session_id,
            &user_message,
            input_context,
            &claims.tenant_id,
            &claims.sub,
        )
        .await
    {
        return Err(e);
    }
    if let Some(checkpoint) = resume_checkpoint.as_ref() {
        if let Err(error) = seed_pm_task_resume_checkpoint(
            state.control_db(),
            &new_task_id,
            &claims.tenant_id,
            &claims.sub,
            checkpoint,
        )
        .await
        {
            tracing::warn!(
                task_id = %new_task_id,
                previous_task_id = %task_id,
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                error = %error,
                "seed resume checkpoint failed; task will fallback to clean restart"
            );
        }
    }

    trigger_pm_runtime_cycle(state.clone(), new_task_id.clone(), "task_resume");

    Ok(PmResearchBotResumeResult {
        task_id: new_task_id,
        session_id,
        status: "queued".to_string(),
        restarted: resume_checkpoint.is_none(),
    })
}

pub(crate) async fn resume_pm_research_task_from_agent_ops(
    state: &AppState,
    claims: Claims,
    task_id: &str,
) -> Result<PmResearchBotResumeResult, AppError> {
    resume_pm_research_task_inner(state, claims, task_id).await
}
