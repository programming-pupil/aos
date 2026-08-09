//! Pure stream-session policy helpers shared by PM/RD chat surfaces.

pub fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn image_max_count() -> usize {
    env_usize("AGENT_STREAM_IMAGE_MAX_COUNT", 5).max(1)
}

pub fn image_max_bytes() -> usize {
    env_usize("AGENT_STREAM_IMAGE_MAX_BYTES", 8 * 1024 * 1024).max(256 * 1024)
}

pub fn image_max_total_bytes() -> usize {
    env_usize("AGENT_STREAM_IMAGE_MAX_TOTAL_BYTES", 24 * 1024 * 1024).max(image_max_bytes())
}

pub fn image_summary_max_tokens() -> u32 {
    u32::try_from(env_usize("AGENT_STREAM_IMAGE_SUMMARY_MAX_TOKENS", 1200))
        .unwrap_or(1200)
        .max(256)
}

pub fn image_summary_timeout_secs() -> u64 {
    env_u64("AGENT_STREAM_IMAGE_SUMMARY_TIMEOUT_SECS", 60).max(15)
}

pub fn image_summary_model(session_model: &str) -> String {
    if let Some(explicit) = std::env::var("AGENT_STREAM_IMAGE_SUMMARY_MODEL")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
    {
        return explicit;
    }
    if let Some(fallback) = std::env::var("PM_TASK_IMAGE_SUMMARY_MODEL")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
    {
        return fallback;
    }
    session_model.trim().to_string()
}

pub fn summary_text_indicates_no_vision(summary: &str) -> bool {
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

pub fn image_context_warning_message(error: &str) -> &'static str {
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

pub fn scenario(source: &str) -> &'static str {
    if source.eq_ignore_ascii_case("pm") || source.starts_with("pm_internal") {
        "pm"
    } else if source.eq_ignore_ascii_case("nl2sql") {
        "nl2sql"
    } else {
        "rd"
    }
}

pub fn notification_preview(text: &str, max_chars: usize) -> String {
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

pub fn notification_route(source: &str) -> (&'static str, &'static str, &'static str) {
    if source.eq_ignore_ascii_case("pm") {
        (
            "pm_assistant",
            "pm_assistant.answer_completed",
            "产运助手问答完成",
        )
    } else if source.eq_ignore_ascii_case("nl2sql") {
        ("nl2sql", "nl2sql.answer_completed", "数据探索问答完成")
    } else {
        ("rd_agent", "rd_agent.answer_completed", "研发助手问答完成")
    }
}

pub fn merge_user_message_with_image_summary(
    user_message: &str,
    image_summary: &str,
    image_count: usize,
) -> String {
    let base = if user_message.trim().is_empty() {
        "请结合图片内容给出结论。".to_string()
    } else {
        user_message.trim().to_string()
    };
    let capped_summary: String = image_summary.chars().take(6000).collect();
    format!(
        "{base}\n\n[图片理解上下文]\n图片数量: {image_count}\n以下内容由系统对用户上传图片提炼，可作为回答依据：\n{capped_summary}\n[/图片理解上下文]"
    )
}
