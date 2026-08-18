//! Coreference resolution — resolves pronouns and references in follow-up questions.
//!
//! Multi-turn NL2SQL conversations often contain follow-up questions that reference
//! previous context: "那上月呢", "排除退货的", "同样的，但只看VIP".
//! This module resolves those references using lightweight pattern matching rather
//! than expensive LLM calls, injecting resolved context into the SQL generation prompt.
//!
//! Resolution strategy:
//! - **Time references**: "上月" / "上月呢" / "上个月" → inherit time range from previous query
//! - **Exclusion modifiers**: "排除X" / "除了X" / "不算X" → NOT filter conditions
//! - **Scope modifiers**: "只看VIP" / "只看大客户" → additional filter conditions
//! - **Subject pronouns**: "这个" / "那" / "它" → substitute with previous subject

use serde::{Deserialize, Serialize};

/// Result of resolving a follow-up question's references against previous context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolvedQuestion {
    /// The resolved question text (with pronouns/ambiguous references expanded).
    pub resolved_text: String,
    /// Time context inherited from the previous query (if any).
    pub time_context: Option<TimeContext>,
    /// Additional filter conditions from scope modifiers ("只看VIP").
    pub additional_filters: Vec<FilterCondition>,
    /// Exclusion conditions ("排除退货的", "不算X").
    pub exclusion_filters: Vec<FilterCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeContext {
    /// Raw text of the time reference in the follow-up question.
    pub raw_text: String,
    /// Inherited time range from previous query (human-readable description).
    pub inherited_range: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterCondition {
    pub column: String,
    pub operator: String,
    pub value: String,
}

/// Previous query context for coreference resolution.
pub struct PrevContext<'a> {
    pub question: &'a str,
    pub sql: &'a str,
    pub time_range: Option<&'a str>,
    pub tables: &'a [&'a str],
    pub filters: &'a [FilterCondition],
}

impl ResolvedQuestion {
    /// Generate a human-readable prompt section for SQL generation.
    pub fn to_prompt_context(&self) -> String {
        let mut ctx = String::new();

        if let Some(ref t) = self.time_context {
            ctx.push_str(&format!(
                "\nTime context from previous query: {} (inferred from \"{}\")\n",
                t.inherited_range, t.raw_text
            ));
        }

        if !self.exclusion_filters.is_empty() {
            ctx.push_str("\nExclusion filters (do NOT include these rows): ");
            for f in &self.exclusion_filters {
                ctx.push_str(&format!("{} {} {}; ", f.column, f.operator, f.value));
            }
            ctx.push('\n');
        }

        if !self.additional_filters.is_empty() {
            ctx.push_str("\nAdditional filters from follow-up: ");
            for f in &self.additional_filters {
                ctx.push_str(&format!("{} {} {}; ", f.column, f.operator, f.value));
            }
            ctx.push('\n');
        }

        ctx
    }

    /// Whether this resolution produced any useful context.
    pub fn is_empty(&self) -> bool {
        self.time_context.is_none()
            && self.additional_filters.is_empty()
            && self.exclusion_filters.is_empty()
    }
}

/// Resolve references in a follow-up question using the previous query's context.
pub fn resolve(question: &str, prev: Option<&PrevContext>) -> ResolvedQuestion {
    let q = question.trim();
    let mut resolved = ResolvedQuestion {
        resolved_text: q.to_string(),
        time_context: None,
        additional_filters: Vec::new(),
        exclusion_filters: Vec::new(),
    };

    if q.is_empty() {
        return resolved;
    }

    // Pattern 1: Time references — inherit time range from previous query
    if is_time_reference(q) {
        if let Some(prev_ctx) = prev {
            if let Some(range) = prev_ctx.time_range {
                resolved.time_context = Some(TimeContext {
                    raw_text: q.to_string(),
                    inherited_range: range.to_string(),
                });
            }
        }
    }

    // Pattern 2: Exclusion modifiers — "排除X", "除了X", "不算X", "不包括X"
    resolved.exclusion_filters = extract_exclusions(q);

    // Pattern 3: Scope modifiers — "只看X", "只看VIP", "只看大客户"
    resolved.additional_filters = extract_scope_filters(q);

    // Pattern 4: Simple subject pronouns — "这个", "那", "它"
    if is_pronoun_only(q) {
        if let Some(prev_ctx) = prev {
            // Replace "这个"/"那" with the previous question's subject
            resolved.resolved_text = substitute_pronoun(q, prev_ctx);
        }
    }

    resolved
}

// ─── Pattern detectors ────────────────────────────────────────────────────────

fn is_time_reference(q: &str) -> bool {
    let t = q.trim();
    TIME_PATTERNS.iter().any(|(pat, _)| pat.is_match(t))
}

fn is_pronoun_only(q: &str) -> bool {
    let t = q.trim();
    t == "那"
        || t == "这个"
        || t == "那个"
        || t == "它"
        || t == "这个呢"
        || t == "那个呢"
        || t == "呢"
}

// ─── Pattern matchers ─────────────────────────────────────────────────────────

fn extract_exclusions(q: &str) -> Vec<FilterCondition> {
    let mut conditions = Vec::new();
    for (pat, col_hint) in EXCLUSION_PATTERNS.iter() {
        if let Some(caps) = pat.captures(q) {
            if let Some(target) = caps.get(1) {
                let target_str = target.as_str().trim_end_matches('的').trim();
                if !target_str.is_empty() {
                    conditions.push(FilterCondition {
                        column: col_hint.to_string(),
                        operator: "!=".to_string(),
                        value: target_str.to_string(),
                    });
                }
            }
        }
    }
    conditions
}

fn extract_scope_filters(q: &str) -> Vec<FilterCondition> {
    let mut conditions = Vec::new();
    for (pat, col_hint) in SCOPE_PATTERNS.iter() {
        if let Some(caps) = pat.captures(q) {
            if let Some(target) = caps.get(1) {
                let target_str = target.as_str().trim();
                if !target_str.is_empty() {
                    conditions.push(FilterCondition {
                        column: col_hint.to_string(),
                        operator: "=".to_string(),
                        value: target_str.to_string(),
                    });
                }
            }
        }
    }
    conditions
}

fn substitute_pronoun(q: &str, prev: &PrevContext) -> String {
    // If the previous query referenced specific tables, reconstruct the subject
    if prev.tables.is_empty() {
        return q.to_string();
    }
    // For now, append the previous question as context
    // This is a fallback; the LLM will handle the actual reconstruction
    format!("{q} ({})", prev.question)
}

// ─── Regex patterns ─────────────────────────────────────────────────────────

// Each pattern is fully compiled once at first use via lazy_static!.
// No unwrap() in hot paths. If a pattern is invalid, the service panics
// immediately on first access — clearly visible in logs rather than silently
// failing at runtime.

// Expose as lazy static iterators for use in is_time_reference etc.
lazy_static::lazy_static! {
    /// All time patterns as (compiled_regex, hint) pairs.
    pub(crate) static ref TIME_PATTERNS: Vec<(regex::Regex, &'static str)> = vec![
        (regex::Regex::new(r"^上个月呢?$").unwrap(), "last month"),
        (regex::Regex::new(r"^上月呢?$").unwrap(), "previous month"),
        (regex::Regex::new(r"^上上个月$").unwrap(), "2 months ago"),
        (regex::Regex::new(r"^那(个?月|周|季度|年)呢?$").unwrap(), "that period"),
        // "那上月呢" / "那上个月呢" / "那下季度呢" — explicit direction + period inherits prev time range.
        (regex::Regex::new(r"^那(上|下)(个?月|周|季度|年)呢?$").unwrap(), "that adjacent period"),
        (regex::Regex::new(r"^同样的(，|$)").unwrap(), "same"),
        (regex::Regex::new(r"^和上(次|一?次)(一样|同样)?$").unwrap(), "same as last"),
        (regex::Regex::new(r"^按上(次|一?次)的条件$").unwrap(), "same conditions"),
        (regex::Regex::new(r"^上期$").unwrap(), "previous period"),
        (regex::Regex::new(r"^同比上(年|个月|季度)?$").unwrap(), "YoY"),
        (regex::Regex::new(r"^环比上(年|个月|季度)?$").unwrap(), "MoM"),
    ];

    /// All exclusion patterns as (compiled_regex, hint) pairs.
    pub(crate) static ref EXCLUSION_PATTERNS: Vec<(regex::Regex, &'static str)> = vec![
        (regex::Regex::new(r#"^排除(.*?的?)['"]?$"#).unwrap(), ""),
        (regex::Regex::new(r#"^除了(.*?的?)['"]?$"#).unwrap(), ""),
        (regex::Regex::new(r#"^不算(.*?的?)$"#).unwrap(), ""),
        (regex::Regex::new(r#"^不包括(.*?的?)$"#).unwrap(), ""),
        (regex::Regex::new(r#"^去掉(.*?的?)['"]?$"#).unwrap(), ""),
        (regex::Regex::new(r#"^剔除(.*?的?)$"#).unwrap(), ""),
    ];

    /// All scope patterns as (compiled_regex, hint) pairs.
    pub(crate) static ref SCOPE_PATTERNS: Vec<(regex::Regex, &'static str)> = vec![
        (regex::Regex::new(r#"^只看(.*)['"]?$"#).unwrap(), ""),
        (regex::Regex::new(r#"^仅看(.*)['"]?$"#).unwrap(), ""),
        (regex::Regex::new(r#"^只要(.*)['"]?$"#).unwrap(), ""),
        (regex::Regex::new(r#"^只要是(.*)$"#).unwrap(), ""),
        (regex::Regex::new(r#"^只看(.*?)的"#).unwrap(), ""),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_reference_detection() {
        assert!(is_time_reference("上月呢"));
        assert!(is_time_reference("上个月"));
        assert!(is_time_reference("上个月呢"));
        assert!(is_time_reference("那个月呢"));
        assert!(!is_time_reference("上个月的订单"));
    }

    #[test]
    fn test_pronoun_only_detection() {
        assert!(is_pronoun_only("那"));
        assert!(is_pronoun_only("这个"));
        assert!(is_pronoun_only("那个"));
        assert!(is_pronoun_only("它"));
        assert!(!is_pronoun_only("那个用户"));
        assert!(!is_pronoun_only("上个月的订单"));
    }

    #[test]
    fn test_exclusion_extraction() {
        let r = resolve("排除退货的", None);
        assert_eq!(r.exclusion_filters.len(), 1);

        let r = resolve("除了VIP用户的", None);
        assert_eq!(r.exclusion_filters.len(), 1);
    }

    #[test]
    fn test_scope_filter_extraction() {
        let r = resolve("只看VIP", None);
        assert_eq!(r.additional_filters.len(), 1);
        assert_eq!(r.additional_filters[0].value, "VIP");
    }

    #[test]
    fn test_resolved_empty_when_no_patterns() {
        let r = resolve("每个月的订单总额", None);
        assert!(r.is_empty());
    }

    #[test]
    fn test_time_inheritance_from_prev_context() {
        let prev = PrevContext {
            question: "上月订单总额",
            sql: "SELECT SUM(amount) FROM orders WHERE ...",
            time_range: Some("2024-04-01 to 2024-04-30"),
            tables: &["orders"],
            filters: &[],
        };
        let r = resolve("那上月呢", Some(&prev));
        assert!(r.time_context.is_some());
        assert_eq!(
            r.time_context.as_ref().unwrap().inherited_range,
            "2024-04-01 to 2024-04-30"
        );
    }
}
