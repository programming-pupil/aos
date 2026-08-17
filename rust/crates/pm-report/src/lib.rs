mod builder;
mod html;
mod text_utils;

use serde::Serialize;

pub use builder::{build_pm_report_artifact, contains_any_token, pm_escape_html};
pub use html::render_pm_report_html;
pub use pm_domain::route_plan::build_pm_query_variants;
pub use text_utils::{
    contains_cjk, estimate_claim_count, extract_http_urls, extract_url_domain,
    first_non_empty_line, is_pm_high_signal_source_url, normalize_claim_key,
    normalize_http_url_candidate, sha256_hex, truncate_for_log,
};

#[derive(Debug, Serialize, Clone)]
pub struct PmClaimEvidenceDto {
    pub claim: String,
    pub evidence_excerpt: String,
    pub urls: Vec<String>,
    pub cited: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct PmConflictRowDto {
    pub topic: String,
    pub source_a: String,
    pub claim_a: String,
    pub source_b: String,
    pub claim_b: String,
    pub verdict: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct PmEvidenceLeafDto {
    pub url: String,
    pub domain: String,
    pub excerpt: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct PmEvidenceTreeNodeDto {
    pub claim: String,
    pub status: String,
    pub evidence_count: usize,
    pub evidences: Vec<PmEvidenceLeafDto>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PmConflictEdgeDto {
    pub topic: String,
    pub source_left: String,
    pub source_right: String,
    pub relation: String,
    pub verdict: String,
    pub confidence: f64,
    pub urls: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PmConflictGraphDto {
    pub topic_count: usize,
    pub edge_count: usize,
    pub adjudicated_count: usize,
    pub unresolved_count: usize,
    pub avg_confidence: f64,
    pub edges: Vec<PmConflictEdgeDto>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PmAnswerQualityDto {
    pub passed: bool,
    pub deliverable: bool,
    pub quality_level: String,
    pub has_tool_calls: bool,
    pub tool_call_count: usize,
    pub citation_count: usize,
    pub domain_count: usize,
    pub claim_count: usize,
    pub claim_alignment_ok: bool,
    pub triad_total_claims: usize,
    pub triad_aligned_claims: usize,
    pub triad_coverage: f64,
    pub conflict_adjudicated: bool,
    pub conflict_confidence: f64,
    pub conflict_reason: String,
    pub citations: Vec<String>,
    pub domains: Vec<String>,
    pub claim_alignment: Vec<PmClaimEvidenceDto>,
    pub evidence_tree: Vec<PmEvidenceTreeNodeDto>,
    pub conflict_matrix: Vec<PmConflictRowDto>,
    pub conflict_graph: PmConflictGraphDto,
    pub missing: Vec<String>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PmReportArtifactDto {
    pub schema_version: String,
    pub question_type: String,
    pub quant_enabled: bool,
    pub report_json: serde_json::Value,
    pub report_html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_json_v3: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_html_v3: Option<String>,
}

pub use pm_domain::json_utils::{
    extract_first_json_object, extract_named_json_object, parse_json_object_relaxed,
};

pub fn strip_pm_list_prefix(line: &str) -> String {
    let mut current = line.trim_start();
    loop {
        if let Some(rest) = current.strip_prefix("- ") {
            current = rest.trim_start();
            continue;
        }
        if let Some(rest) = current.strip_prefix("* ") {
            current = rest.trim_start();
            continue;
        }
        if let Some(rest) = current.strip_prefix("• ") {
            current = rest.trim_start();
            continue;
        }
        let mut digit_bytes = 0usize;
        for ch in current.chars() {
            if ch.is_ascii_digit() {
                digit_bytes += ch.len_utf8();
            } else {
                break;
            }
        }
        if digit_bytes > 0 {
            let after_digits = &current[digit_bytes..];
            let mut chars = after_digits.chars();
            if let Some(sep) = chars.next() {
                if sep == '.' || sep == ')' || sep == '、' {
                    current = chars.as_str().trim_start();
                    continue;
                }
            }
        }
        break;
    }
    current.to_string()
}

fn is_pm_rigid_template_heading(line: &str) -> bool {
    let cleaned = line
        .trim()
        .trim_matches(|ch: char| {
            ch == '#'
                || ch == '*'
                || ch == '`'
                || ch == '_'
                || ch == '-'
                || ch == '='
                || ch == '~'
                || ch.is_whitespace()
        })
        .trim_end_matches([':', '：'])
        .trim()
        .to_ascii_lowercase();
    matches!(
        cleaned.as_str(),
        "summary"
            | "research plan"
            | "key findings"
            | "claim-evidence alignment"
            | "claim evidence alignment"
            | "risks/unknowns"
            | "action plan"
    )
}

pub fn is_pm_visible_output_noise(line: &str) -> bool {
    let normalized = strip_pm_list_prefix(line);
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return false;
    }
    if matches!(trimmed, "{" | "}" | "[" | "]" | "- {" | "- }") {
        return true;
    }
    if is_pm_rigid_template_heading(trimmed) {
        return true;
    }

    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("RETRIEVE_CONSTRAINTS")
        || upper.starts_with("RETRIEVE_RESULT")
        || upper.starts_with("REPAIR_SCOPE")
        || upper.starts_with("REPAIR_RESULT")
        || upper.starts_with("SYNTHESIS_META")
        || upper.starts_with("REPORT_JSON")
    {
        return true;
    }
    if trimmed.starts_with('{')
        && (upper.contains("\"ROUTE\"")
            || upper.contains("\"REPAIRONLY\"")
            || upper.contains("\"EVIDENCECONFIDENCE\"")
            || upper.contains("\"SCHEMAVERSION\"")
            || upper.contains("\"EVIDENCETRIADS\"")
            || upper.contains("\"ERROR\"")
            || upper.contains("\"STACK\""))
    {
        return true;
    }

    let source_stub = trimmed.trim_start_matches('{').trim_start();
    if source_stub.starts_with("（来源：")
        || source_stub.starts_with("(source:")
        || source_stub.starts_with("来源：")
    {
        let lexical = source_stub
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(ch))
            .count();
        if lexical <= 12 {
            return true;
        }
    }

    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("tool '")
        || lower.starts_with("tool \"")
        || lower.starts_with("fact:")
        || lower.starts_with("### probe source [")
        || lower.contains("probe source [")
        || (lower.contains("tool ") && lower.contains(" failed:"))
        || lower.contains("web search unavailable on all endpoints")
        || lower.contains("search unavailable on all endpoints")
        || lower.contains("websearch retrieval attempted for query variant")
        || lower.contains("configured search provider returned http 429")
        || lower.contains("no external evidence urls were retrieved in this probe")
        || lower.contains("insufficient retrieved evidence to support claims")
        || lower.contains("no conflict matrix can be produced")
        || lower.contains("recommended next retrieval actions")
        || lower.contains("本次按指令仅调用 1 次 websearch")
        || lower.contains("外部检索证据不足")
        || lower.contains("来源状态：当前证据不足")
        || lower.contains("来源状态: 当前证据不足")
        || lower.contains("外部检索这轮有缺口")
        || lower.contains("websearch 没配置成功")
        || lower.contains("websearch 未配置成功")
        || lower.contains("websearch requires a configured search provider")
        || lower.contains("no healthy enabled configured search provider")
        || lower.contains("depth gate:")
        || lower.contains("dimension coverage gap:")
        || lower.contains("subtask_depth_gap:")
        || lower.contains("subtask_probe_gap:")
        || lower.contains("dimension_gap:")
        || lower.contains("runtime recovery failed")
        || lower.contains("runtime execution failed")
        || lower.contains("returned deterministic emergency conclusion")
        || lower.contains("retrieve source slot timed out")
        || lower.contains("contract_invalid:")
        || lower.contains("prompt:")
}

fn strip_pm_inline_diagnostics(line: &str) -> String {
    let mut cleaned = line.to_string();
    let diagnostic_patterns = [
        "外部检索这轮有缺口：WebSearch 没配置成功，",
        "外部检索这轮有缺口：WebSearch 未配置成功，",
        "外部检索这轮有缺口: WebSearch 没配置成功，",
        "外部检索这轮有缺口: WebSearch 未配置成功，",
        "外部检索这轮有缺口：WebSearch 没配置成功。",
        "外部检索这轮有缺口：WebSearch 未配置成功。",
        "外部检索这轮有缺口: WebSearch 没配置成功。",
        "外部检索这轮有缺口: WebSearch 未配置成功。",
        "WebSearch 没配置成功，",
        "WebSearch 未配置成功，",
        "WebSearch 没配置成功。",
        "WebSearch 未配置成功。",
        "WebSearch 没配置成功",
        "WebSearch 未配置成功",
        "WebSearch requires a configured search provider in the database for Chat/PM; env-based WebSearch fallback is disabled for assistant scenarios",
    ];
    for pattern in diagnostic_patterns {
        cleaned = cleaned.replace(pattern, "");
    }
    cleaned.trim().to_string()
}

pub fn extract_pm_visible_answer_text(answer_text: &str) -> String {
    let mut lines = Vec::<String>::new();
    for line in answer_text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            continue;
        }
        let cleaned = strip_pm_inline_diagnostics(trimmed);
        if cleaned.trim().is_empty() || is_pm_visible_output_noise(cleaned.trim()) {
            continue;
        }
        lines.push(cleaned);
    }
    let joined = lines.join("\n").trim().to_string();
    if joined.is_empty() {
        return String::new();
    }
    joined
}

pub fn tokenize_for_match(input: &str) -> Vec<String> {
    fn push_cjk_tokens(buffer: &str, tokens: &mut Vec<String>) {
        let chars = buffer.chars().collect::<Vec<_>>();
        if chars.len() < 2 {
            return;
        }
        tokens.push(buffer.to_string());
        for width in 2..=4.min(chars.len()) {
            for window in chars.windows(width) {
                tokens.push(window.iter().collect());
            }
        }
    }
    let mut tokens = Vec::new();
    let lowered = input.to_ascii_lowercase();
    for token in lowered
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| part.len() >= 3)
    {
        tokens.push(token.to_string());
    }

    let mut cjk_buf = String::new();
    for ch in input.chars() {
        let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&ch)
            || ('\u{3400}'..='\u{4dbf}').contains(&ch)
            || ('\u{3040}'..='\u{30ff}').contains(&ch)
            || ('\u{ac00}'..='\u{d7af}').contains(&ch);
        if is_cjk {
            cjk_buf.push(ch);
        } else if !cjk_buf.is_empty() {
            push_cjk_tokens(&cjk_buf, &mut tokens);
            cjk_buf.clear();
        }
    }
    if !cjk_buf.is_empty() {
        push_cjk_tokens(&cjk_buf, &mut tokens);
    }

    tokens.sort();
    tokens.dedup();
    tokens
}
