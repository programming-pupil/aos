use std::collections::HashSet;

pub fn compact_pm_query_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn truncate_pm_query_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

fn count_digits(input: &str) -> usize {
    input.chars().filter(|ch| ch.is_ascii_digit()).count()
}

fn count_ascii_alpha(input: &str) -> usize {
    input.chars().filter(|ch| ch.is_ascii_alphabetic()).count()
}

fn count_cjk(input: &str) -> usize {
    input
        .chars()
        .filter(|ch| ('\u{4e00}'..='\u{9fff}').contains(ch))
        .count()
}

fn count_separators(input: &str) -> usize {
    input
        .chars()
        .filter(|ch| {
            matches!(
                ch,
                ',' | '，'
                    | ';'
                    | '；'
                    | ':'
                    | '：'
                    | '/'
                    | '|'
                    | '+'
                    | '~'
                    | '～'
                    | '%'
                    | '$'
                    | '<'
                    | '>'
                    | '='
                    | '≤'
                    | '≥'
            )
        })
        .count()
}

fn has_internal_contract_noise(lower: &str) -> bool {
    [
        "exec_constraints",
        "task_graph",
        "retrieve_constraints",
        "repair_scope",
        "report_json",
        "pm_llm_expert_review",
        "pm_orch_internal",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn has_table_fragment_markers(input: &str, lower: &str) -> bool {
    let cjk_headings = ["一、", "二、", "三、", "四、", "五、", "六、", "七、"];
    let dense_table_words = [
        "日均",
        "占比",
        "分层",
        "结论",
        "当前情况",
        "用户类型",
        "指标",
        "成本",
        "收入",
        "策略价值",
    ];
    let ascii_table_words = [
        " daily ",
        " avg ",
        " share ",
        " segment ",
        " cohort ",
        " metric ",
        " revenue ",
        " cost ",
        " conclusion ",
        " table ",
    ];
    let heading_hits = cjk_headings
        .iter()
        .filter(|marker| input.contains(**marker))
        .count();
    let cjk_hits = dense_table_words
        .iter()
        .filter(|marker| input.contains(**marker))
        .count();
    let padded = format!(" {lower} ");
    let ascii_hits = ascii_table_words
        .iter()
        .filter(|marker| padded.contains(**marker))
        .count();
    heading_hits >= 2 || cjk_hits >= 5 || ascii_hits >= 5
}

fn has_metric_glue(input: &str) -> bool {
    let mut prev: Option<char> = None;
    let mut transitions = 0usize;
    for ch in input.chars() {
        if let Some(last) = prev {
            let last_alnum_or_cjk =
                last.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&last);
            let ch_alnum_or_cjk =
                ch.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&ch);
            if last_alnum_or_cjk && ch_alnum_or_cjk {
                let mixed = (last.is_ascii_digit() && !ch.is_ascii_digit())
                    || (!last.is_ascii_digit() && ch.is_ascii_digit());
                if mixed {
                    transitions = transitions.saturating_add(1);
                }
            }
        }
        prev = Some(ch);
    }
    transitions >= 8
}

fn has_truncation_or_placeholder_marker(input: &str, lower: &str) -> bool {
    input.contains("...")
        || input.contains('…')
        || lower.contains("+ more")
        || lower.contains(" more.")
        || lower.contains(" more,")
        || lower.contains(" more;")
}

fn is_probably_pasted_report_fragment(input: &str) -> bool {
    let compact = compact_pm_query_whitespace(input);
    let len = compact.chars().count();
    let lower = compact.to_ascii_lowercase();
    if has_internal_contract_noise(&lower) {
        return true;
    }
    if has_truncation_or_placeholder_marker(&compact, &lower) {
        return true;
    }
    if len <= 90 {
        return false;
    }
    let digits = count_digits(&compact);
    let separators = count_separators(&compact);
    let alpha = count_ascii_alpha(&compact);
    let cjk = count_cjk(&compact);
    let text_units = alpha + cjk;
    let digit_ratio = digits as f64 / len.max(1) as f64;
    let separator_ratio = separators as f64 / len.max(1) as f64;
    let tableish = has_table_fragment_markers(&compact, &lower);
    let glued = has_metric_glue(&compact);
    let has_ellipsis_or_truncation = false;

    if len > 115 && (tableish || (glued && digits >= 8) || has_ellipsis_or_truncation) {
        return true;
    }
    if len > 120 && digit_ratio > 0.18 && separators >= 4 {
        return true;
    }
    if len > 120 && separator_ratio > 0.08 && tableish {
        return true;
    }
    if len > 160 && digits >= 12 && text_units >= 20 {
        return true;
    }
    false
}

pub fn pm_query_looks_searchable(input: &str) -> bool {
    let compact = compact_pm_query_whitespace(input);
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return false;
    }
    let len = trimmed.chars().count();
    if len > 150 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if has_internal_contract_noise(&lower) {
        return false;
    }
    if has_truncation_or_placeholder_marker(trimmed, &lower) {
        return false;
    }
    if is_probably_pasted_report_fragment(trimmed) {
        return false;
    }
    let digits = count_digits(trimmed);
    let separators = count_separators(trimmed);
    if len > 80 && digits >= 10 && separators >= 4 && has_metric_glue(trimmed) {
        return false;
    }
    true
}

pub fn sanitize_pm_search_query(raw: &str, max_chars: usize) -> Option<String> {
    let compact = compact_pm_query_whitespace(raw);
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if has_internal_contract_noise(&lower)
        || has_truncation_or_placeholder_marker(trimmed, &lower)
        || is_probably_pasted_report_fragment(trimmed)
    {
        return None;
    }
    let limit = max_chars.clamp(24, 150);
    let shortened = truncate_pm_query_chars(trimmed, limit)
        .trim()
        .trim_matches(|ch: char| {
            ch.is_ascii_punctuation() || matches!(ch, '，' | '。' | '；' | '：' | '、' | '…' | '—')
        })
        .trim()
        .to_string();
    if shortened.is_empty() || !pm_query_looks_searchable(&shortened) {
        return None;
    }
    Some(shortened)
}

pub fn sanitize_pm_search_queries<I, S>(
    items: I,
    fallback: Option<&str>,
    max_items: usize,
) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for item in items {
        let Some(cleaned) = sanitize_pm_search_query(item.as_ref(), 140) else {
            continue;
        };
        let key = cleaned.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(cleaned);
        }
        if out.len() >= max_items {
            return out;
        }
    }
    if out.is_empty() {
        if let Some(fallback) = fallback.and_then(|raw| sanitize_pm_search_query(raw, 140)) {
            out.push(fallback);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_pasted_metric_table_fragments() {
        let raw = "20260608~20260621 日均数据 成本只算 UA+UG 印尼网赚单机休闲矩阵产品 结果是： 算法结果hybridROI 不如原加权平均，放... 三、按 eCPM 用户价值分层 eCPM分层日均UVUV占比日均收入收入占比日均UA+UG成本ROIAIPU 四、按 A";
        assert!(sanitize_pm_search_query(raw, 140).is_none());
        assert!(!pm_query_looks_searchable(raw));
    }

    #[test]
    fn rejects_truncated_report_snippets_even_when_short() {
        let raw = "是印尼网赚单机休闲 App 矩阵 约 5 到 6 个产品 当前策略制定需要特别注意：不能把所有用户一刀切，... 三、AIPU 分层结论 分层策略 实验";
        assert!(sanitize_pm_search_query(raw, 140).is_none());
        assert!(!pm_query_looks_searchable(raw));
    }

    #[test]
    fn rejects_generic_table_fragments_without_domain_keywords() {
        let raw = "Q2 enterprise SaaS daily active accounts 12345 conversion rate7.2% churn3.1% segmentSMBshare43% revenue$880000 cost$230000 conclusion expansion motion table metric benchmark + more";
        assert!(sanitize_pm_search_query(raw, 140).is_none());
    }

    #[test]
    fn keeps_concise_semantic_queries() {
        let raw = "B2B SaaS onboarding activation benchmark case study";
        assert_eq!(
            sanitize_pm_search_query(raw, 140).as_deref(),
            Some("B2B SaaS onboarding activation benchmark case study")
        );
    }
}
