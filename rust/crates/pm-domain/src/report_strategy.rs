use std::{collections::BTreeSet, sync::OnceLock};

use regex::Regex;
use serde_json::Value;

use crate::query_hygiene::{
    sanitize_pm_search_queries, sanitize_pm_search_query, truncate_pm_query_chars,
};

#[derive(Debug, Clone)]
pub struct PmReportStrategySignal {
    pub matched: bool,
    pub score: usize,
    pub reasons: Vec<String>,
    pub primary_terms: Vec<String>,
    pub targeted_queries: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PmReportSemanticExtraction {
    pub domain_terms: Vec<String>,
    pub product_terms: Vec<String>,
    pub metric_terms: Vec<String>,
    pub objective_terms: Vec<String>,
    pub constraint_terms: Vec<String>,
    pub segment_terms: Vec<String>,
    pub mechanism_terms: Vec<String>,
    pub prior_experiment_terms: Vec<String>,
    pub key_sentences: Vec<String>,
    pub search_queries: Vec<String>,
    pub source: String,
}

impl PmReportSemanticExtraction {
    pub fn from_value(value: &Value) -> Option<Self> {
        let source = value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("llm_semantic_extract")
            .to_string();
        let extraction = Self {
            domain_terms: read_string_array(value, "domainTerms", 8),
            product_terms: read_string_array(value, "productTerms", 8),
            metric_terms: read_string_array(value, "metricTerms", 12),
            objective_terms: read_string_array(value, "objectiveTerms", 8),
            constraint_terms: read_string_array(value, "constraintTerms", 8),
            segment_terms: read_string_array(value, "segmentTerms", 8),
            mechanism_terms: read_string_array(value, "mechanismTerms", 8),
            prior_experiment_terms: read_string_array(value, "priorExperimentTerms", 8),
            key_sentences: read_string_array(value, "keySentences", 10),
            search_queries: read_string_array(value, "searchQueries", 8),
            source,
        };
        extraction.has_useful_signal().then_some(extraction)
    }

    pub fn has_useful_signal(&self) -> bool {
        !self.domain_terms.is_empty()
            || !self.product_terms.is_empty()
            || !self.metric_terms.is_empty()
            || !self.objective_terms.is_empty()
            || !self.constraint_terms.is_empty()
            || !self.segment_terms.is_empty()
            || !self.mechanism_terms.is_empty()
            || !self.key_sentences.is_empty()
            || !self.search_queries.is_empty()
    }
}

#[derive(Debug, Clone)]
struct PmConcept {
    key: &'static str,
    labels: &'static [&'static str],
}

#[derive(Debug, Clone)]
struct PmConceptRegistry {
    metrics: &'static [PmConcept],
    segment_terms: &'static [&'static str],
    strategy_terms: &'static [&'static str],
    history_terms: &'static [&'static str],
    section_terms: &'static [&'static str],
    context_terms: &'static [PmConcept],
    metric_query_terms: &'static [PmConcept],
}

const DEFAULT_PM_CONCEPT_REGISTRY: PmConceptRegistry = PmConceptRegistry {
    metrics: &[
        PmConcept {
            key: "roi",
            labels: &["roi", "ROI"],
        },
        PmConcept {
            key: "revenue",
            labels: &["收入", "revenue"],
        },
        PmConcept {
            key: "cost",
            labels: &["成本", "cost"],
        },
        PmConcept {
            key: "retention",
            labels: &["留存", "次留", "retention"],
        },
        PmConcept {
            key: "conversion",
            labels: &["转化", "转化率", "conversion"],
        },
        PmConcept {
            key: "churn",
            labels: &["流失", "流失率", "churn"],
        },
        PmConcept {
            key: "ltv",
            labels: &["ltv", "LTV"],
        },
        PmConcept {
            key: "cac",
            labels: &["cac", "CAC", "获客成本", "获取成本"],
        },
        PmConcept {
            key: "arr_mrr",
            labels: &["arr", "ARR", "mrr", "MRR"],
        },
        PmConcept {
            key: "gross_margin",
            labels: &["毛利", "毛利率", "gross margin"],
        },
        PmConcept {
            key: "nps",
            labels: &["nps", "NPS", "满意度"],
        },
    ],
    segment_terms: &[
        "分层",
        "人群",
        "用户类型",
        "客户类型",
        "场景",
        "客群",
        "新老",
        "segment",
        "cohort",
        "persona",
        "scenario",
    ],
    strategy_terms: &[
        "策略",
        "玩法",
        "诉求",
        "方案",
        "实验",
        "提升",
        "strategy",
        "experiment",
        "playbook",
        "mechanic",
    ],
    history_terms: &[
        "之前试过",
        "结果",
        "结论",
        "放弃",
        "风险",
        "不能",
        "当前已有",
        "tried",
        "result",
        "conclusion",
        "guardrail",
    ],
    section_terms: &[
        "一、",
        "二、",
        "三、",
        "四、",
        "五、",
        "六、",
        "当前",
        "关键结论",
    ],
    context_terms: &[],
    metric_query_terms: &[
        PmConcept {
            key: "retention",
            labels: &["次留", "retention"],
        },
        PmConcept {
            key: "duration",
            labels: &["时长", "周期", "耗时", "duration", "time spent"],
        },
        PmConcept {
            key: "fatigue or saturation",
            labels: &["疲劳", "饱和", "fatigue", "saturation"],
        },
        PmConcept {
            key: "segmentation",
            labels: &["分层", "segmentation"],
        },
        PmConcept {
            key: "new vs existing cohorts",
            labels: &["新老用户", "新老客户", "new user", "existing user"],
        },
        PmConcept {
            key: "pricing",
            labels: &["定价", "价格", "pricing"],
        },
        PmConcept {
            key: "activation",
            labels: &["激活", "activation", "onboarding"],
        },
        PmConcept {
            key: "funnel",
            labels: &["漏斗", "funnel"],
        },
    ],
};

fn default_concept_registry() -> &'static PmConceptRegistry {
    &DEFAULT_PM_CONCEPT_REGISTRY
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut out = input.chars().take(max_chars).collect::<String>();
    if input.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

fn compact_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn lower_ascii(input: &str) -> String {
    input.to_ascii_lowercase()
}

fn contains_any_ascii(input: &str, tokens: &[&str]) -> bool {
    let lower = lower_ascii(input);
    tokens.iter().any(|token| lower.contains(token))
}

fn contains_any_raw(input: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| input.contains(token))
}

fn count_numeric_markers(input: &str) -> usize {
    let mut count = 0usize;
    let mut in_run = false;
    for ch in input.chars() {
        let numeric = ch.is_ascii_digit() || matches!(ch, '%' | '$');
        if numeric && !in_run {
            count = count.saturating_add(1);
        }
        in_run = numeric;
    }
    count
}

fn push_unique(out: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if out.iter().any(|item| item == trimmed) {
        return;
    }
    out.push(trimmed.to_string());
}

fn read_string_array(value: &Value, key: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(items) = value.get(key).and_then(Value::as_array) {
        for item in items.iter().take(cap.saturating_mul(2)) {
            let Some(raw) = item.as_str() else {
                continue;
            };
            let cleaned = truncate_chars(&compact_whitespace(raw), 180);
            push_unique(&mut out, cleaned);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

fn split_report_chunks(input: &str) -> Vec<String> {
    let mut chunks = Vec::<String>::new();
    for line in input
        .replace("一、", "\n一、")
        .replace("二、", "\n二、")
        .replace("三、", "\n三、")
        .replace("四、", "\n四、")
        .replace("五、", "\n五、")
        .replace("六、", "\n六、")
        .lines()
    {
        let line = compact_whitespace(line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.chars().count() <= 260 {
            push_unique(&mut chunks, line);
            continue;
        }
        for part in line.split(['。', '；', ';']) {
            let part = compact_whitespace(part);
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            push_unique(&mut chunks, truncate_chars(part, 260));
        }
    }
    chunks
}

fn split_clause_candidates(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in split_report_chunks(input) {
        for part in chunk.split(['。', '；', ';', '，']) {
            let part = compact_whitespace(part);
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            push_unique(&mut out, truncate_chars(part, 140));
        }
    }
    out
}

fn collect_chunks_with_tokens(input: &str, tokens: &[&str], cap: usize) -> Vec<String> {
    let mut out = Vec::<String>::new();
    for chunk in split_report_chunks(input) {
        if contains_any_ascii(&chunk, tokens) || contains_any_raw(&chunk, tokens) {
            push_unique(&mut out, truncate_chars(&chunk, 220));
        }
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn is_context_stopword(token: &str) -> bool {
    let normalized = token.trim().to_ascii_lowercase();
    if normalized.chars().count() <= 1 {
        return true;
    }
    matches!(
        normalized.as_str(),
        "roi"
            | "roas"
            | "aipu"
            | "ecpm"
            | "arpu"
            | "dau"
            | "ua"
            | "ug"
            | "and"
            | "the"
            | "with"
            | "for"
            | "app"
            | "id"
    ) || token
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '_' | '-' | '/'))
}

fn push_context_candidate(out: &mut Vec<String>, value: impl Into<String>) {
    let compacted = compact_whitespace(&value.into());
    let trimmed = compacted
        .trim_matches(|ch: char| {
            ch.is_ascii_punctuation()
                || matches!(
                    ch,
                    '：' | '，' | '。' | '；' | '、' | '（' | '）' | '(' | ')' | '[' | ']'
                )
        })
        .trim();
    if trimmed.is_empty() || is_context_stopword(trimmed) {
        return;
    }
    let char_count = trimmed.chars().count();
    let numeric_count = count_numeric_markers(trimmed);
    let digit_count = trimmed.chars().filter(|ch| ch.is_ascii_digit()).count();
    let lower = lower_ascii(trimmed);
    let looks_like_measurement_window = (trimmed.contains('~') || trimmed.contains('～'))
        && digit_count >= 8
        && contains_any_raw(trimmed, &["日均", "数据", "成本", "口径"]);
    let looks_like_metric_clause = numeric_count >= 2
        && contains_any_ascii(
            &lower,
            &[
                "roi", "roas", "aipu", "ecpm", "arpu", "dau", "cac", "mrr", "churn",
            ],
        );
    if looks_like_measurement_window || (char_count > 24 && looks_like_metric_clause) {
        return;
    }
    let ascii_count = trimmed.chars().filter(|ch| ch.is_ascii()).count();
    let id_punct_count = trimmed
        .chars()
        .filter(|ch| matches!(ch, '_' | '/' | '\\'))
        .count();
    if id_punct_count > 0 && ascii_count.saturating_mul(2) >= trimmed.chars().count() {
        return;
    }
    if !(2..=48).contains(&char_count) {
        return;
    }
    push_unique(out, trimmed);
}

fn collect_report_context_terms(input: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::<String>::new();
    for chunk in split_report_chunks(input).into_iter().take(18) {
        for marker in [
            "我们是",
            "产品",
            "业务背景",
            "基于",
            "we are",
            "our product is",
            "current product is",
            "product is",
            "based on",
            "for ",
        ] {
            let Some((_, tail)) = chunk.split_once(marker) else {
                continue;
            };
            for part in tail.split(['，', ',', '。', ';', '；', '：', ':']).take(3) {
                push_context_candidate(&mut out, part);
                if out.len() >= cap {
                    return out;
                }
            }
        }
    }
    // Fallback for English reports: keep a few noun-like title tokens without
    // needing industry/country-specific Rust keywords.
    for token in input
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\''
                )
        })
        .take(160)
    {
        if token.chars().any(|ch| ch.is_ascii_alphabetic())
            && token.chars().count() >= 4
            && token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            push_context_candidate(&mut out, token);
        }
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn value_after_alias_tail(tail: &str) -> Option<String> {
    let mut started = false;
    let mut value = String::new();
    for ch in tail.chars().take(32) {
        if !started {
            if ch.is_ascii_digit() || ch == '$' || ch == '<' || ch == '>' || ch == '=' {
                started = true;
                value.push(ch);
            } else if ch.is_whitespace() || matches!(ch, ':' | '：' | '=' | '约') {
                continue;
            } else {
                return None;
            }
        } else if ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '.' | ',' | '%' | '$' | '/' | '~' | '-' | '+' | '<' | '>' | '='
            )
        {
            value.push(ch);
        } else {
            break;
        }
    }
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn value_after_alias(input: &str, alias: &str) -> Option<String> {
    let lower = input.to_ascii_lowercase();
    let alias_lower = alias.to_ascii_lowercase();
    for (idx, _) in lower.match_indices(&alias_lower) {
        let start = idx + alias.len();
        let Some(tail) = input.get(start..) else {
            continue;
        };
        if let Some(value) = value_after_alias_tail(tail) {
            return Some(value);
        }
    }
    None
}

fn push_metric_if_present(
    metrics: &mut Vec<serde_json::Value>,
    input: &str,
    name: &str,
    aliases: &[&str],
) {
    for alias in aliases {
        if let Some(value) = value_after_alias(input, alias) {
            push_metric_json_unique(metrics, name, &value, alias);
            return;
        }
    }
}

fn metric_ascii_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?P<name>[A-Za-z][A-Za-z0-9_./+-]{1,32})(?:\s*(?:=|:|：)\s*|\s+)(?P<value>\$?\d[\d,]*(?:\.\d+)?%?(?:[kmb])?(?:/\d+(?:/\d+)?)?)",
        )
        .expect("valid ascii metric regex")
    })
}

fn metric_ascii_compact_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?P<name>[A-Za-z][A-Za-z._+-]*[A-Za-z])(?P<value>\$?\d[\d,]*(?:\.\d+)?%?(?:[kmb])?(?:/\d+(?:/\d+)?)?)",
        )
        .expect("valid compact ascii metric regex")
    })
}

fn metric_cjk_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?P<name>[\p{Han}A-Za-z][\p{Han}A-Za-z0-9_+/ -]{1,24}?)(?:约|为|是|=|:|：)?\s*(?P<value>\$?\d[\d,]*(?:\.\d+)?%?)",
        )
        .expect("valid cjk metric regex")
    })
}

fn normalize_dynamic_metric_name(raw: &str) -> Option<String> {
    let trimmed = compact_whitespace(raw)
        .trim_matches(|ch: char| {
            ch.is_ascii_punctuation()
                || matches!(
                    ch,
                    '：' | '，' | '。' | '；' | '、' | '（' | '）' | '(' | ')' | '[' | ']'
                )
        })
        .trim()
        .trim_start_matches("当前")
        .trim_start_matches("过去")
        .trim_start_matches("近")
        .trim_start_matches("日均")
        .trim_start_matches("指标")
        .trim()
        .to_string();
    if !(2..=36).contains(&trimmed.chars().count()) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "the"
            | "and"
            | "for"
            | "with"
            | "past"
            | "last"
            | "next"
            | "current"
            | "previous"
            | "day"
            | "days"
            | "week"
            | "weeks"
            | "month"
            | "months"
            | "year"
            | "years"
            | "report"
            | "data"
    ) {
        return None;
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '_' | '-' | '/' | '.'))
    {
        return None;
    }
    Some(trimmed)
}

fn normalize_metric_value(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_matches(|ch: char| matches!(ch, ',' | '，' | '。' | ';' | '；'))
        .to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

fn push_metric_json_unique(
    metrics: &mut Vec<serde_json::Value>,
    name: &str,
    value: &str,
    source_alias: &str,
) {
    let Some(name) = normalize_dynamic_metric_name(name) else {
        return;
    };
    let Some(value) = normalize_metric_value(value) else {
        return;
    };
    let name_key = name.to_ascii_lowercase();
    let value_key = value.to_ascii_lowercase();
    if metrics.iter().any(|item| {
        let existing_name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let existing_value = item
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        existing_name == name_key && existing_value == value_key
    }) {
        return;
    }
    metrics.push(serde_json::json!({
        "name": name,
        "value": value,
        "sourceAlias": source_alias,
    }));
}

fn push_dynamic_metric_candidates(metrics: &mut Vec<serde_json::Value>, input: &str) {
    for chunk in split_report_chunks(input).into_iter().take(24) {
        for caps in metric_ascii_regex().captures_iter(&chunk) {
            let Some(name) = caps.name("name").map(|m| m.as_str()) else {
                continue;
            };
            let Some(value) = caps.name("value").map(|m| m.as_str()) else {
                continue;
            };
            push_metric_json_unique(metrics, name, value, "dynamic_metric");
            if metrics.len() >= 24 {
                return;
            }
        }
        for caps in metric_ascii_compact_regex().captures_iter(&chunk) {
            let Some(name) = caps.name("name").map(|m| m.as_str()) else {
                continue;
            };
            let Some(value) = caps.name("value").map(|m| m.as_str()) else {
                continue;
            };
            push_metric_json_unique(metrics, name, value, "dynamic_metric_compact");
            if metrics.len() >= 24 {
                return;
            }
        }
        for caps in metric_cjk_regex().captures_iter(&chunk) {
            let Some(name) = caps.name("name").map(|m| m.as_str()) else {
                continue;
            };
            let Some(value) = caps.name("value").map(|m| m.as_str()) else {
                continue;
            };
            push_metric_json_unique(metrics, name, value, "dynamic_metric");
            if metrics.len() >= 24 {
                return;
            }
        }
    }
}

fn extract_dynamic_metric_terms(input: &str) -> Vec<String> {
    let mut metrics = Vec::new();
    push_dynamic_metric_candidates(&mut metrics, input);
    let mut out = metrics
        .into_iter()
        .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_string))
        .fold(Vec::new(), |mut out, item| {
            push_unique(&mut out, item);
            out
        });
    for token in extract_ascii_metric_tokens(input) {
        push_unique(&mut out, token);
        if out.len() >= 16 {
            break;
        }
    }
    out
}

fn extract_ascii_metric_tokens(input: &str) -> Vec<String> {
    let stopwords = [
        "the",
        "and",
        "for",
        "with",
        "from",
        "that",
        "this",
        "past",
        "last",
        "next",
        "current",
        "previous",
        "day",
        "days",
        "week",
        "weeks",
        "month",
        "months",
        "year",
        "years",
        "user",
        "users",
        "product",
        "strategy",
        "experiment",
        "report",
        "data",
        "case",
        "study",
        "benchmark",
        "app",
    ];
    let mut out = Vec::<String>::new();
    for token in input.split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))) {
        let trimmed = token.trim_matches(['_', '-']);
        let len = trimmed.chars().count();
        if !(2..=24).contains(&len) || !trimmed.chars().any(|ch| ch.is_ascii_alphabetic()) {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if stopwords.iter().any(|item| *item == lower) {
            continue;
        }
        let is_metricish = trimmed
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
            || default_concept_registry()
                .metrics
                .iter()
                .chain(default_concept_registry().metric_query_terms.iter())
                .any(|concept| {
                    concept
                        .labels
                        .iter()
                        .any(|label| lower == label.to_ascii_lowercase())
                });
        if is_metricish {
            push_unique(&mut out, trimmed);
        }
        if out.len() >= 12 {
            break;
        }
    }
    out
}

fn detect_objectives(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in split_report_chunks(input) {
        let lower = lower_ascii(&chunk);
        let looks_like_objective = contains_any_raw(
            &chunk,
            &[
                "目标", "诉求", "希望", "提升", "增长", "降低", "减少", "优化", "改善",
            ],
        ) || contains_any_ascii(
            &lower,
            &[
                "objective",
                "goal",
                "target",
                "increase",
                "improve",
                "reduce",
                "decrease",
                "optimize",
            ],
        );
        if looks_like_objective {
            push_unique(&mut out, truncate_chars(&chunk, 120));
        }
        if out.len() >= 6 {
            break;
        }
    }
    if out.is_empty() && contains_any_raw(input, &["目标", "诉求", "提升"]) {
        push_unique(&mut out, "基于用户报告提升核心业务指标");
    }
    out
}

fn detect_guardrails(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in split_clause_candidates(input) {
        let lower = lower_ascii(&chunk);
        let looks_like_guardrail = contains_any_raw(
            &chunk,
            &[
                "不能下降",
                "不能上升",
                "不能降低",
                "不下降",
                "不低于",
                "不高于",
                "保护指标",
                "约束",
                "底线",
                "风险",
            ],
        ) || contains_any_ascii(
            &lower,
            &[
                "guardrail",
                "must not",
                "cannot",
                "should not",
                "not decrease",
                "not increase",
                "constraint",
                "risk",
            ],
        );
        if looks_like_guardrail {
            push_unique(&mut out, truncate_chars(&chunk, 120));
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn detect_existing_mechanics(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let markers = [
        "当前已有",
        "已有能力",
        "已有玩法",
        "已存在",
        "当前已经存在",
        "current capabilities",
        "current mechanics",
        "existing capabilities",
        "existing mechanics",
    ];
    for chunk in split_report_chunks(input) {
        let lower = lower_ascii(&chunk);
        if !markers
            .iter()
            .any(|marker| chunk.contains(marker) || lower.contains(marker))
        {
            continue;
        }
        let mut tails = Vec::<String>::new();
        for marker in markers {
            if let Some((_, tail)) = chunk.split_once(marker) {
                push_unique(&mut tails, tail);
            } else if let Some((_, tail)) = lower.split_once(marker) {
                if let Some(original_tail) = chunk.get(chunk.len().saturating_sub(tail.len())..) {
                    push_unique(&mut tails, original_tail);
                }
            }
        }
        if tails.is_empty() {
            tails.push(chunk);
        }
        for tail in tails {
            for part in tail.split(['、', '，', ',', ';', '；', '。']) {
                let mut item = compact_whitespace(part);
                for prefix in [
                    "玩法",
                    "能力",
                    "模块",
                    "策略",
                    "mechanics",
                    "capabilities",
                    "features",
                ] {
                    let trimmed = item.trim_start();
                    if trimmed.starts_with(prefix) && trimmed.chars().count() > prefix.len() + 1 {
                        item = trimmed[prefix.len()..].trim().to_string();
                    }
                }
                let item = item
                    .trim_matches(|ch: char| {
                        ch.is_ascii_punctuation()
                            || matches!(ch, '：' | ':' | '，' | '。' | '；' | '、')
                    })
                    .trim()
                    .to_string();
                if item.chars().count() >= 2
                    && item.chars().count() <= 80
                    && !contains_any_raw(&item, &["不建议", "不要", "不能"])
                {
                    push_unique(&mut out, item);
                }
                if out.len() >= 8 {
                    return out;
                }
            }
        }
    }
    out
}

fn extract_experiment_candidate_names(chunk: &str) -> Vec<String> {
    let common = [
        "roi",
        "roas",
        "aipu",
        "ecpm",
        "arpu",
        "dau",
        "ua",
        "ug",
        "id",
        "app",
        "new",
        "active",
        "cost",
        "result",
        "tried",
        "test",
        "experiment",
        "strategy",
        "model",
    ];
    let mut out = Vec::new();
    for token in
        chunk.split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')))
    {
        let trimmed = token.trim_matches(['.', '-', '_']);
        if !(2..=32).contains(&trimmed.chars().count()) {
            continue;
        }
        if !trimmed.chars().any(|ch| ch.is_ascii_alphabetic()) {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if common.iter().any(|item| *item == lower) {
            continue;
        }
        push_unique(&mut out, trimmed);
    }
    out
}

fn detect_failed_experiments(input: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut seen_names = Vec::<String>::new();
    for chunk in split_report_chunks(input) {
        let lower = lower_ascii(&chunk);
        let looks_like_history = contains_any_raw(
            &chunk,
            &[
                "试过", "实验", "结果", "结论", "放弃", "下降", "不如", "风险",
            ],
        ) || contains_any_ascii(
            &lower,
            &[
                "tried",
                "experiment",
                "result",
                "failed",
                "decline",
                "worse",
                "risk",
            ],
        );
        if !looks_like_history {
            continue;
        }
        let names = extract_experiment_candidate_names(&chunk);
        if names.is_empty() && (chunk.contains("试过") || lower.contains("experiment")) {
            out.push(serde_json::json!({
                "name": "用户报告中的历史尝试",
                "result": truncate_chars(&chunk, 180),
                "lesson": "保留这段一手实验约束，后续策略不能重复已验证的失败方向",
            }));
        }
        for name in names {
            let name_key = name.to_ascii_lowercase();
            if seen_names.iter().any(|item| item == &name_key) {
                continue;
            }
            push_unique(&mut seen_names, name_key);
            out.push(serde_json::json!({
                "name": name,
                "result": truncate_chars(&chunk, 180),
                "lesson": "报告显示该历史尝试未完全满足核心目标，后续建议需映射其风险和保护指标",
            }));
            if out.len() >= 6 {
                return out;
            }
        }
    }
    out
}

fn detect_anti_patterns(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in split_report_chunks(input) {
        if contains_any_raw(
            &chunk,
            &[
                "不能",
                "不要",
                "不建议",
                "不应",
                "避免",
                "风险",
                "下降",
                "伤",
            ],
        ) || contains_any_ascii(
            &chunk,
            &[
                "must not",
                "do not",
                "avoid",
                "risk",
                "decline",
                "decrease",
                "guardrail",
            ],
        ) {
            push_unique(&mut out, truncate_chars(&chunk, 160));
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn detect_opportunity_cohorts(input: &str) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for chunk in split_clause_candidates(input) {
        let lower = lower_ascii(&chunk);
        let has_segment_signal = contains_any_raw(
            &chunk,
            &[
                "分层",
                "人群",
                "用户类型",
                "客群",
                "场景",
                "关键",
                "高价值",
                "低价值",
                "核心",
                "亏损",
            ],
        ) || contains_any_ascii(
            &lower,
            &[
                "segment",
                "cohort",
                "persona",
                "customer group",
                "scenario",
                "high value",
                "low value",
                "loss",
                "profitable",
            ],
        );
        let has_metric_signal = count_numeric_markers(&chunk) >= 1
            || !detect_metric_terms(&chunk).is_empty()
            || contains_any_raw(&chunk, &["高", "低", "中", "新", "老"]);
        if !has_segment_signal || !has_metric_signal {
            continue;
        }
        let mut label = chunk.trim().to_string();
        for marker in [
            "结论",
            "策略价值",
            "建议",
            "明显",
            "接近",
            "正向",
            "核心",
            "不值得",
            "不应",
            "需",
            "需要",
            "can",
            "should",
            "recommend",
        ] {
            if let Some((head, _)) = label.split_once(marker) {
                label = head.trim().to_string();
            }
        }
        label = label
            .trim_matches(|ch: char| ch.is_ascii_punctuation() || matches!(ch, '：' | ':' | '，'))
            .trim()
            .to_string();
        if !(2..=80).contains(&label.chars().count()) {
            label = truncate_chars(&chunk, 56);
        }
        if out.iter().any(|item| {
            item.get("cohort")
                .and_then(Value::as_str)
                .is_some_and(|existing| existing == label)
        }) {
            continue;
        }
        out.push(serde_json::json!({
            "cohort": label,
            "why": truncate_chars(&chunk, 180),
            "strategyHint": "根据该人群/场景的一手指标、约束和历史结论设计差异化实验，而不是套用统一策略",
        }));
        if out.len() >= 6 {
            break;
        }
    }
    out
}

pub fn extract_pm_first_party_evidence(question: &str) -> serde_json::Value {
    let mut metrics = Vec::new();
    push_metric_if_present(&mut metrics, question, "revenue", &["收入", "revenue"]);
    push_metric_if_present(&mut metrics, question, "cost", &["成本", "总成本", "cost"]);
    push_metric_if_present(&mut metrics, question, "ROI", &["ROI"]);
    push_dynamic_metric_candidates(&mut metrics, question);
    let context_terms = detect_context_terms(question);
    let dynamic_metric_terms = extract_dynamic_metric_terms(question);
    let mut snippet_terms = vec![
        "分层",
        "人群",
        "用户类型",
        "客群",
        "场景",
        "细分",
        "策略",
        "玩法",
        "机制",
        "能力",
        "实验",
        "目标",
        "诉求",
        "约束",
        "风险",
        "成本",
        "收入",
        "留存",
        "转化",
        "guardrail",
        "segment",
        "cohort",
        "experiment",
        "strategy",
        "objective",
        "goal",
    ];
    for term in &context_terms {
        snippet_terms.push(term.as_str());
    }
    for term in &dynamic_metric_terms {
        snippet_terms.push(term.as_str());
    }
    let raw_evidence_snippets = collect_chunks_with_tokens(question, &snippet_terms, 12);
    let segment_snippets = detect_query_cohort_terms(question);

    serde_json::json!({
        "schemaVersion": "pm_first_party_report.v1",
        "evidencePriority": "primary",
        "contextTerms": context_terms,
        "objectives": detect_objectives(question),
        "guardrails": detect_guardrails(question),
        "metrics": metrics,
        "segments": segment_snippets,
        "opportunityCohorts": detect_opportunity_cohorts(question),
        "existingMechanics": detect_existing_mechanics(question),
        "failedExperiments": detect_failed_experiments(question),
        "antiPatterns": detect_anti_patterns(question),
        "rawEvidenceSnippets": raw_evidence_snippets,
    })
}

fn detect_context_terms(input: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for term in collect_report_context_terms(input, 6) {
        push_unique(&mut terms, term);
    }
    let lower = lower_ascii(input);
    for concept in default_concept_registry().context_terms {
        if concept
            .labels
            .iter()
            .any(|label| input.contains(label) || lower.contains(&label.to_ascii_lowercase()))
        {
            push_unique(&mut terms, concept.key);
        }
    }
    terms
}

fn detect_metric_terms(input: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let lower = lower_ascii(input);
    for concept in default_concept_registry()
        .metrics
        .iter()
        .chain(default_concept_registry().metric_query_terms.iter())
    {
        if concept
            .labels
            .iter()
            .any(|label| input.contains(label) || lower.contains(&label.to_ascii_lowercase()))
        {
            push_unique(&mut terms, concept.key);
        }
    }
    for term in extract_dynamic_metric_terms(input) {
        push_unique(&mut terms, term);
    }
    terms
}

fn compact_query_context(terms: &[String], fallback: &str) -> String {
    let mut selected = Vec::<String>::new();
    let max_terms = if fallback.contains("metric") || fallback.contains("business outcome") {
        3
    } else {
        2
    };
    for term in terms {
        let trimmed = term.trim();
        if trimmed.is_empty() {
            continue;
        }
        let shortened = truncate_pm_query_chars(trimmed, 24);
        push_unique(&mut selected, shortened);
        if selected.len() >= max_terms {
            break;
        }
    }
    if selected.is_empty() {
        fallback.to_string()
    } else {
        selected.join(" ")
    }
}

fn push_search_query(out: &mut Vec<String>, value: impl Into<String>) {
    let value = compact_whitespace(&value.into());
    if let Some(value) = sanitize_pm_search_query(value.trim(), 116) {
        push_unique(out, value);
    }
}

fn contains_cjk_text(input: &str) -> bool {
    input
        .chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}

fn has_policy_or_compliance_intent(input: &str) -> bool {
    contains_any_raw(input, &["政策", "合规", "平台", "监管", "审核", "法务"])
        || contains_any_ascii(
            input,
            &[
                "policy",
                "compliance",
                "regulation",
                "regulatory",
                "legal",
                "platform rule",
            ],
        )
}

fn detect_query_cohort_terms(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in split_report_chunks(input) {
        let lower = lower_ascii(&chunk);
        if contains_any_raw(
            &chunk,
            &["分层", "人群", "用户类型", "场景", "客群", "细分"],
        ) || contains_any_ascii(
            &lower,
            &["segment", "cohort", "persona", "customer group", "scenario"],
        ) {
            for term in detect_metric_terms(&chunk) {
                push_context_candidate(&mut out, term);
                if out.len() >= 4 {
                    break;
                }
            }
            for token in [
                "分层",
                "人群",
                "用户类型",
                "新用户",
                "活跃老用户",
                "低价值",
                "高价值",
                "低活跃",
                "高活跃",
                "segment",
                "cohort",
                "persona",
                "scenario",
            ] {
                if chunk.contains(token) || lower.contains(&token.to_ascii_lowercase()) {
                    push_context_candidate(&mut out, token);
                }
                if out.len() >= 4 {
                    break;
                }
            }
        }
        if out.len() >= 4 {
            break;
        }
    }
    out
}

fn build_targeted_queries(input: &str) -> Vec<String> {
    let context_terms = detect_context_terms(input);
    let metrics = detect_metric_terms(input);
    let mechanics = detect_existing_mechanics(input);
    let objectives = detect_objectives(input);
    let cohorts = detect_query_cohort_terms(input);
    let context = compact_query_context(&context_terms, "product operations strategy");
    let metric_context = compact_query_context(&metrics, "business metric");
    let mechanic_context = compact_query_context(&mechanics, "operating mechanism");
    let objective_context = compact_query_context(&objectives, "business outcome");
    let cohort_context = compact_query_context(&cohorts, "segment scenario");
    let cjk = contains_cjk_text(input);
    let mut queries = Vec::new();
    if cjk {
        push_search_query(
            &mut queries,
            format!("{context} {metric_context} 提升策略 案例"),
        );
        push_search_query(
            &mut queries,
            format!("{context} {mechanic_context} 机制优化 复盘"),
        );
        push_search_query(
            &mut queries,
            format!("{context} {cohort_context} 分层策略 实验"),
        );
        push_search_query(
            &mut queries,
            format!("{context} {objective_context} 增长 变现 留存 保护指标"),
        );
        push_search_query(&mut queries, format!("{context} 实验设计 灰度 回滚 指标"));
        if has_policy_or_compliance_intent(input) {
            push_search_query(&mut queries, format!("{context} 平台政策 合规 风险 边界"));
        } else {
            push_search_query(&mut queries, format!("{context} 用户体验 风险 边界"));
        }
    } else {
        push_search_query(
            &mut queries,
            format!("{context} {metric_context} outcome improvement case study"),
        );
        if mechanics.is_empty() {
            push_search_query(
                &mut queries,
                format!("{context} operating mechanism playbook case study"),
            );
        } else {
            push_search_query(
                &mut queries,
                format!("{context} {mechanic_context} mechanism optimization case study"),
            );
        }
        push_search_query(
            &mut queries,
            format!("{context} {cohort_context} segmentation strategy experiment"),
        );
        push_search_query(
            &mut queries,
            format!("{context} {objective_context} guardrail metrics rollout"),
        );
        if has_policy_or_compliance_intent(input) {
            push_search_query(
                &mut queries,
                format!("{context} policy compliance risk constraints"),
            );
        } else {
            push_search_query(
                &mut queries,
                format!("{context} stakeholder experience risk guardrails"),
            );
        }
    }
    queries.truncate(6);
    queries
}

fn build_semantic_targeted_queries(extraction: &PmReportSemanticExtraction) -> Vec<String> {
    let mut context_terms = Vec::new();
    for term in extraction
        .domain_terms
        .iter()
        .chain(extraction.product_terms.iter())
    {
        push_unique(&mut context_terms, term);
    }
    let mut metric_terms = Vec::new();
    for term in extraction
        .metric_terms
        .iter()
        .chain(extraction.objective_terms.iter())
    {
        push_unique(&mut metric_terms, term);
    }
    let mut segment_terms = Vec::new();
    for term in &extraction.segment_terms {
        push_unique(&mut segment_terms, term);
    }
    let mut mechanism_terms = Vec::new();
    for term in &extraction.mechanism_terms {
        push_unique(&mut mechanism_terms, term);
    }
    let context = compact_query_context(&context_terms, "product operations strategy");
    let metrics = compact_query_context(&metric_terms, "business metric");
    let segments = compact_query_context(&segment_terms, "segment scenario");
    let mechanisms = compact_query_context(&mechanism_terms, "operating mechanism");
    let cjk = extraction
        .domain_terms
        .iter()
        .chain(extraction.product_terms.iter())
        .chain(extraction.search_queries.iter())
        .any(|term| contains_cjk_text(term));
    let mut queries = Vec::new();
    for query in extraction.search_queries.iter().take(6) {
        push_search_query(&mut queries, query);
    }
    if cjk {
        push_search_query(
            &mut queries,
            format!("{context} {mechanisms} 机制优化 案例"),
        );
        push_search_query(&mut queries, format!("{context} {segments} 分层策略 实验"));
        push_search_query(
            &mut queries,
            format!("{context} {metrics} 业务影响 保护指标"),
        );
        push_search_query(&mut queries, format!("{context} 风险 边界 灰度 回滚"));
    } else {
        push_search_query(
            &mut queries,
            format!("{context} {mechanisms} mechanism optimization case study"),
        );
        push_search_query(
            &mut queries,
            format!("{context} {segments} segmentation strategy experiment"),
        );
        push_search_query(
            &mut queries,
            format!("{context} {metrics} business impact guardrails"),
        );
        push_search_query(
            &mut queries,
            format!("{context} risks rollout kill criteria"),
        );
    }
    queries.truncate(8);
    queries
}

fn merge_string_json_array(
    target: &mut serde_json::Map<String, Value>,
    key: &str,
    additions: &[String],
    cap: usize,
) {
    if additions.is_empty() {
        return;
    }
    let mut merged = target
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for item in additions {
        push_unique(&mut merged, truncate_chars(item, 180));
    }
    merged.truncate(cap);
    target.insert(
        key.to_string(),
        Value::Array(merged.into_iter().map(Value::String).collect()),
    );
}

fn merge_first_party_evidence_with_semantic(
    evidence: &mut Value,
    extraction: &PmReportSemanticExtraction,
) {
    let Some(obj) = evidence.as_object_mut() else {
        return;
    };
    merge_string_json_array(obj, "contextTerms", &extraction.domain_terms, 12);
    merge_string_json_array(obj, "contextTerms", &extraction.product_terms, 12);
    merge_string_json_array(obj, "objectives", &extraction.objective_terms, 12);
    merge_string_json_array(obj, "guardrails", &extraction.constraint_terms, 12);
    merge_string_json_array(obj, "existingMechanics", &extraction.mechanism_terms, 12);
    merge_string_json_array(obj, "rawEvidenceSnippets", &extraction.key_sentences, 16);

    if !extraction.metric_terms.is_empty() {
        let mut metrics = obj
            .get("metrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for metric in extraction.metric_terms.iter().take(12) {
            if metrics.iter().any(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(metric))
            }) {
                continue;
            }
            metrics.push(serde_json::json!({
                "name": truncate_chars(metric, 80),
                "value": "mentioned",
                "sourceAlias": "llm_semantic_extract",
            }));
        }
        metrics.truncate(32);
        obj.insert("metrics".to_string(), Value::Array(metrics));
    }

    if !extraction.segment_terms.is_empty() {
        let mut cohorts = obj
            .get("opportunityCohorts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for segment in extraction.segment_terms.iter().take(8) {
            if cohorts.iter().any(|item| {
                item.get("cohort")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(segment))
            }) {
                continue;
            }
            cohorts.push(serde_json::json!({
                "cohort": truncate_chars(segment, 100),
                "why": "LLM semantic extraction identified this as a report-relevant segment/scenario.",
                "strategyHint": "围绕该人群/场景的一手指标、约束和历史结论设计差异化实验，而不是套用统一策略",
            }));
        }
        cohorts.truncate(12);
        obj.insert("opportunityCohorts".to_string(), Value::Array(cohorts));
    }

    if !extraction.prior_experiment_terms.is_empty() {
        let mut failed = obj
            .get("failedExperiments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in extraction.prior_experiment_terms.iter().take(8) {
            failed.push(serde_json::json!({
                "name": truncate_chars(item, 80),
                "result": truncate_chars(item, 180),
                "lesson": "LLM semantic extraction identified this as prior experiment/history that future strategy must respect.",
            }));
        }
        failed.truncate(12);
        obj.insert("failedExperiments".to_string(), Value::Array(failed));
    }

    obj.insert(
        "semanticExtractionSource".to_string(),
        Value::String(extraction.source.clone()),
    );
    obj.insert(
        "semanticExtraction".to_string(),
        serde_json::json!({
            "domainTerms": extraction.domain_terms,
            "productTerms": extraction.product_terms,
            "metricTerms": extraction.metric_terms,
            "objectiveTerms": extraction.objective_terms,
            "constraintTerms": extraction.constraint_terms,
            "segmentTerms": extraction.segment_terms,
            "mechanismTerms": extraction.mechanism_terms,
            "priorExperimentTerms": extraction.prior_experiment_terms,
            "keySentences": extraction.key_sentences,
            "searchQueries": extraction.search_queries,
            "source": extraction.source,
        }),
    );
}

pub fn detect_pm_report_strategy_signal(question: &str) -> PmReportStrategySignal {
    let trimmed = question.trim();
    if trimmed.is_empty() {
        return PmReportStrategySignal {
            matched: false,
            score: 0,
            reasons: Vec::new(),
            primary_terms: Vec::new(),
            targeted_queries: Vec::new(),
        };
    }

    let registry = default_concept_registry();
    let lower_trimmed = lower_ascii(trimmed);
    let metric_count = {
        let mut metric_terms = Vec::<String>::new();
        for concept in registry.metrics {
            if concept.labels.iter().any(|label| {
                trimmed.contains(label) || lower_trimmed.contains(&label.to_ascii_lowercase())
            }) {
                push_unique(&mut metric_terms, concept.key);
            }
        }
        for term in extract_dynamic_metric_terms(trimmed) {
            push_unique(&mut metric_terms, term);
        }
        metric_terms.len()
    };
    let segment_count = registry
        .segment_terms
        .iter()
        .filter(|term| {
            trimmed.contains(**term) || lower_trimmed.contains(&term.to_ascii_lowercase())
        })
        .count();
    let strategy_count = registry
        .strategy_terms
        .iter()
        .filter(|term| {
            trimmed.contains(**term) || lower_trimmed.contains(&term.to_ascii_lowercase())
        })
        .count();
    let history_count = registry
        .history_terms
        .iter()
        .filter(|term| {
            trimmed.contains(**term) || lower_trimmed.contains(&term.to_ascii_lowercase())
        })
        .count();
    let numeric_count = count_numeric_markers(trimmed);
    let section_count = registry
        .section_terms
        .iter()
        .filter(|term| trimmed.contains(**term))
        .count();
    let context_count = detect_context_terms(trimmed).len();

    let mut score = 0usize;
    let mut reasons = Vec::new();
    if metric_count >= 4 {
        score += 3;
        reasons.push("rich_metric_context".to_string());
    }
    if segment_count >= 2 {
        score += 3;
        reasons.push("explicit_user_segmentation".to_string());
    }
    if strategy_count >= 2 {
        score += 2;
        reasons.push("strategy_request".to_string());
    }
    if history_count >= 2 {
        score += 2;
        reasons.push("prior_experiment_context".to_string());
    }
    if numeric_count >= 12 {
        score += 2;
        reasons.push("dense_first_party_numbers".to_string());
    }
    if section_count >= 3 {
        score += 1;
        reasons.push("structured_report".to_string());
    }
    if context_count >= 2 {
        score += 1;
        reasons.push("specific_business_context".to_string());
    }

    let mut primary_terms = Vec::new();
    for term in detect_context_terms(trimmed)
        .into_iter()
        .chain(detect_metric_terms(trimmed))
    {
        push_unique(&mut primary_terms, term);
    }
    let has_sufficient_metric_context = metric_count >= 3
        || (metric_count >= 1 && numeric_count >= 12)
        || (numeric_count >= 18 && section_count >= 2);
    let matched = score >= 7 && has_sufficient_metric_context && strategy_count >= 1;
    PmReportStrategySignal {
        matched,
        score,
        reasons,
        primary_terms,
        targeted_queries: if matched {
            build_targeted_queries(trimmed)
        } else {
            Vec::new()
        },
    }
}

pub fn pm_is_report_strategy_mode(plan: &Value) -> bool {
    plan.get("mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode.eq_ignore_ascii_case("business_report_strategy"))
}

pub fn attach_pm_report_strategy_hint(plan: &mut Value, question: &str) -> PmReportStrategySignal {
    let signal = detect_pm_report_strategy_signal(question);
    let Some(obj) = plan.as_object_mut() else {
        return signal;
    };
    obj.insert(
        "reportStrategyHint".to_string(),
        serde_json::json!({
            "advisory": true,
            "matched": signal.matched,
            "score": signal.score,
            "reasons": signal.reasons,
            "primaryTerms": signal.primary_terms,
            "targetedQueries": signal.targeted_queries,
            "firstPartyEvidenceJson": extract_pm_first_party_evidence(question),
            "instruction": "Advisory only. The LLM turn router must decide whether this is pm_report_strategy, pm_strategy, general_research, live_lookup, simple_answer, or simple_chat.",
        }),
    );
    signal
}

pub fn apply_pm_report_semantic_extraction(
    plan: &mut Value,
    extraction: &PmReportSemanticExtraction,
) -> bool {
    if !pm_is_report_strategy_mode(plan) || !extraction.has_useful_signal() {
        return false;
    }
    let Some(obj) = plan.as_object_mut() else {
        return false;
    };

    let mut primary_terms = obj
        .get("reportStrategy")
        .and_then(|value| value.get("primaryTerms"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for term in extraction
        .domain_terms
        .iter()
        .chain(extraction.product_terms.iter())
        .chain(extraction.metric_terms.iter())
        .chain(extraction.objective_terms.iter())
        .chain(extraction.segment_terms.iter())
    {
        push_unique(&mut primary_terms, truncate_chars(term, 80));
    }
    primary_terms.truncate(24);

    let existing_query_variants = obj
        .get("queryVariants")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let existing_query_variants = sanitize_pm_search_queries(existing_query_variants, None, 16);
    let mut query_variants = Vec::<String>::new();
    for query in build_semantic_targeted_queries(extraction) {
        push_search_query(&mut query_variants, query);
    }
    let semantic_query_count = query_variants.len();
    for query in existing_query_variants {
        if semantic_query_count >= 4 && query_variants.len() >= 8 {
            break;
        }
        push_search_query(&mut query_variants, query);
    }
    query_variants = sanitize_pm_search_queries(query_variants, None, 16);
    obj.insert(
        "queryVariants".to_string(),
        Value::Array(query_variants.iter().cloned().map(Value::String).collect()),
    );

    if let Some(report_strategy) = obj.get_mut("reportStrategy").and_then(Value::as_object_mut) {
        report_strategy.insert(
            "primaryTerms".to_string(),
            Value::Array(primary_terms.into_iter().map(Value::String).collect()),
        );
        report_strategy.insert(
            "targetedQueries".to_string(),
            Value::Array(query_variants.into_iter().map(Value::String).collect()),
        );
        if let Some(first_party) = report_strategy.get_mut("firstPartyEvidenceJson") {
            merge_first_party_evidence_with_semantic(first_party, extraction);
        }
        report_strategy.insert(
            "semanticExtractionApplied".to_string(),
            serde_json::json!(true),
        );
    }

    if let Some(task_graph) = obj.get_mut("taskGraph").and_then(Value::as_object_mut) {
        task_graph.insert(
            "semanticExtractionApplied".to_string(),
            serde_json::json!(true),
        );
        task_graph.insert(
            "semanticFocus".to_string(),
            serde_json::json!({
                "domainTerms": extraction.domain_terms,
                "productTerms": extraction.product_terms,
                "metricTerms": extraction.metric_terms,
                "objectiveTerms": extraction.objective_terms,
                "constraintTerms": extraction.constraint_terms,
                "segmentTerms": extraction.segment_terms,
                "mechanismTerms": extraction.mechanism_terms,
            }),
        );
    }

    true
}

pub fn apply_pm_report_strategy_plan(plan: &mut Value, question: &str) -> PmReportStrategySignal {
    let mut signal = detect_pm_report_strategy_signal(question);
    let llm_selected_report_strategy = plan
        .get("turnRoute")
        .and_then(Value::as_object)
        .and_then(|obj| {
            obj.get("turnClass")
                .or_else(|| obj.get("turn_class"))
                .and_then(Value::as_str)
        })
        .is_some_and(|turn_class| {
            matches!(
                turn_class.trim().to_ascii_lowercase().as_str(),
                "pm_report_strategy" | "business_report_strategy" | "report_strategy"
            )
        });
    if !llm_selected_report_strategy {
        return signal;
    }
    if signal.targeted_queries.is_empty() {
        signal.targeted_queries = build_targeted_queries(question);
    }
    let Some(obj) = plan.as_object_mut() else {
        return signal;
    };
    obj.insert(
        "mode".to_string(),
        Value::String("business_report_strategy".to_string()),
    );
    obj.insert(
        "queryVariants".to_string(),
        Value::Array(
            signal
                .targeted_queries
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    obj.insert("targetEvidenceCount".to_string(), serde_json::json!(6));
    obj.insert("mustCiteUrls".to_string(), serde_json::json!(false));
    let first_party_evidence = obj
        .get("reportStrategyHint")
        .and_then(|value| value.get("firstPartyEvidenceJson"))
        .cloned()
        .unwrap_or_else(|| extract_pm_first_party_evidence(question));
    obj.insert(
        "reportStrategy".to_string(),
        serde_json::json!({
            "score": signal.score,
            "reasons": signal.reasons,
            "primaryTerms": signal.primary_terms,
            "targetedQueries": signal.targeted_queries,
            "firstPartyEvidenceJson": first_party_evidence,
            "firstPartyEvidencePriority": "primary",
            "externalEvidenceRole": "targeted_augmentation",
            "selectedBy": "llm_turn_router",
        }),
    );
    if obj.get("taskGraph").is_none() {
        obj.insert(
            "taskGraph".to_string(),
            serde_json::json!({
                "intent": "decision_support",
                "complexityScore": 82,
                "decompositionMode": "light",
                "subtasks": [
                    {
                        "id": "mechanic_inspiration",
                        "title": "行业案例与机制启发",
                        "goal": "围绕用户报告中识别出的行业、对象、已有机制和目标，检索可借鉴案例与机制启发；不得覆盖用户报告事实",
                        "queries": signal.targeted_queries.iter().take(2).cloned().collect::<Vec<_>>(),
                        "deliverable": "输出可借鉴机制、适用人群和风险边界",
                        "requiredEvidenceType": "external",
                        "priority": "high"
                    },
                    {
                        "id": "guardrail_benchmark",
                        "title": "实验保护指标与风险基准",
                        "goal": "补充与用户报告目标和约束匹配的实验保护指标、观察窗口、回滚阈值和风险基准",
                        "queries": signal.targeted_queries.iter().skip(2).take(2).cloned().collect::<Vec<_>>(),
                        "deliverable": "输出灰度实验保护指标、kill criteria 和验证节奏",
                        "requiredEvidenceType": "external",
                        "priority": "high"
                    },
                    {
                        "id": "segment_strategy_benchmark",
                        "title": "人群/场景分层策略补强",
                        "goal": "检索与报告中人群、场景、价值差异、资源强度和成本收益相关的分层策略经验",
                        "queries": signal.targeted_queries.iter().skip(3).take(2).cloned().collect::<Vec<_>>(),
                        "deliverable": "输出可映射到报告人群/场景/指标的策略启发",
                        "requiredEvidenceType": "external",
                        "priority": "high"
                    },
                    {
                        "id": "risk_policy_constraints",
                        "title": "风险、合规与体验边界",
                        "goal": "补充与用户问题相关的政策/合规/体验/风险约束；只有在问题相关时才把政策资料作为边界证据",
                        "queries": signal.targeted_queries.iter().skip(4).take(2).cloned().collect::<Vec<_>>(),
                        "deliverable": "输出不能踩的风险边界和上线前检查项",
                        "requiredEvidenceType": "external",
                        "priority": "medium"
                    }
                ]
            }),
        );
    }
    let enabled_route_ids: BTreeSet<String> = obj
        .get("sourceRoutes")
        .and_then(Value::as_array)
        .map(|routes| {
            routes
                .iter()
                .filter_map(|route| route.get("routeId").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    let route = serde_json::json!({
        "routeId": "web.search.general",
        "channel": "web_search",
        "enabled": true,
        "priority": "high",
        "executionChannel": "search",
        "quota": 3,
        "reason": "Targeted external augmentation for first-party business report strategy",
        "toolHints": ["mcp_search"]
    });
    if !enabled_route_ids.contains("web.search.general") {
        let mut routes = obj
            .get("sourceRoutes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        routes.push(route);
        obj.insert("sourceRoutes".to_string(), Value::Array(routes));
    } else if let Some(routes) = obj.get_mut("sourceRoutes").and_then(Value::as_array_mut) {
        for route in routes {
            if let Some(route_obj) = route.as_object_mut() {
                let route_id = route_obj
                    .get("routeId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if route_id != "web.search.general" {
                    continue;
                }
                route_obj.insert("enabled".to_string(), serde_json::json!(true));
                route_obj.insert("quota".to_string(), serde_json::json!(3));
                route_obj.insert("priority".to_string(), serde_json::json!("high"));
                route_obj.insert(
                    "reason".to_string(),
                    serde_json::json!(
                        "Targeted external augmentation for first-party business report strategy"
                    ),
                );
            }
        }
    }
    let selected_route_ids: Vec<Value> = obj
        .get("sourceRoutes")
        .and_then(Value::as_array)
        .map(|routes| {
            routes
                .iter()
                .filter(|route| {
                    route
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .filter_map(|route| route.get("routeId").and_then(Value::as_str))
                .map(|route_id| Value::String(route_id.to_string()))
                .collect()
        })
        .unwrap_or_default();
    obj.insert(
        "selectedRouteIds".to_string(),
        Value::Array(selected_route_ids),
    );
    obj.insert(
        "parallelism".to_string(),
        serde_json::json!({
            "probeVariantFanoutMax": 2,
            "probeRouteFanoutMax": 1,
            "probeCandidateMax": 2,
            "maxConcurrentSubtasks": 2,
            "maxProbePerSubtask": 1,
            "minSourcesPerSubtask": 1,
            "runtimePerSessionTurn": 1
        }),
    );
    signal
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROI_REPORT: &str = "口径我先写清楚：基于 LUCKYMAHJONGMATCH_A / mahjong_ID，20260608~20260621 日均数据，成本只算 UA+UG，不含资产成本。我们是印尼网赚单机休闲矩阵产品。
一、业务背景
当前核心目标不是单纯少发金币，而是：ROI提升，AIPU不能下降，游戏时长不能下降，次留不能下降，ROAS1/3/7希望提升。
之前试过 EWMA / hybrid 等 eCPM 算法，EWMA ROI 小幅上涨，但 AIPU、时长、次留、ROAS下降。
二、当前大盘日均表现 DAU25,352 广告收入$1,369 UA成本$756 UG成本$352 ROI1.235 AIPU17.11 eCPM3.16 ARPU$0.054
三、按 eCPM 用户价值分层 eCPM <1 ROI0.384，eCPM 5+ ROI2.264
四、按 AIPU 活跃度分层 低AIPU1~4 ROI0.432 高AIPU>=16 ROI2.375
五、eCPM × AIPU 关键人群 eCPM 5+ + AIPU 1~4 高价值低活跃，最适合拉广告次数
六、当前已有玩法 连击玩法、悬浮宝箱、广告位ID、float box奖励、double奖励。
我的诉求是基于我发你的这份报告做一些玩法和策略，不要烂大街的玩法和策略，要立竿见影的。";

    #[test]
    fn detects_first_party_business_report_strategy_request() {
        let signal = detect_pm_report_strategy_signal(ROI_REPORT);
        assert!(signal.matched, "{signal:?}");
        assert!(signal.score >= 7);
        assert!(signal.targeted_queries.iter().any(|query| {
            query.contains("印尼网赚单机休闲矩阵产品") || query.contains("LUCKYMAHJONGMATCH")
        }));
        assert!(signal.targeted_queries.iter().all(|query| {
            query.chars().count() < 120 && !query.contains("LUCKYMAHJONGMATCH_A")
        }));
        assert!(
            signal
                .targeted_queries
                .iter()
                .any(|query| lower_ascii(query).contains("roi")
                    || lower_ascii(query).contains("aipu"))
        );
    }

    #[test]
    fn applies_report_strategy_plan_with_targeted_queries() {
        let mut plan = serde_json::json!({
            "mode": "auto",
            "turnRoute": {"turnClass":"pm_report_strategy"},
            "queryVariants": [ROI_REPORT],
            "sourceRoutes": [
                {"routeId":"web.search.general","enabled":true},
                {"routeId":"news.sites.search","enabled":true}
            ],
            "parallelism": {}
        });
        let signal = apply_pm_report_strategy_plan(&mut plan, ROI_REPORT);
        assert!(signal.matched);
        assert_eq!(
            plan.get("mode").and_then(Value::as_str),
            Some("business_report_strategy")
        );
        assert!(plan
            .get("queryVariants")
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() >= 3
                && items.iter().all(|item| {
                    let query = item.as_str().unwrap_or("");
                    query.chars().count() < 120
                        && !query.contains("...")
                        && !query.contains('…')
                        && !query.contains("+ more")
                        && !query.contains("当前策略制定需要特别注意")
                        && !query.contains("UVUV")
                })));
        assert_eq!(
            plan.get("taskGraph")
                .and_then(|v| v.get("decompositionMode"))
                .and_then(Value::as_str),
            Some("light")
        );
        assert_eq!(
            plan.get("parallelism")
                .and_then(|value| value.get("probeCandidateMax"))
                .and_then(Value::as_u64),
            Some(2)
        );
        assert!(plan
            .get("selectedRouteIds")
            .and_then(Value::as_array)
            .is_some_and(|items| items
                .iter()
                .any(|item| item.as_str() == Some("news.sites.search"))));
        let first_party = plan
            .get("reportStrategy")
            .and_then(|value| value.get("firstPartyEvidenceJson"))
            .expect("first-party evidence should be embedded in report strategy plan");
        assert_eq!(
            first_party.get("evidencePriority").and_then(Value::as_str),
            Some("primary")
        );
        assert!(first_party
            .get("guardrails")
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() >= 3));
        assert!(first_party
            .get("failedExperiments")
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() >= 2));
        assert!(first_party
            .get("opportunityCohorts")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()));
    }

    #[test]
    fn report_strategy_signal_is_advisory_until_llm_selects_route() {
        let mut plan = serde_json::json!({
            "mode": "auto",
            "queryVariants": [ROI_REPORT],
            "sourceRoutes": [
                {"routeId":"web.search.general","enabled":true}
            ],
            "parallelism": {}
        });
        let signal = apply_pm_report_strategy_plan(&mut plan, ROI_REPORT);
        assert!(signal.matched);
        assert_eq!(plan.get("mode").and_then(Value::as_str), Some("auto"));
        assert!(plan.get("reportStrategy").is_none());
    }

    #[test]
    fn targeted_queries_are_derived_from_report_not_reward_game_template() {
        let report = "我们是 B2B SaaS 自助 onboarding 产品，过去 30 天 trial 用户 18,420，activation 31%，MRR $120k，churn 7.2%，CAC $86。目标是提升 activation、降低 churn、提升 MRR，但 support tickets 不能上升，销售人工介入不能上升。按用户场景分层：solo trial activation 18%，team trial activation 44%，enterprise trial activation 27%。之前试过 mandatory demo wall，activation 下降，self-serve 转化变差。当前已有 email onboarding、in-app checklist、template gallery。我的诉求是基于这份报告给产品运营策略和实验方案。";
        let signal = detect_pm_report_strategy_signal(report);
        assert!(signal.matched, "{signal:?}");
        assert!(signal
            .targeted_queries
            .iter()
            .any(|query| query.contains("B2B SaaS") || query.contains("onboarding")));
        assert!(signal
            .targeted_queries
            .iter()
            .any(|query| lower_ascii(query).contains("activation")
                || lower_ascii(query).contains("churn")));
        assert!(signal.targeted_queries.iter().all(|query| {
            let lower = lower_ascii(query);
            !lower.contains("rewarded")
                && !lower.contains("reward economy")
                && !lower.contains("incentive economy")
                && !lower.contains("ad frequency")
                && !query.contains("奖励广告")
                && !query.contains("激励经济")
                && !query.contains("频控")
        }));
    }

    #[test]
    fn semantic_extraction_does_not_reintroduce_truncated_report_queries() {
        let mut plan = serde_json::json!({
            "mode": "business_report_strategy",
            "queryVariants": [
                "reward app ad monetization rewarded ads retention strategy",
                "是印尼网赚单机休闲 App 矩阵 约 5 到 6 个产品 当前策略制定需要特别注意：不能把所有用户一刀切，... 三、AIPU 分层结论 分层策略 实验",
                "casual game rewarded ads frequency AIPU retention ROAS"
            ],
            "reportStrategy": {
                "primaryTerms": ["是印尼网赚单机休闲 App 矩阵 约 5 到 6 个产品"],
                "firstPartyEvidenceJson": {}
            }
        });
        let extraction = PmReportSemanticExtraction {
            domain_terms: vec!["印尼网赚单机休闲 App 矩阵".to_string()],
            product_terms: vec!["单机休闲 App".to_string()],
            metric_terms: vec!["ROI".to_string(), "ROAS".to_string(), "AIPU".to_string()],
            objective_terms: vec!["提升有效广告收入".to_string()],
            constraint_terms: vec![],
            segment_terms: vec!["新用户".to_string(), "AIPU 1 到 4".to_string()],
            mechanism_terms: vec!["首日广告激活".to_string()],
            prior_experiment_terms: vec![],
            key_sentences: vec![],
            search_queries: vec![
                "new user day 1 ad activation rewarded app cohort strategy".to_string(),
                "rewarded ads experiment guardrails retention uninstall".to_string(),
                "ad monetization unit economics ROI ROAS user acquisition rewards app".to_string(),
                "Indonesia rewarded app ad monetization eCPM user acquisition".to_string(),
            ],
            source: "llm_semantic_extract".to_string(),
        };

        assert!(apply_pm_report_semantic_extraction(&mut plan, &extraction));
        let variants = plan.get("queryVariants").and_then(Value::as_array).unwrap();
        assert!(variants.iter().all(|item| {
            let query = item.as_str().unwrap_or_default();
            !query.contains("...") && !query.contains("当前策略制定需要特别注意")
        }));
        assert!(variants.iter().any(|item| {
            item.as_str() == Some("new user day 1 ad activation rewarded app cohort strategy")
        }));
    }

    #[test]
    fn first_party_evidence_schema_is_industry_agnostic() {
        let report = "我们是 B2B SaaS 自助 onboarding 产品，过去 30 天 trial 用户 18,420，activation 31%，MRR $120k，churn 7.2%，CAC $86。目标是提升 activation、降低 churn、提升 MRR，但 support tickets 不能上升。按用户场景分层：solo trial activation 18%，team trial activation 44%，enterprise trial activation 27%。当前已有 email onboarding、in-app checklist。";
        let evidence = extract_pm_first_party_evidence(report);
        assert!(evidence.get("segments").and_then(Value::as_array).is_some());
        assert!(evidence
            .get("segments")
            .and_then(Value::as_object)
            .is_none());
        let metrics = evidence
            .get("metrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let names = metrics
            .iter()
            .filter_map(|item| item.get("name").and_then(Value::as_str))
            .map(|item| item.to_ascii_lowercase())
            .collect::<Vec<_>>();
        assert!(
            names.iter().any(|name| name.contains("activation")),
            "{names:?}"
        );
        assert!(names.iter().any(|name| name.contains("mrr")), "{names:?}");
        assert!(names.iter().any(|name| name.contains("churn")), "{names:?}");
        assert!(names.iter().any(|name| name.contains("cac")), "{names:?}");
    }

    #[test]
    fn dynamic_metric_extraction_handles_compact_kpis_without_vertical_defaults() {
        let report = "当前大盘 KPI：AIPU17.11 eCPM3.16 ROAS1/3/7 activation31% MRR$120k churn7.2%。目标是提升关键指标，但留存不能下降。";
        let evidence = extract_pm_first_party_evidence(report);
        let metrics = evidence
            .get("metrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let pairs = metrics
            .iter()
            .filter_map(|item| {
                Some((
                    item.get("name")?.as_str()?.to_string(),
                    item.get("value")?.as_str()?.to_string(),
                ))
            })
            .collect::<Vec<_>>();
        assert!(
            pairs
                .iter()
                .any(|(name, value)| name == "AIPU" && value == "17.11"),
            "{pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(name, value)| name == "eCPM" && value == "3.16"),
            "{pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(name, value)| name == "activation" && value == "31%"),
            "{pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(name, value)| name == "MRR" && value == "$120k"),
            "{pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(name, value)| name == "churn" && value == "7.2%"),
            "{pairs:?}"
        );
        assert!(
            !pairs
                .iter()
                .any(|(name, value)| name == "ROI" && value == "1/3/7"),
            "{pairs:?}"
        );
    }

    #[test]
    fn ordinary_external_research_question_is_not_report_strategy() {
        let signal =
            detect_pm_report_strategy_signal("现在印尼网赚游戏市场规模和最新用户画像趋势如何");
        assert!(!signal.matched, "{signal:?}");
    }

    #[test]
    fn extracts_first_party_evidence_without_metric_goal_bleed() {
        let evidence = extract_pm_first_party_evidence(ROI_REPORT);
        assert_eq!(
            evidence.get("schemaVersion").and_then(Value::as_str),
            Some("pm_first_party_report.v1")
        );
        let metrics = evidence
            .get("metrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(metrics.iter().any(|item| {
            item.get("name").and_then(Value::as_str) == Some("ROI")
                && item.get("value").and_then(Value::as_str) == Some("1.235")
        }));
        assert!(!metrics.iter().any(|item| {
            item.get("name").and_then(Value::as_str) == Some("ROI")
                && item.get("value").and_then(Value::as_str) == Some("1/3/7")
        }));
        assert!(evidence
            .get("antiPatterns")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()));
    }

    #[test]
    fn extracts_compressed_table_opportunity_cohorts() {
        let report = "eCPM <1 + AIPU >=16 低eCPM但高活跃，不能一刀切；eCPM 5+ + AIPU 1~4 高价值低活跃，最适合拉广告次数。用户类型AIPU分层：new低AIPU ROI0.172，new中AIPU ROI0.510。";
        let evidence = extract_pm_first_party_evidence(report);
        let cohorts = evidence
            .get("opportunityCohorts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let names = cohorts
            .iter()
            .filter_map(|item| item.get("cohort").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(
            names
                .iter()
                .any(|name| name.contains("eCPM") && name.contains("AIPU")),
            "{names:?}"
        );
        assert!(
            names.iter().any(|name| name.contains("new低AIPU")),
            "{names:?}"
        );
    }
}
