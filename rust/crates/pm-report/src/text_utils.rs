use sha2::{Digest, Sha256};

pub fn extract_http_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let remainder = &text[cursor..];
        let http_rel = remainder.find("http://");
        let https_rel = remainder.find("https://");
        let Some(rel) = pick_nearest_offset(http_rel, https_rel) else {
            break;
        };
        let start = cursor + rel;
        let tail = &text[start..];
        let mut end = text.len();
        for (offset, ch) in tail.char_indices() {
            if offset == 0 {
                continue;
            }
            if is_url_break_char(ch) {
                end = start + offset;
                break;
            }
        }
        if let Some(url) = normalize_http_url_candidate(&text[start..end]) {
            urls.push(url);
        }
        cursor = end.saturating_add(1);
    }
    urls.sort();
    urls.dedup();
    urls
}

pub fn extract_url_domain(url: &str) -> Option<String> {
    let normalized = normalize_http_url_candidate(url)?;
    let (_, tail) = normalized.split_once("://")?;
    let host = tail
        .split('/')
        .next()?
        .split('?')
        .next()?
        .trim()
        .trim_end_matches(':')
        .to_ascii_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

pub fn estimate_claim_count(text: &str) -> usize {
    let mut count = 0usize;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let is_bullet = line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("• ")
            || line.starts_with("1.")
            || line.starts_with("2.")
            || line.starts_with("3.")
            || line.starts_with("4.");
        let has_label =
            line.contains("FACT") || line.contains("HYPOTHESIS") || line.contains("RECOMMENDATION");
        if (is_bullet || has_label) && line.len() >= 10 {
            count += 1;
        }
    }
    count
}

pub fn normalize_claim_key(input: &str) -> String {
    input
        .trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '`')
        .to_ascii_lowercase()
}

pub fn truncate_for_log(input: &str, max_chars: usize) -> String {
    let mut out: String = input
        .chars()
        .take(max_chars)
        .collect::<String>()
        .replace('\n', " ");
    if input.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn first_non_empty_line(input: &str) -> String {
    input
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

pub fn normalize_http_url_candidate(raw: &str) -> Option<String> {
    let mut candidate = raw.trim();
    if candidate.is_empty() {
        return None;
    }
    if let Some((before, _)) = candidate.split_once('\\') {
        candidate = before;
    }
    candidate = candidate.trim_matches(|c: char| {
        c == '"'
            || c == '\''
            || c == ')'
            || c == '('
            || c == '['
            || c == ']'
            || c == '{'
            || c == '}'
            || c == '<'
            || c == '>'
            || c == ','
            || c == ';'
            || c == '，'
            || c == '。'
            || c == '！'
            || c == '？'
    });
    if candidate.ends_with(':') {
        candidate = candidate.trim_end_matches(':');
    }
    if !(candidate.starts_with("http://") || candidate.starts_with("https://")) {
        return None;
    }
    if candidate.chars().count() > 2048 {
        return None;
    }
    let (_, tail) = candidate.split_once("://")?;
    let host = tail
        .split('/')
        .next()?
        .split('?')
        .next()?
        .trim()
        .to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(candidate.to_string())
}

pub fn is_pm_high_signal_source_url(raw: &str) -> bool {
    let Some(url) = normalize_http_url_candidate(raw) else {
        return false;
    };
    let Some((_, tail)) = url.split_once("://") else {
        return false;
    };
    let mut host_and_rest = tail.splitn(2, '/');
    let host = host_and_rest
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let rest = host_and_rest.next().unwrap_or("");
    if host.is_empty() {
        return false;
    }

    // Drop infra/auth/static/search-landing sources that regularly pollute PM reports.
    let blocked_exact_hosts = [
        "accounts.google.com",
        "myaccount.google.com",
        "consent.google.com",
        "fonts.googleapis.com",
        "fonts.gstatic.com",
        "www.gstatic.com",
        "gstatic.com",
        "googleads.g.doubleclick.net",
        "adservice.google.com",
        "webcache.googleusercontent.com",
    ];
    if blocked_exact_hosts.contains(&host.as_str()) {
        return false;
    }
    let blocked_host_suffixes = [
        ".gstatic.com",
        ".doubleclick.net",
        ".googleusercontent.com",
        ".googlesyndication.com",
    ];
    if blocked_host_suffixes
        .iter()
        .any(|suffix| host.ends_with(suffix))
    {
        return false;
    }

    let rest_lower = rest.to_ascii_lowercase();
    let path = rest_lower.split('?').next().unwrap_or("").trim();
    let query = rest_lower.split_once('?').map(|(_, q)| q).unwrap_or("");
    let search_host_exact = [
        "google.com",
        "www.google.com",
        "m.google.com",
        "bing.com",
        "www.bing.com",
        "search.yahoo.com",
        "baidu.com",
        "www.baidu.com",
        "yandex.com",
        "www.yandex.com",
    ];
    let search_host_suffixes = [".bing.com", ".baidu.com", ".yandex.com"];
    let is_search_host = search_host_exact.contains(&host.as_str())
        || search_host_suffixes
            .iter()
            .any(|suffix| host.ends_with(suffix))
        || host.starts_with("search.")
        || host.starts_with("www.search.");
    if is_search_host
        && (path.starts_with("search")
            || path.starts_with("/search")
            || path.starts_with("html")
            || path.starts_with("/html")
            || query.contains("q=")
            || query.contains("wd=")
            || query.contains("text="))
    {
        return false;
    }

    if path.contains("/login")
        || path.contains("/signin")
        || path.contains("/signup")
        || path.contains("/oauth")
        || path.contains("/authorize")
        || path.contains("/auth")
        || path.contains("/account")
        || path.contains("/consent")
    {
        return false;
    }

    let blocked_ext = [
        ".css", ".js", ".mjs", ".svg", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".woff",
        ".woff2", ".ttf", ".otf", ".map", ".xml",
    ];
    if blocked_ext.iter().any(|ext| path.ends_with(ext)) {
        return false;
    }

    true
}

fn pick_nearest_offset(first: Option<usize>, second: Option<usize>) -> Option<usize> {
    match (first, second) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn is_url_break_char(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '|' | '`' | '\\'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_http_urls_drops_prompt_suffix_noise() {
        let text =
            "{(https://dataportal.com/reports/digital-2025-indonesia\\nPrompt:)} and next line";
        let urls = extract_http_urls(text);
        assert_eq!(
            urls,
            vec!["https://dataportal.com/reports/digital-2025-indonesia".to_string()]
        );
    }

    #[test]
    fn extract_http_urls_handles_mixed_punctuation() {
        let text = "evidence: [https://example.com/path?a=1], (https://foo.bar/baz).";
        let urls = extract_http_urls(text);
        assert_eq!(
            urls,
            vec![
                "https://example.com/path?a=1".to_string(),
                "https://foo.bar/baz".to_string(),
            ]
        );
    }

    #[test]
    fn is_pm_high_signal_source_url_filters_auth_and_search_noise() {
        assert!(!is_pm_high_signal_source_url(
            "https://accounts.google.com/signin/v2/identifier"
        ));
        assert!(!is_pm_high_signal_source_url(
            "https://www.google.com/search?q=indonesia+market"
        ));
        assert!(!is_pm_high_signal_source_url(
            "https://search.example.com/search?q=indonesia+reward+app"
        ));
        assert!(!is_pm_high_signal_source_url(
            "https://search.example.com/html/?q=indonesia+reward+app"
        ));
        assert!(is_pm_high_signal_source_url(
            "https://dataportal.com/reports/digital-2025-indonesia"
        ));
    }
}

pub fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}
