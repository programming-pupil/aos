pub fn should_failover_immediately(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("http status 403")
        || lower.contains("http status 429")
        || lower.contains("rate limit")
}

pub fn classify_retrieve_error(error_text: &str) -> &'static str {
    let lower = error_text.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        "timeout"
    } else if lower.contains("empty response") {
        "upstream_empty"
    } else if lower.contains("tool-only") {
        "tool_only"
    } else if lower.contains("403") || lower.contains("429") {
        "blocked_or_ratelimited"
    } else if lower.contains("sse channel full") {
        "stream_backpressure"
    } else {
        "runtime_error"
    }
}
