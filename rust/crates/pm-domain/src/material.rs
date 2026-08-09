use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

pub fn is_html_document_body(body: &str) -> bool {
    let trimmed = body.trim_start();
    trimmed.starts_with("<!DOCTYPE html")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
}

pub fn excerpt_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub fn contains_any(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| text.contains(pattern))
}

fn image_size_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(\d{2,5})\s*[x×*]\s*(\d{2,5})\b").expect("valid image-size regex")
    })
}

pub fn extract_explicit_image_size_from_prompt(prompt: &str) -> Option<String> {
    let normalized = prompt.replace('＊', "*").replace('×', "x");
    let captures = image_size_regex().captures(&normalized)?;
    let width = captures.get(1)?.as_str().parse::<u32>().ok()?;
    let height = captures.get(2)?.as_str().parse::<u32>().ok()?;
    if width < 64 || height < 64 || width > 8192 || height > 8192 {
        return None;
    }
    Some(format!("{width}x{height}"))
}

pub fn infer_image_size_by_orientation(prompt: &str) -> &'static str {
    let normalized = prompt
        .to_ascii_lowercase()
        .replace('：', ":")
        .replace('／', "/")
        .replace('＊', "x")
        .replace('×', "x")
        .replace('*', "x");

    if contains_any(
        &normalized,
        &[
            "1:1",
            "1/1",
            "1x1",
            "square",
            "方图",
            "方形",
            "正方形",
            "方版",
        ],
    ) {
        return "1024x1024";
    }

    if contains_any(
        &normalized,
        &[
            "9:16",
            "9/16",
            "2:3",
            "2/3",
            "3:4",
            "3/4",
            "4:5",
            "4/5",
            "portrait",
            "vertical",
            "竖版",
            "竖屏",
            "竖图",
            "竖构图",
            "直版",
            "直式",
        ],
    ) {
        return "1024x1536";
    }

    if contains_any(
        &normalized,
        &[
            "16:9",
            "16/9",
            "3:2",
            "3/2",
            "4:3",
            "4/3",
            "5:4",
            "5/4",
            "landscape",
            "horizontal",
            "横版",
            "横屏",
            "横图",
            "横构图",
            "宽图",
            "宽屏",
        ],
    ) {
        return "1536x1024";
    }

    "1024x1024"
}

pub fn infer_image_size_from_prompt(prompt: &str) -> String {
    extract_explicit_image_size_from_prompt(prompt)
        .unwrap_or_else(|| infer_image_size_by_orientation(prompt).to_string())
}

pub fn infer_media_type_from_filename(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub fn images_edits_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/images/edits") {
        trimmed.to_string()
    } else if trimmed.ends_with("/images") {
        format!("{trimmed}/edits")
    } else {
        format!("{trimmed}/images/edits")
    }
}

pub fn audio_speech_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/audio/speech") {
        trimmed.to_string()
    } else if trimmed.ends_with("/audio") {
        format!("{trimmed}/speech")
    } else {
        format!("{trimmed}/audio/speech")
    }
}

pub fn audio_generations_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/audio/generations") {
        trimmed.to_string()
    } else if trimmed.ends_with("/audio") {
        format!("{trimmed}/generations")
    } else {
        format!("{trimmed}/audio/generations")
    }
}

pub fn openai_endpoint_candidates(base: &str, endpoint: fn(&str) -> String) -> Vec<String> {
    let trimmed = base.trim_end_matches('/');
    let mut candidates = vec![endpoint(trimmed)];
    let lowercase = trimmed.to_ascii_lowercase();
    let has_version_segment = lowercase.contains("/v1")
        || lowercase.contains("/v2")
        || lowercase.contains("/v3")
        || lowercase.contains("/v4")
        || lowercase.contains("/v5")
        || lowercase.contains("/compatible-mode/");
    if !has_version_segment {
        let with_v1 = format!("{trimmed}/v1");
        let v1_endpoint = endpoint(&with_v1);
        if !candidates.iter().any(|it| it == &v1_endpoint) {
            candidates.push(v1_endpoint);
        }
    }
    candidates
}

pub fn infer_audio_extension_from_content_type(content_type: &str) -> &'static str {
    let lower = content_type.to_ascii_lowercase();
    if lower.contains("mpeg") || lower.contains("mp3") {
        "mp3"
    } else if lower.contains("wav") || lower.contains("x-wav") {
        "wav"
    } else if lower.contains("ogg") {
        "ogg"
    } else if lower.contains("flac") {
        "flac"
    } else if lower.contains("aac") {
        "aac"
    } else if lower.contains("webm") {
        "webm"
    } else {
        "mp3"
    }
}

pub fn sanitize_audio_format(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mp3" => "mp3",
        "wav" => "wav",
        "opus" => "opus",
        "flac" => "flac",
        "pcm" => "pcm",
        _ => "mp3",
    }
}

pub fn is_suno_audio_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized == "suno" || normalized.starts_with("suno/")
}

pub fn normalized_non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

pub fn suno_endpoint_candidates(base: &str, paths: &[&str]) -> Vec<String> {
    let trimmed = base.trim_end_matches('/');
    let mut candidates = Vec::new();

    for path in paths {
        let normalized_path = path.trim();
        if normalized_path.is_empty() {
            continue;
        }
        if trimmed.ends_with(normalized_path.trim_start_matches('/')) {
            let endpoint = trimmed.to_string();
            if !candidates.iter().any(|item| item == &endpoint) {
                candidates.push(endpoint);
            }
            continue;
        }
        let endpoint = format!("{trimmed}/{}", normalized_path.trim_start_matches('/'));
        if !candidates.iter().any(|item| item == &endpoint) {
            candidates.push(endpoint);
        }
    }
    candidates
}

pub fn resolve_audio_endpoint(base: &str, path: &str) -> String {
    let trimmed_path = path.trim();
    if trimmed_path.starts_with("http://") || trimmed_path.starts_with("https://") {
        return trimmed_path.to_string();
    }
    let trimmed_base = base.trim_end_matches('/');
    let normalized_path = trimmed_path.trim_start_matches('/');
    format!("{trimmed_base}/{normalized_path}")
}

pub fn resolve_audio_query_endpoint_for_task(base: &str, path: &str, task_id: &str) -> String {
    resolve_audio_endpoint(base, path)
        .replace("{task_id}", task_id)
        .replace("{taskId}", task_id)
}

pub fn suno_generate_endpoint_candidates(
    base: &str,
    explicit_generate_path: Option<&str>,
) -> Vec<String> {
    if let Some(path) = explicit_generate_path
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return vec![resolve_audio_endpoint(base, path)];
    }
    suno_endpoint_candidates(base, &["api/v1/generate", "v1/generate", "generate"])
}

pub fn suno_submit_endpoint_candidates(
    base: &str,
    explicit_generate_path: Option<&str>,
    fallback_paths: &[&str],
) -> Vec<String> {
    if let Some(path) = explicit_generate_path
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return vec![resolve_audio_endpoint(base, path)];
    }
    suno_endpoint_candidates(base, fallback_paths)
}

pub fn suno_record_info_endpoint_candidates(
    base: &str,
    explicit_query_path: Option<&str>,
    task_id: &str,
) -> Vec<String> {
    if let Some(path) = explicit_query_path.map(str::trim).filter(|v| !v.is_empty()) {
        return vec![resolve_audio_query_endpoint_for_task(base, path, task_id)];
    }
    suno_endpoint_candidates(
        base,
        &[
            "api/v1/generate/record-info",
            "v1/generate/record-info",
            "generate/record-info",
        ],
    )
}

pub fn suno_try_get_string_at_path<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = payload;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str()
}

pub fn suno_find_string_by_keys(payload: &Value, keys: &[&str]) -> Option<String> {
    if let Some(obj) = payload.as_object() {
        for key in keys {
            if let Some(value) = obj.get(*key).and_then(Value::as_str) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        for value in obj.values() {
            if let Some(found) = suno_find_string_by_keys(value, keys) {
                return Some(found);
            }
        }
    } else if let Some(arr) = payload.as_array() {
        for item in arr {
            if let Some(found) = suno_find_string_by_keys(item, keys) {
                return Some(found);
            }
        }
    }
    None
}

pub fn suno_extract_task_id(payload: &Value) -> Option<String> {
    if let Some(task_id) = payload
        .get("data")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(task_id.to_string());
    }
    if let Some(task_id) = suno_try_get_string_at_path(payload, &["data", "taskId"]) {
        let trimmed = task_id.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    suno_find_string_by_keys(payload, &["taskId", "task_id", "jobId"])
}

pub fn suno_extract_status(payload: &Value) -> Option<String> {
    if let Some(status) = suno_try_get_string_at_path(payload, &["data", "response", "status"]) {
        let trimmed = status.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    suno_find_string_by_keys(payload, &["status"])
}

pub fn suno_extract_audio_url(payload: &Value) -> Option<String> {
    if let Some(url) = payload
        .get("data")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|v| {
            v.get("audioUrl")
                .or_else(|| v.get("audio_url"))
                .or_else(|| v.get("sourceAudioUrl"))
                .or_else(|| v.get("source_audio_url"))
                .or_else(|| v.get("streamAudioUrl"))
                .or_else(|| v.get("stream_audio_url"))
                .or_else(|| v.get("cld2AudioUrl"))
                .or_else(|| v.get("cld2_audio_url"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(url.to_string());
    }
    if let Some(url) = payload
        .get("data")
        .and_then(|v| v.get("response"))
        .and_then(|v| v.get("sunoData"))
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("audioUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(url.to_string());
    }
    if let Some(url) = suno_find_string_by_keys(
        payload,
        &[
            "audioUrl",
            "audio_url",
            "sourceAudioUrl",
            "source_audio_url",
            "streamAudioUrl",
            "stream_audio_url",
            "cld2AudioUrl",
            "cld2_audio_url",
            "audio",
        ],
    ) {
        let lowered = url.to_ascii_lowercase();
        if lowered.starts_with("http://") || lowered.starts_with("https://") {
            return Some(url);
        }
    }
    None
}

pub fn suno_is_processing_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "PENDING" | "QUEUEING" | "QUEUED" | "TEXT_SUCCESS" | "PROCESSING" | "RUNNING"
    )
}

pub fn suno_is_success_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "SUCCESS" | "FIRST_SUCCESS" | "COMPLETED" | "DONE"
    )
}

pub fn suno_is_failure_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "FAILED" | "ERROR" | "CANCELLED" | "TIMEOUT" | "REJECTED"
    )
}

fn suno_default_generation_model() -> String {
    normalized_non_empty_string(
        std::env::var("PM_MUSIC_SUNO_GENERATION_MODEL")
            .ok()
            .as_deref(),
    )
    .unwrap_or_else(|| "V4_5ALL".to_string())
}

pub fn suno_generation_model_from_entry_model(model: Option<&str>) -> String {
    let Some(raw) = model else {
        return suno_default_generation_model();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return suno_default_generation_model();
    }
    let lowered = trimmed.to_ascii_lowercase();
    if lowered == "suno" {
        return suno_default_generation_model();
    }
    for sep in ['/', ':', '|'] {
        let prefix = format!("suno{sep}");
        if lowered.starts_with(&prefix) {
            let tail = trimmed[prefix.len()..].trim();
            if !tail.is_empty() {
                return tail.to_string();
            }
            return suno_default_generation_model();
        }
    }
    trimmed.to_string()
}

pub fn suno_callback_url() -> String {
    normalized_non_empty_string(std::env::var("PM_MUSIC_SUNO_CALLBACK_URL").ok().as_deref())
        .unwrap_or_else(|| "https://example.com/aos/suno-callback".to_string())
}

pub fn infer_instrumental_mode_from_prompt(prompt: &str) -> bool {
    let lowered = prompt.to_ascii_lowercase();
    lowered.contains("instrumental")
        || lowered.contains("no vocal")
        || lowered.contains("纯音乐")
        || lowered.contains("无歌词")
}

pub fn compact_suno_prompt(prompt: &str) -> String {
    const MAX_CHARS: usize = 5000;
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(MAX_CHARS).collect()
}

pub trait PmContinuationAssetLike {
    fn meta(&self) -> &Value;
}

pub trait PmContinuationContentLike {
    fn content_text(&self) -> Option<&str>;
}

#[derive(Debug, Clone)]
pub struct PmMaterialThreadTextItem {
    pub iteration_no: i32,
    pub workflow_stage: Option<String>,
    pub prompt_text: String,
    pub content_text: String,
}

pub fn normalize_thread_id(thread_id: Option<i64>, fallback_job_id: i64) -> i64 {
    thread_id.unwrap_or(fallback_job_id)
}

pub fn material_previous_content_block<T: PmContinuationContentLike>(
    continuation_asset: Option<&T>,
) -> String {
    continuation_asset
        .and_then(PmContinuationContentLike::content_text)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!("上一阶段结果如下，可直接复用其中有效内容：\n{v}\n\n"))
        .unwrap_or_default()
}

pub fn build_music_audio_generation_input<T: PmContinuationContentLike>(
    prompt_text: &str,
    continuation_asset: Option<&T>,
    thread_context: &[PmMaterialThreadTextItem],
) -> Option<String> {
    let mut lines = Vec::<String>::new();
    let current_prompt = prompt_text.trim();
    lines.push(
        "任务：生成 1 首最终可播放音乐。优先保障成品质量、人声清晰度、吐字准确度和低噪声。"
            .to_string(),
    );
    lines.push("质量要求：studio-quality vocal, clear pronunciation, intelligible lyrics, clean mix, natural dynamics, no clipping, no harsh artifacts, no muffled vocal, no mumbling, no garbled lyrics.".to_string());
    if !current_prompt.is_empty() {
        lines.push(format!("当前用户要求：{current_prompt}"));
    }

    if thread_context.is_empty() {
        if let Some(previous) = continuation_asset
            .and_then(PmContinuationContentLike::content_text)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            lines.push("[PREVIOUS_STAGE]".to_string());
            lines.push(previous.to_string());
            lines.push("[/PREVIOUS_STAGE]".to_string());
        }
    } else {
        for item in thread_context {
            let stage = item.workflow_stage.as_deref().unwrap_or("unknown");
            let tag = match stage {
                "lyrics_draft" => "LYRICS_DRAFT",
                "composition_plan" => "COMPOSITION_PLAN",
                "generate" => "PREVIOUS_GENERATE_INSTRUCTION",
                _ => "REFERENCE_STAGE",
            };
            lines.push(format!("[{tag}]"));
            lines.push(format!(
                "iteration: {}\nworkflow_stage: {}\noriginal_requirement: {}",
                item.iteration_no,
                stage,
                item.prompt_text.trim()
            ));
            lines.push(item.content_text.trim().to_string());
            lines.push(format!("[/{tag}]"));
        }
    }

    if lines.len() <= 2 {
        return None;
    }
    lines.push("执行要求：如果存在歌词草案，必须按歌词内容演唱，不要改成无意义哼唱；如果存在编曲方案，优先沿用 style、mood、vocal、instruments、structure、negative_constraints。".to_string());
    Some(lines.join("\n\n"))
}

pub fn build_ppt_final_deck_input<T: PmContinuationContentLike>(
    prompt_text: &str,
    continuation_asset: Option<&T>,
    thread_context: &[PmMaterialThreadTextItem],
) -> String {
    let mut lines = Vec::<String>::new();
    let current_prompt = prompt_text.trim();
    lines.push("你在执行 PPT 工作流最终阶段：生成可渲染的完整 HTML 演示稿。".to_string());
    lines.push("目标：基于前序大纲、逐页蓝图与视觉方案，输出 1 套可直接预览、演示、导出 PDF 的 self-contained HTML deck，不要多版本。".to_string());
    if !current_prompt.is_empty() {
        lines.push(format!("当前用户要求：{current_prompt}"));
    }

    if thread_context.is_empty() {
        if let Some(previous) = continuation_asset
            .and_then(PmContinuationContentLike::content_text)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            lines.push("[PREVIOUS_STAGE]".to_string());
            lines.push(previous.to_string());
            lines.push("[/PREVIOUS_STAGE]".to_string());
        }
    } else {
        for item in thread_context {
            let stage = item.workflow_stage.as_deref().unwrap_or("unknown");
            let tag = match stage {
                "outline_draft" => "PPT_OUTLINE",
                "slide_blueprint" => "PPT_SLIDE_BLUEPRINT",
                "visual_plan" => "PPT_VISUAL_PLAN",
                "generate" => "PREVIOUS_FINAL_DECK",
                _ => "PPT_REFERENCE_STAGE",
            };
            lines.push(format!("[{tag}]"));
            lines.push(format!(
                "iteration: {}\nworkflow_stage: {}\noriginal_requirement: {}",
                item.iteration_no,
                stage,
                item.prompt_text.trim()
            ));
            lines.push(item.content_text.trim().to_string());
            lines.push(format!("[/{tag}]"));
        }
    }

    lines.push(
        "输出格式必须是完整 HTML 文档，并且只输出 HTML，不要 Markdown 代码围栏、不要解释文字。要求：
1) 必须包含 <!DOCTYPE html>、<html>、<head>、<body>；
2) 所有 CSS 和 JavaScript 内联在同一个 HTML 中，不依赖外部文件、CDN、远程字体或图片；
3) 使用 16:9 slide，推荐每页 <section class=\"slide\">，键盘左右键/空格可翻页；
4) 每页只表达一个核心结论，标题短而有判断，正文短句化；
5) 图表/数据页用 HTML/CSS 表格、条形图、KPI 卡片或 SVG，不要只写“此处放图”；
6) 每页讲稿备注放在 <aside class=\"notes\">，默认隐藏；
7) 页面需要适合 iframe 预览、浏览器全屏演示和 headless Chrome 导出 PDF；
8) 避免使用会被浏览器安全策略拦截的外部脚本或资源；
9) 视觉风格参考专业 HTML PPT：统一 tokens、主题、页码、进度条、适度动画，不要花哨堆叠。
验收标准：打开 HTML 文件即可翻页演示，内容完整，排版不溢出，不出现占位废话。"
            .to_string(),
    );
    lines.join("\n\n")
}

pub fn build_material_base_prompt<T: PmContinuationContentLike>(
    asset_type: &str,
    workflow_stage: Option<&str>,
    prompt_text: &str,
    reference_block: &str,
    continuation_asset: Option<&T>,
    workflow_payload: Option<&Value>,
) -> String {
    let previous_block = material_previous_content_block(continuation_asset);
    let workflow_payload_block = workflow_payload
        .map(|payload| payload.to_string())
        .map(|payload| format!("补充结构化输入（JSON）：{payload}\n\n"))
        .unwrap_or_default();
    match (asset_type, workflow_stage.unwrap_or_default()) {
        ("music", "lyrics_draft") => format!(
            "你在执行音乐工作流阶段 1/3：歌词草案。\n目标：根据需求产出可直接演唱的歌词初稿（只输出 1 版）。\n输出要求：\n1) 仅输出最终内容，不要解释；\n2) 必须包含“标题、语言、主歌、副歌、桥段”；\n3) 用清晰段落分隔，便于下阶段复用；\n4) 如果用户要求纯音乐，明确标注“纯音乐（无歌词）”。\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("music", "composition_plan") => format!(
            "你在执行音乐工作流阶段 2/3：编曲与风格确认。\n目标：基于已有歌词，输出可直接用于音乐模型的编曲方案（只输出 1 套）。\n输出要求：\n1) 仅输出最终内容，不要解释；\n2) 请按以下结构输出，且字段名保持不变：\n[COMPOSITION_PLAN]\nstyle: <风格标签，逗号分隔>\nmood: <情绪>\nbpm: <整数>\nvocal: <男声/女声/童声/合唱/纯音乐>\ninstruments: <核心乐器，逗号分隔>\nstructure: <主歌-副歌-桥段安排>\nmix_notes: <混音与空间感>\nnegative_constraints: <避免元素，逗号分隔>\n[/COMPOSITION_PLAN]\n3) 若用户要求纯音乐，vocal 必须写“纯音乐”。\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("music", "generate") => format!(
            "你在执行音乐工作流阶段 3/3：生成交付。\n目标：输出 1 套最终可执行的音乐生成指令包（只输出 1 套，不要多版本）。\n输出要求：\n1) 仅输出最终内容，不要解释；\n2) 输出顺序必须为：主 prompt、风格标签、负向约束、时长与结构、验收清单；\n3) 如已有上阶段结构化方案，请优先沿用并补齐缺失字段。\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("video", "script_draft") => format!(
            "你在执行视频工作流阶段 1/3：脚本草案。\n目标：输出 1 版可拍摄脚本。\n输出要求：\n1) 核心卖点；\n2) 时长建议（15s/30s）；\n3) 旁白与字幕；\n4) CTA。\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("video", "storyboard_plan") => format!(
            "你在执行视频工作流阶段 2/3：分镜规划。\n目标：基于脚本输出 1 套分镜与镜头执行清单。\n输出要求：\n1) 镜头编号；\n2) 画面描述；\n3) 运镜与节奏；\n4) 字幕出现点。\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("video", "generate") => format!(
            "你在执行视频工作流阶段 3/3：生成交付。\n目标：输出 1 套最终可执行的视频生成指令包（只输出 1 套，不要多版本）。\n输出要求：\n1) 主 prompt；\n2) 负向约束（可选）；\n3) 镜头衔接要求；\n4) 音频与字幕指令；\n5) 验收清单。\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("ppt", "outline_draft") => format!(
            "你在执行 PPT 工作流阶段 1/4：大纲草案。\n目标：根据需求产出 1 版高质量演示稿大纲。\n输出要求：\n1) 仅输出最终内容，不要解释；\n2) 必须包含目标受众、演示目标、建议页数、故事线、章节结构；\n3) 每个章节都要给核心结论，避免只有空泛标题；\n4) 请按以下结构输出，字段名保持不变：\n[PPT_OUTLINE]\ntitle: <演示稿标题>\naudience: <目标受众>\nobjective: <演示目标>\nrecommended_slide_count: <页数>\nstoryline: <背景-问题-洞察-方案-行动>\nsections:\n- section: <章节名>\n  key_message: <章节核心结论>\n  suggested_slides: <页数>\n[/PPT_OUTLINE]\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("ppt", "slide_blueprint") => format!(
            "你在执行 PPT 工作流阶段 2/4：逐页蓝图。\n目标：基于大纲输出 1 套可执行的逐页内容蓝图。\n输出要求：\n1) 仅输出最终内容，不要解释；\n2) 每页必须只有一个核心结论；\n3) 每页给出 slide_type、title、key_message、content_blocks、visual_need、speaker_notes；\n4) 图表页必须写清楚数据口径和推荐图表类型；\n5) 请按以下结构输出：\n[PPT_SLIDE_BLUEPRINT]\nslide_count: <页数>\nslides:\n1. slide_type: <cover|section|content|chart|comparison|timeline|summary>\n   title: <标题>\n   key_message: <本页核心结论>\n   content_blocks: <短句要点>\n   visual_need: <图表/图片/表格/图标建议>\n   speaker_notes: <讲稿备注>\n[/PPT_SLIDE_BLUEPRINT]\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("ppt", "visual_plan") => format!(
            "你在执行 PPT 工作流阶段 3/4：视觉方案。\n目标：基于逐页蓝图输出 1 套可渲染的视觉与模板方案。\n输出要求：\n1) 仅输出最终内容，不要解释；\n2) 不调用图片模型，仅规划版式、颜色、字体、图表风格和页面密度；\n3) 每页必须给 layout、visual_style、chart_or_media_plan、risk_notes；\n4) 请按以下结构输出：\n[PPT_VISUAL_PLAN]\ntheme: <商务/数据分析/产品方案/路演/运营复盘等>\ncolor_direction: <配色方向>\ntypography: <字体层级建议>\nlayout_rules: <统一版式规则>\nslides:\n1. layout: <推荐版式>\n   visual_style: <视觉风格>\n   chart_or_media_plan: <图表/图片/图标/表格方案>\n   density: <low|medium|high>\n   risk_notes: <文字过多/图表缺数据/需拆页等风险>\n[/PPT_VISUAL_PLAN]\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("ppt", "generate") => format!(
            "你在执行 PPT 工作流阶段 4/4：生成交付。\n目标：输出 1 套最终可演示的 self-contained HTML deck（只输出 1 套，不要多版本）。\n输出要求：\n1) 只输出完整 HTML 文档，不要 Markdown 代码围栏、不要解释；\n2) 必须可直接在浏览器打开并键盘翻页；\n3) 每页给出标题、核心结论、正文、视觉表达、隐藏讲稿备注；\n4) 正文必须短句化，关键数据和图表说明必须明确；\n5) 结尾必须有行动建议；\n6) CSS/JS 全部内联，不能依赖外部资源。\n\n{}{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("image", _) => format!(
            "请根据以下需求生成一张高质量图片。若提供参考图，请将其作为参考信息（风格/元素/构图等），并以用户需求为准进行创作；若未提供参考图，则按需求直接生成。要求构图清晰、主题突出、可用于实际投放。\n{}\n需求：{}",
            reference_block, prompt_text
        ),
        ("video", _) => format!(
            "请基于以下需求生成可落地的视频素材方案，输出：视频脚本（15s/30s）、分镜要点、镜头节奏、字幕文案、CTA、A/B测试建议。\n{}\n{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("music", _) => format!(
            "请基于以下需求生成音乐素材方案（只输出 1 套），输出：风格方向、情绪与节奏、主副段落结构、关键词提示词、使用场景、执行建议。\n{}\n{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        ("ppt", _) => format!(
            "请基于以下需求生成可执行的演示稿方案，输出：目录结构、每页核心内容、关键图表建议、收尾行动建议。\n{}\n{}需求：{}",
            workflow_payload_block, previous_block, prompt_text
        ),
        _ => {
            if let Some(previous_text) = continuation_asset
                .and_then(PmContinuationContentLike::content_text)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                format!(
                    "你将收到一版已有素材，请基于新需求进行改写，并直接输出完整的新版本内容（不要解释修改过程）。\n已有版本：\n{}\n\n新需求：{}",
                    previous_text, prompt_text
                )
            } else {
                prompt_text.to_string()
            }
        }
    }
}

pub fn material_notification_preview(text: &str, max_chars: usize) -> String {
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

pub fn suno_extract_meta_string(meta: &Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut current = meta;
        let mut ok = true;
        for segment in *path {
            if let Some(next) = current.get(*segment) {
                current = next;
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            if let Some(value) = current.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn suno_extract_previous_audio_id<T: PmContinuationAssetLike>(
    ctx: Option<&T>,
) -> Option<String> {
    let continuation = ctx?;
    suno_extract_meta_string(
        continuation.meta(),
        &[
            &["extra", "audioProviderMeta", "track", "audioId"],
            &["extra", "audioProviderMeta", "audioId"],
            &["extra", "audioProviderMeta", "sourceAudioId"],
            &["extra", "suno", "track", "audioId"],
            &["extra", "suno", "audioId"],
        ],
    )
}

pub fn prompt_requests_extend(prompt: &str) -> bool {
    let lowered = prompt.to_ascii_lowercase();
    ["extend", "continue", "续写", "延长", "加长", "接着生成"]
        .iter()
        .any(|token| lowered.contains(token))
}

pub fn prompt_requests_restart(prompt: &str) -> bool {
    let lowered = prompt.to_ascii_lowercase();
    [
        "重新生成",
        "从头生成",
        "全新",
        "from scratch",
        "new song",
        "restart",
    ]
    .iter()
    .any(|token| lowered.contains(token))
}

pub fn prompt_requests_add_vocals(prompt: &str) -> bool {
    let lowered = prompt.to_ascii_lowercase();
    [
        "add vocals",
        "add vocal",
        "vocalize",
        "添加人声",
        "加人声",
        "补人声",
        "加唱",
    ]
    .iter()
    .any(|token| lowered.contains(token))
}

pub fn prompt_requests_add_instrumental(prompt: &str) -> bool {
    let lowered = prompt.to_ascii_lowercase();
    [
        "add instrumental",
        "add accompaniment",
        "instrumentalize",
        "添加伴奏",
        "加伴奏",
        "补伴奏",
        "纯伴奏版",
    ]
    .iter()
    .any(|token| lowered.contains(token))
}

pub fn prompt_requests_replace_section(prompt: &str) -> bool {
    let lowered = prompt.to_ascii_lowercase();
    [
        "replace section",
        "replace lyrics",
        "section replace",
        "局部替换",
        "替换歌词",
        "替换片段",
        "替换某一段",
    ]
    .iter()
    .any(|token| lowered.contains(token))
}

pub fn prompt_requests_mashup(prompt: &str) -> bool {
    let lowered = prompt.to_ascii_lowercase();
    [
        "mashup",
        "mix songs",
        "song mash",
        "混音",
        "拼接歌曲",
        "拼接两首",
    ]
    .iter()
    .any(|token| lowered.contains(token))
}

pub fn suno_extract_previous_task_id<T: PmContinuationAssetLike>(
    ctx: Option<&T>,
) -> Option<String> {
    let continuation = ctx?;
    suno_extract_meta_string(
        continuation.meta(),
        &[
            &["extra", "audioProviderMeta", "taskId"],
            &["extra", "suno", "taskId"],
        ],
    )
}

pub fn suno_extract_previous_audio_url<T: PmContinuationAssetLike>(
    ctx: Option<&T>,
) -> Option<String> {
    let continuation = ctx?;
    suno_extract_meta_string(
        continuation.meta(),
        &[
            &["extra", "audioProviderMeta", "track", "audioUrl"],
            &["extra", "audioProviderMeta", "track", "streamAudioUrl"],
            &["extra", "audioProviderMeta", "audioUrl"],
            &["extra", "suno", "track", "audioUrl"],
            &["extra", "suno", "audioUrl"],
        ],
    )
}

pub fn extract_http_urls_from_prompt(prompt: &str) -> Vec<String> {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    let regex =
        URL_RE.get_or_init(|| Regex::new(r#"https?://[^\s)>\"]+"#).expect("valid url regex"));
    let mut urls = Vec::new();
    for cap in regex.captures_iter(prompt) {
        if let Some(m) = cap.get(0) {
            let url = m
                .as_str()
                .trim_end_matches(|ch: char| {
                    ['.', ',', ';', ')', ']', '，', '。', '；'].contains(&ch)
                })
                .trim()
                .to_string();
            if !url.is_empty() && !urls.iter().any(|it| it == &url) {
                urls.push(url);
            }
        }
    }
    urls
}

pub fn parse_replace_section_range(prompt: &str) -> Option<(f64, f64)> {
    static RANGE_RE: OnceLock<Regex> = OnceLock::new();
    let regex = RANGE_RE.get_or_init(|| {
        Regex::new(
            r"(?i)(\d+(?:\.\d+)?)\s*(?:-|~|到|至|to)\s*(\d+(?:\.\d+)?)\s*(?:s|sec|second|秒)?",
        )
        .expect("valid range regex")
    });
    let caps = regex.captures(prompt)?;
    let start = caps.get(1)?.as_str().parse::<f64>().ok()?;
    let end = caps.get(2)?.as_str().parse::<f64>().ok()?;
    if start >= end {
        return None;
    }
    let span = end - start;
    if (6.0..=60.0).contains(&span) {
        Some((start, end))
    } else {
        None
    }
}

pub fn parse_first_number_after_keywords(prompt: &str, keywords: &[&str]) -> Option<f64> {
    let lowered = prompt.to_ascii_lowercase();
    for key in keywords {
        if let Some(start) = lowered.find(key) {
            let tail = &lowered[start + key.len()..];
            let mut digits = String::new();
            for ch in tail.chars() {
                if ch.is_ascii_digit() || ch == '.' {
                    digits.push(ch);
                } else if !digits.is_empty() {
                    break;
                }
            }
            if let Ok(value) = digits.parse::<f64>() {
                if value > 0.0 {
                    return Some(value);
                }
            }
        }
    }
    None
}

pub fn parse_comp_line_value(prompt: &str, key: &str) -> Option<String> {
    let lowered_key = key.to_ascii_lowercase();
    for line in prompt.lines() {
        let trimmed = line.trim();
        let pair = trimmed.split_once(':').or_else(|| trimmed.split_once('：'));
        if let Some((left, right)) = pair {
            if left.trim().to_ascii_lowercase() == lowered_key {
                let value = right.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

#[derive(Debug)]
pub struct SunoGenerationPlan {
    pub prompt: String,
    pub custom_mode: bool,
    pub instrumental: bool,
    pub style: Option<String>,
    pub title: Option<String>,
    pub negative_tags: Option<String>,
    pub vocal_gender: Option<String>,
}

pub fn default_suno_negative_tags(instrumental: bool) -> String {
    if instrumental {
        "vocals, singing, speech, noise, distortion, clipping, harsh artifacts, low quality, muffled mix"
            .to_string()
    } else {
        "noise, distortion, clipping, harsh artifacts, low quality, muffled vocals, unclear pronunciation, garbled lyrics, mumbling, over-compressed mix"
            .to_string()
    }
}

pub fn build_suno_generation_plan(prompt: &str) -> SunoGenerationPlan {
    let prompt_text = prompt.trim();
    let style = parse_comp_line_value(prompt_text, "style")
        .or_else(|| parse_comp_line_value(prompt_text, "风格"))
        .or_else(|| parse_comp_line_value(prompt_text, "风格标签"));
    let mood = parse_comp_line_value(prompt_text, "mood")
        .or_else(|| parse_comp_line_value(prompt_text, "情绪"));
    let title = parse_comp_line_value(prompt_text, "title")
        .or_else(|| parse_comp_line_value(prompt_text, "标题"));
    let vocal = parse_comp_line_value(prompt_text, "vocal")
        .or_else(|| parse_comp_line_value(prompt_text, "人声"))
        .or_else(|| parse_comp_line_value(prompt_text, "人声设定"));
    let negative_tags = parse_comp_line_value(prompt_text, "negative_constraints")
        .or_else(|| parse_comp_line_value(prompt_text, "negative tags"))
        .or_else(|| parse_comp_line_value(prompt_text, "负向约束"));
    let has_lyrics_markers = ["[verse", "[chorus", "主歌", "副歌", "桥段"]
        .iter()
        .any(|marker| prompt_text.to_ascii_lowercase().contains(marker));
    let instrumental = infer_instrumental_mode_from_prompt(prompt_text)
        || vocal
            .as_deref()
            .map(|v| {
                let lowered = v.to_ascii_lowercase();
                lowered.contains("纯音乐") || lowered.contains("instrumental")
            })
            .unwrap_or(false);

    let style_value = match (style, mood) {
        (Some(s), Some(m)) if !m.is_empty() => Some(format!("{s}, {m}")),
        (Some(s), _) => Some(s),
        (_, Some(m)) => Some(m),
        _ => None,
    };
    let vocal_gender = vocal.and_then(|v| {
        let lowered = v.to_ascii_lowercase();
        if lowered.contains("female") || lowered.contains('女') {
            Some("f".to_string())
        } else if lowered.contains("male") || lowered.contains('男') {
            Some("m".to_string())
        } else {
            None
        }
    });

    let custom_mode =
        !prompt_text.is_empty() || style_value.is_some() || title.is_some() || has_lyrics_markers;
    SunoGenerationPlan {
        prompt: if custom_mode {
            prompt_text.chars().take(5000).collect()
        } else {
            prompt_text.chars().take(500).collect()
        },
        custom_mode,
        instrumental,
        style: style_value,
        title,
        negative_tags,
        vocal_gender,
    }
}

pub fn suno_extract_track_value<'a>(track: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    for key in keys {
        if let Some(value) = track.get(*key) {
            if !value.is_null() {
                return Some(value);
            }
        }
    }
    None
}

pub fn suno_extract_primary_track(payload: &Value) -> Option<Value> {
    payload
        .get("data")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .cloned()
        .or_else(|| {
            payload
                .get("data")
                .and_then(|v| v.get("response"))
                .and_then(|v| v.get("sunoData"))
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .cloned()
        })
}

pub fn suno_track_to_meta(track: Option<&Value>) -> Value {
    let Some(track) = track else {
        return Value::Null;
    };
    let mut map = serde_json::Map::new();
    if let Some(id) =
        suno_extract_track_value(track, &["id", "audioId", "clipId"]).and_then(Value::as_str)
    {
        map.insert("audioId".to_string(), Value::String(id.to_string()));
    }
    if let Some(url) = suno_extract_track_value(
        track,
        &[
            "audioUrl",
            "audio_url",
            "source_audio_url",
            "sourceAudioUrl",
            "streamAudioUrl",
            "stream_audio_url",
            "cld2AudioUrl",
            "cld2_audio_url",
        ],
    )
    .and_then(Value::as_str)
    {
        map.insert("audioUrl".to_string(), Value::String(url.to_string()));
    }
    if let Some(url) = suno_extract_track_value(track, &["streamAudioUrl", "stream_audio_url"])
        .and_then(Value::as_str)
    {
        map.insert("streamAudioUrl".to_string(), Value::String(url.to_string()));
    }
    if let Some(url) =
        suno_extract_track_value(track, &["imageUrl", "image_url", "source_image_url"])
            .and_then(Value::as_str)
    {
        map.insert("imageUrl".to_string(), Value::String(url.to_string()));
    }
    if let Some(title) = suno_extract_track_value(track, &["title"]).and_then(Value::as_str) {
        map.insert("title".to_string(), Value::String(title.to_string()));
    }
    if let Some(tags) = suno_extract_track_value(track, &["tags"]).and_then(Value::as_str) {
        map.insert("tags".to_string(), Value::String(tags.to_string()));
    }
    if let Some(duration) = suno_extract_track_value(track, &["duration"]).and_then(Value::as_f64) {
        map.insert("duration".to_string(), serde_json::json!(duration));
    }
    if map.is_empty() {
        Value::Null
    } else {
        Value::Object(map)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PmMaterialWorkflowError {
    UnsupportedStageForAssetType { asset_type: String },
    InvalidStage { asset_type: String, stage: String },
}

impl std::fmt::Display for PmMaterialWorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedStageForAssetType { asset_type } => {
                write!(
                    f,
                    "asset_type '{asset_type}' does not support workflow_stage"
                )
            }
            Self::InvalidStage { asset_type, stage } => {
                write!(
                    f,
                    "workflow_stage '{stage}' is invalid for asset_type '{asset_type}'"
                )
            }
        }
    }
}

impl std::error::Error for PmMaterialWorkflowError {}

pub fn material_model_type_for_asset_and_stage(
    asset_type: &str,
    workflow_stage: Option<&str>,
) -> Option<&'static str> {
    match asset_type {
        "text" => Some("chat"),
        "image" => Some("image"),
        "video" => Some("video"),
        "music" => {
            if workflow_stage.is_some_and(|stage| stage.eq_ignore_ascii_case("generate")) {
                Some("audio")
            } else {
                Some("chat")
            }
        }
        "ppt" => Some("chat"),
        _ => None,
    }
}

pub fn default_material_workflow_stage(asset_type: &str) -> Option<&'static str> {
    match asset_type {
        "music" => Some("lyrics_draft"),
        "video" => Some("script_draft"),
        "ppt" => Some("outline_draft"),
        _ => None,
    }
}

pub fn normalize_material_workflow_stage(
    asset_type: &str,
    raw_stage: Option<&str>,
) -> Result<Option<String>, PmMaterialWorkflowError> {
    let normalized = raw_stage
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_ascii_lowercase);
    let selected = if let Some(stage) = normalized.clone() {
        stage
    } else if let Some(default_stage) = default_material_workflow_stage(asset_type) {
        default_stage.to_string()
    } else {
        return Ok(None);
    };

    let allowed: &[&str] = match asset_type {
        "music" => &["lyrics_draft", "composition_plan", "generate"],
        "video" => &["script_draft", "storyboard_plan", "generate"],
        "ppt" => &[
            "outline_draft",
            "slide_blueprint",
            "visual_plan",
            "generate",
        ],
        _ => &[],
    };
    if allowed.is_empty() {
        if normalized.is_some() {
            return Err(PmMaterialWorkflowError::UnsupportedStageForAssetType {
                asset_type: asset_type.to_string(),
            });
        }
        return Ok(None);
    }
    if !allowed
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&selected))
    {
        return Err(PmMaterialWorkflowError::InvalidStage {
            asset_type: asset_type.to_string(),
            stage: selected,
        });
    }
    Ok(Some(selected))
}

pub fn material_workflow_is_generate_stage(stage: Option<&str>) -> bool {
    stage.is_some_and(|v| v.eq_ignore_ascii_case("generate"))
}

pub fn trim_text(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }
    value.chars().take(max_len).collect()
}

pub fn mission_task_id_prefix(mission_id: i64) -> String {
    format!("pm-mission-{mission_id}-")
}

pub fn mission_task_id_like(mission_id: i64) -> String {
    format!("{}%", mission_task_id_prefix(mission_id))
}
