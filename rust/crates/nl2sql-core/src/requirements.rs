//! NL2SQL requirement-gate and metric-constraint domain rules.
//!
//! The functions in this module are deterministic and side-effect free: no HTTP,
//! database, AppState, or tenant-specific adapters. Route handlers feed them
//! schema/metric/query-understanding snapshots and consume the returned rules.

use serde::{Deserialize, Serialize};

use crate::query_understanding::{Intent, QueryUnderstandingResult};

/// Human-readable explanation for a missing requirement slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MissingRequirementReason {
    /// Requirement key (metric/time/granularity/dimension/filter).
    pub key: String,
    /// The missing requirement label shown in clarification tags.
    pub requirement: String,
    /// Why this requirement is considered missing for this question.
    pub why_missing: String,
    /// What user can provide to satisfy this requirement.
    pub how_to_provide: String,
    /// A few short examples users can paste directly.
    #[serde(default)]
    pub examples: Vec<String>,
}

#[derive(Debug)]
pub struct RequirementCheckResult {
    pub confirmed: Vec<String>,
    pub missing: Vec<String>,
    pub missing_reasons: Vec<MissingRequirementReason>,
}

#[derive(Debug, Clone)]
pub struct MetricMatchCandidate {
    pub name: String,
    pub aliases: Vec<String>,
    pub expression: Option<String>,
    pub filter_conditions: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct MetricHardConstraint {
    pub metric_name: String,
    pub expression: String,
    pub filter_clause: Option<String>,
}

fn has_any_keyword(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| text.contains(p))
}

pub fn normalize_domain_match_text(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn metric_name_mentioned(question: &str, metric_name: &str) -> bool {
    let q_norm = normalize_domain_match_text(question);
    let m_norm = normalize_domain_match_text(metric_name);
    !m_norm.is_empty() && q_norm.contains(&m_norm)
}

pub fn parse_metric_aliases(metric_aliases: Option<&serde_json::Value>) -> Vec<String> {
    metric_aliases
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn matched_metric_names(question: &str, candidates: &[MetricMatchCandidate]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for candidate in candidates {
        let hit_name = metric_name_mentioned(question, &candidate.name);
        let hit_alias = candidate
            .aliases
            .iter()
            .any(|alias| metric_name_mentioned(question, alias));
        if (hit_name || hit_alias) && !out.iter().any(|n| n == &candidate.name) {
            out.push(candidate.name.clone());
        }
    }
    out
}

pub fn augment_question_for_metric_hint(question: &str, matched_metrics: &[String]) -> String {
    if matched_metrics.len() != 1 {
        return question.to_string();
    }
    let metric_name = matched_metrics[0].trim();
    if metric_name.is_empty() || metric_name_mentioned(question, metric_name) {
        return question.to_string();
    }
    format!("{question}\n指标：{metric_name}")
}

pub fn augment_question_for_metric_generation(
    question: &str,
    matched_metrics: &[String],
) -> String {
    if matched_metrics.len() != 1 {
        return question.to_string();
    }
    let metric_name = matched_metrics[0].trim();
    if metric_name.is_empty() {
        return question.to_string();
    }
    format!(
        "{question}\n系统约束：用户已明确指定指标“{metric_name}”，请直接按该指标定义生成 SQL，不要再次追问“要看哪个指标”。"
    )
}

fn quote_sql_literal(raw: &str) -> String {
    let escaped = raw.replace('\\', "\\\\").replace('\'', "''");
    format!("'{escaped}'")
}

fn normalize_metric_filter_clause(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return None;
    }
    let no_where = if trimmed.len() >= 5 && trimmed[..5].eq_ignore_ascii_case("where") {
        trimmed[5..].trim()
    } else {
        trimmed
    };
    if no_where.is_empty() {
        None
    } else {
        Some(no_where.to_string())
    }
}

fn metric_filter_clause_from_value(value: Option<&serde_json::Value>) -> Option<String> {
    let v = value?;
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => normalize_metric_filter_clause(s),
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = Vec::new();
            for (k, val) in map {
                let key = k.trim();
                if key.is_empty() {
                    continue;
                }
                let expr = match val {
                    serde_json::Value::Null => format!("{key} IS NULL"),
                    serde_json::Value::Bool(b) => {
                        format!("{key} = {}", if *b { "TRUE" } else { "FALSE" })
                    }
                    serde_json::Value::Number(n) => format!("{key} = {n}"),
                    serde_json::Value::String(s) => format!("{key} = {}", quote_sql_literal(s)),
                    other => format!("{key} = {}", quote_sql_literal(&other.to_string())),
                };
                parts.push(expr);
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" AND "))
            }
        }
        serde_json::Value::Array(items) => {
            let mut parts: Vec<String> = Vec::new();
            for item in items {
                if let Some(c) = item.as_str().and_then(normalize_metric_filter_clause) {
                    parts.push(c);
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" AND "))
            }
        }
        other => normalize_metric_filter_clause(&other.to_string()),
    }
}

pub fn resolve_metric_hard_constraint(
    matched_metrics: &[String],
    candidates: &[MetricMatchCandidate],
) -> Option<MetricHardConstraint> {
    if matched_metrics.len() != 1 {
        return None;
    }
    let target_name = &matched_metrics[0];
    let candidate = candidates.iter().find(|c| c.name == *target_name)?;
    let expression = candidate.expression.as_ref()?.trim();
    if expression.is_empty() {
        return None;
    }
    Some(MetricHardConstraint {
        metric_name: candidate.name.clone(),
        expression: expression.to_string(),
        filter_clause: metric_filter_clause_from_value(candidate.filter_conditions.as_ref()),
    })
}

fn parse_sql_expression(expr_sql: &str) -> Option<sqlparser::ast::Expr> {
    use sqlparser::ast::{SelectItem, SetExpr, Statement};
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    let wrapped = format!("SELECT {}", expr_sql.trim());
    let stmts = Parser::parse_sql(&GenericDialect {}, &wrapped).ok()?;
    let stmt = stmts.first()?;
    let Statement::Query(query) = stmt else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let item = select.projection.first()?;
    match item {
        SelectItem::UnnamedExpr(e) => Some(e.clone()),
        SelectItem::ExprWithAlias { expr, .. } => Some(expr.clone()),
        _ => None,
    }
}

fn parse_filter_expression(filter_clause: &str) -> Option<sqlparser::ast::Expr> {
    use sqlparser::ast::{SetExpr, Statement};
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    let wrapped = format!(
        "SELECT * FROM __metric_filter_tmp WHERE {}",
        filter_clause.trim()
    );
    let stmts = Parser::parse_sql(&GenericDialect {}, &wrapped).ok()?;
    let stmt = stmts.first()?;
    let Statement::Query(query) = stmt else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    select.selection.clone()
}

pub fn enforce_metric_hard_constraint_sql(
    sql: &str,
    constraint: &MetricHardConstraint,
) -> Option<String> {
    use sqlparser::ast::{
        BinaryOperator, Expr, GroupByExpr, Ident, SelectItem, SetExpr, Statement,
    };
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    let metric_expr = parse_sql_expression(&constraint.expression)?;
    let filter_expr = constraint
        .filter_clause
        .as_deref()
        .and_then(parse_filter_expression);

    let mut stmts = Parser::parse_sql(&GenericDialect {}, sql).ok()?;
    if stmts.len() != 1 {
        return None;
    }
    let Statement::Query(query) = &mut stmts[0] else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        return None;
    };

    let metric_item = SelectItem::ExprWithAlias {
        expr: metric_expr,
        alias: Ident::new("metric_value"),
    };

    let mut projection: Vec<SelectItem> = Vec::new();
    if let GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            projection.push(SelectItem::UnnamedExpr(expr.clone()));
        }
    }
    projection.push(metric_item);
    select.projection = projection;

    if let Some(extra_filter) = filter_expr {
        select.selection = Some(match select.selection.take() {
            Some(existing) => Expr::BinaryOp {
                left: Box::new(existing),
                op: BinaryOperator::And,
                right: Box::new(extra_filter),
            },
            None => extra_filter,
        });
    }

    Some(stmts[0].to_string())
}

pub fn llm_clarification_reasks_metric(
    clarification_question: &str,
    original_question: &str,
    metrics: &[(String, String, Option<&str>)],
    matched_metrics: &[String],
) -> bool {
    let cq = clarification_question.to_lowercase();
    let asks_metric = cq.contains("指标")
        || cq.contains("metric")
        || cq.contains("which metric")
        || cq.contains("哪些")
        || cq.contains("哪种")
        || cq.contains("哪一个")
        || cq.contains("哪个");
    let references_matched_metric = matched_metrics
        .iter()
        .any(|name| metric_name_mentioned(clarification_question, name));
    if !asks_metric && !references_matched_metric {
        return false;
    }
    let explicit_metrics = if matched_metrics.is_empty() {
        metrics
            .iter()
            .filter_map(|(name, _, _)| {
                if metric_name_mentioned(original_question, name) {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    } else {
        matched_metrics.to_vec()
    };
    explicit_metrics.len() == 1
}

pub fn normalize_sql_time_filters_with_qu(
    sql: &str,
    qu_result: Option<&QueryUnderstandingResult>,
) -> String {
    let Some(qu) = qu_result else {
        return sql.to_string();
    };
    let Some(time) = qu.entities.time.as_ref() else {
        return sql.to_string();
    };
    if time.ranges.is_empty() {
        return sql.to_string();
    }

    let has_normalized_bounds = time
        .ranges
        .iter()
        .any(|(start, end)| sql.contains(start) || sql.contains(end));
    if !has_normalized_bounds {
        return sql.to_string();
    }

    let re_head = match regex::Regex::new(
        r"(?is)\bWHERE\s+[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)?\s*>=\s*DATE_SUB\([^)]*\)\s+AND\s+",
    ) {
        Ok(r) => r,
        Err(_) => return sql.to_string(),
    };
    let sql = re_head.replace_all(sql, "WHERE ").to_string();

    let re_tail = match regex::Regex::new(
        r"(?is)\s+AND\s+[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)?\s*>=\s*DATE_SUB\([^)]*\)",
    ) {
        Ok(r) => r,
        Err(_) => return sql,
    };
    re_tail.replace_all(&sql, "").to_string()
}

fn split_terms(input: &str) -> Vec<String> {
    input
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .map(str::trim)
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequirementIntentProfile {
    EntityLookup,
    AggregateAnalysis,
    ComparativeAnalysis,
}

fn has_marker_with_payload(question: &str, marker: &str) -> bool {
    let mut start = 0usize;
    while let Some(rel) = question[start..].find(marker) {
        let marker_end = start + rel + marker.len();
        let tail = question[marker_end..].trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ':' | '：' | ',' | '，')
        });
        if let Some(ch) = tail.chars().next() {
            if !matches!(ch, '。' | '，' | ',' | '？' | '?' | '！' | '!') {
                return true;
            }
        }
        start = marker_end;
    }
    false
}

fn has_dimension_request_signal(question: &str) -> bool {
    has_any_keyword(question, &["group by", "分组", "维度"])
        || has_marker_with_payload(question, "按")
        || has_marker_with_payload(question, "按照")
        || has_marker_with_payload(question, "每个")
        || has_marker_with_payload(question, "各个")
}

fn has_ranking_signal(question: &str) -> bool {
    has_any_keyword(
        question,
        &["top", "排行", "排名", "前十", "前10", "前五", "前5"],
    )
}

fn has_sort_limit_signal(question: &str) -> bool {
    has_any_keyword(
        question,
        &[
            "倒序", "升序", "排序", "sort by", "order by", "limit", "取前", "取10", "取 10", "top",
            "前10", "前 10", "top10", "top 10",
        ],
    )
}

fn has_detail_lookup_signal(question: &str) -> bool {
    has_any_keyword(
        question,
        &[
            "详情",
            "明细",
            "全部信息",
            "全部字段",
            "所有字段",
            "完整信息",
            "记录",
            "list",
            "detail",
            "查询",
            "查",
        ],
    )
}

fn has_explicit_aggregation_signal(
    question: &str,
    qu_result: Option<&QueryUnderstandingResult>,
) -> bool {
    let keyword_signal = has_any_keyword(
        question,
        &[
            "count",
            "sum",
            "avg",
            "max",
            "min",
            "how many",
            "number of",
            "多少",
            "几条",
            "几笔",
            "几个",
            "几台",
            "数量",
            "总数",
            "总量",
            "金额",
            "占比",
            "比例",
            "增长",
            "统计",
            "汇总",
            "聚合",
        ],
    );

    let qu_signal = qu_result
        .map(|qu| {
            !qu.entities.aggregations.is_empty()
                || matches!(
                    qu.intent,
                    Intent::Aggregate
                        | Intent::Count
                        | Intent::Sum
                        | Intent::Avg
                        | Intent::Max
                        | Intent::Min
                )
        })
        .unwrap_or(false);

    keyword_signal || qu_signal
}

/// Carry only the previous turn's metric/object semantics into an elliptical
/// follow-up requirement check. SQL generation already receives full history;
/// this bounded hint keeps the deterministic clarification gate from rejecting
/// requests such as "group it by date" before generation can use that history.
pub fn augment_follow_up_requirement_context(
    question: &str,
    previous_turn: Option<(&str, &str)>,
) -> String {
    let Some((previous_question, previous_sql)) = previous_turn else {
        return question.to_string();
    };
    let normalized = question.trim().to_lowercase();
    let follow_up_signal = has_dimension_request_signal(&normalized)
        || has_sort_limit_signal(&normalized)
        || has_compare_signal(&normalized, None)
        || has_any_keyword(
            &normalized,
            &[
                "同样", "继续", "再", "改成", "改为", "换成", "只看", "仅看", "排除", "去掉", "呢",
                "also", "instead", "same", "only", "exclude", "group it", "sort it",
            ],
        );
    if !follow_up_signal {
        return question.to_string();
    }

    let previous_question: String = previous_question.chars().take(1_000).collect();
    let previous_sql: String = previous_sql.chars().take(4_000).collect();
    let previous_sql_lower = previous_sql.to_ascii_lowercase();
    let inherited_metric = ["count(", "sum(", "avg(", "min(", "max("]
        .iter()
        .find(|function| previous_sql_lower.contains(**function))
        .map(|function| function.trim_end_matches('('))
        .unwrap_or("previous_projection");
    format!(
        "{question}\n[Previous successful query context for inherited metric/object only]\nInherited metric kind: {inherited_metric}\nQuestion: {previous_question}\nSQL: {previous_sql}"
    )
}

fn has_compare_signal(question: &str, qu_result: Option<&QueryUnderstandingResult>) -> bool {
    qu_result
        .map(|qu| {
            !qu.entities.comparisons.is_empty()
                || matches!(qu.intent, Intent::Compare | Intent::Trend)
        })
        .unwrap_or(false)
        || has_any_keyword(
            question,
            &[
                "环比",
                "同比",
                "趋势",
                "对比",
                "比较",
                "较昨日",
                "较上日",
                "较上周",
                "较上月",
                "mom",
                "yoy",
                "wow",
                "trend",
                "compare",
            ],
        )
}

fn to_simple_chinese_number(n: u32) -> Option<String> {
    match n {
        1 => Some("一".to_string()),
        2 => Some("二".to_string()),
        3 => Some("三".to_string()),
        4 => Some("四".to_string()),
        5 => Some("五".to_string()),
        6 => Some("六".to_string()),
        7 => Some("七".to_string()),
        8 => Some("八".to_string()),
        9 => Some("九".to_string()),
        10 => Some("十".to_string()),
        11..=19 => to_simple_chinese_number(n - 10).map(|tail| format!("十{tail}")),
        20 => Some("二十".to_string()),
        21..=29 => to_simple_chinese_number(n - 20).map(|tail| format!("二十{tail}")),
        30 => Some("三十".to_string()),
        31 => Some("三十一".to_string()),
        _ => None,
    }
}

fn has_explicit_day_window(question: &str) -> bool {
    let q = question.to_lowercase();
    if has_any_keyword(
        &q,
        &[
            "昨天和今天",
            "今天和昨天",
            "昨日和今日",
            "今日和昨日",
            "last 2 days",
            "past 2 days",
        ],
    ) {
        return true;
    }

    for n in 1..=31 {
        let n_text = n.to_string();
        let mut cn_nums: Vec<String> = Vec::new();
        if let Some(cn) = to_simple_chinese_number(n) {
            cn_nums.push(cn);
        }
        if n == 2 {
            cn_nums.push("两".to_string());
        }

        for prefix in ["最近", "近", "过去"] {
            for unit in ["天", "日"] {
                if q.contains(&format!("{prefix}{n_text}{unit}"))
                    || q.contains(&format!("{prefix}{n_text}个{unit}"))
                {
                    return true;
                }
                for cn in &cn_nums {
                    if q.contains(&format!("{prefix}{cn}{unit}"))
                        || q.contains(&format!("{prefix}{cn}个{unit}"))
                    {
                        return true;
                    }
                }
            }
        }

        if q.contains(&format!("last {n_text} days")) || q.contains(&format!("past {n_text} days"))
        {
            return true;
        }
    }

    false
}

fn should_infer_daily_granularity(
    question: &str,
    qu_result: Option<&QueryUnderstandingResult>,
    has_time: bool,
) -> bool {
    if !has_time || !has_compare_signal(question, qu_result) {
        return false;
    }
    if has_explicit_day_window(question) {
        return true;
    }
    qu_result
        .and_then(|qu| qu.entities.time.as_ref())
        .map(|t| t.ranges.len() >= 2 || t.granularity.eq_ignore_ascii_case("day"))
        .unwrap_or(false)
}

fn infer_requirement_profile(
    question: &str,
    qu_result: Option<&QueryUnderstandingResult>,
) -> RequirementIntentProfile {
    let has_aggregation_signal = has_explicit_aggregation_signal(question, qu_result);
    if has_compare_signal(question, qu_result) {
        return RequirementIntentProfile::ComparativeAnalysis;
    }

    let ranking_intent = qu_result
        .map(|qu| matches!(qu.intent, Intent::Ranking))
        .unwrap_or(false)
        || has_ranking_signal(question);
    let ranking_is_detail_lookup = ranking_intent
        && !has_aggregation_signal
        && (has_detail_lookup_signal(question) || has_sort_limit_signal(question));
    if ranking_is_detail_lookup {
        return RequirementIntentProfile::EntityLookup;
    }
    if ranking_intent && has_aggregation_signal {
        return RequirementIntentProfile::AggregateAnalysis;
    }

    if let Some(qu) = qu_result {
        return match qu.intent {
            Intent::Compare | Intent::Trend => RequirementIntentProfile::ComparativeAnalysis,
            Intent::Aggregate
            | Intent::Count
            | Intent::Sum
            | Intent::Avg
            | Intent::Max
            | Intent::Min
            | Intent::Ranking => RequirementIntentProfile::AggregateAnalysis,
            Intent::Select | Intent::List | Intent::Detail => {
                if has_aggregation_signal {
                    RequirementIntentProfile::AggregateAnalysis
                } else {
                    RequirementIntentProfile::EntityLookup
                }
            }
            Intent::Unknown => {
                if has_aggregation_signal {
                    RequirementIntentProfile::AggregateAnalysis
                } else {
                    RequirementIntentProfile::EntityLookup
                }
            }
        };
    }
    if has_aggregation_signal {
        RequirementIntentProfile::AggregateAnalysis
    } else {
        RequirementIntentProfile::EntityLookup
    }
}

pub fn parse_requirements_from_question(
    question: &str,
    qu_result: Option<&QueryUnderstandingResult>,
    schema_tables: &serde_json::Value,
    metrics: &[(String, String, Option<String>)],
) -> RequirementCheckResult {
    let q = question.to_lowercase();
    let mut confirmed: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut missing_reasons: Vec<MissingRequirementReason> = Vec::new();
    let profile = infer_requirement_profile(&q, qu_result);

    let mut metric_terms: Vec<String> = vec![
        "count".to_string(),
        "sum".to_string(),
        "avg".to_string(),
        "max".to_string(),
        "min".to_string(),
        "how many".to_string(),
        "number of".to_string(),
        "多少".to_string(),
        "几条".to_string(),
        "几笔".to_string(),
        "几个".to_string(),
        "几台".to_string(),
        "数量".to_string(),
        "总数".to_string(),
        "总量".to_string(),
        "金额".to_string(),
        "占比".to_string(),
        "增长".to_string(),
        "环比".to_string(),
        "同比".to_string(),
        "新用户".to_string(),
        "活跃用户".to_string(),
        "留存率".to_string(),
        "转化率".to_string(),
    ];
    for (name, expr, _) in metrics {
        let name_lc = name.trim().to_lowercase();
        if name_lc.chars().count() >= 2 {
            metric_terms.push(name_lc);
        }
        metric_terms.extend(split_terms(name));
        metric_terms.extend(split_terms(expr));
    }
    let has_metric = qu_result
        .map(|qu| !qu.entities.aggregations.is_empty())
        .unwrap_or(false)
        || metric_terms.iter().any(|m| !m.is_empty() && q.contains(m));
    let metric_required = !matches!(profile, RequirementIntentProfile::EntityLookup);
    if !metric_required {
        if has_metric {
            confirmed.push("指标/度量已明确".to_string());
        }
    } else if has_metric {
        confirmed.push("指标/度量已明确".to_string());
    } else {
        let requirement = "缺少指标/度量（例如：GMV、订单数、活跃用户）".to_string();
        missing.push(requirement.clone());
        missing_reasons.push(MissingRequirementReason {
            key: "metric".to_string(),
            requirement,
            why_missing: "当前问题属于统计/分析意图，但未明确要统计哪个业务指标。".to_string(),
            how_to_provide: "补充一个明确指标名称（如新用户数、订单数、GMV），可同时说明口径。"
                .to_string(),
            examples: vec![
                "指标看新用户数".to_string(),
                "统计支付订单数".to_string(),
                "按 GMV 计算".to_string(),
            ],
        });
    }

    let has_time = qu_result
        .and_then(|qu| qu.entities.time.as_ref())
        .map(|t| !t.ranges.is_empty())
        .unwrap_or(false)
        || has_any_keyword(
            &q,
            &[
                "today",
                "yesterday",
                "last",
                "week",
                "month",
                "quarter",
                "year",
                "最近",
                "今日",
                "昨天",
                "本周",
                "上周",
                "本月",
                "上月",
                "今年",
                "去年",
                "近7天",
                "近30天",
                "最近两天",
                "近两天",
                "最近2天",
                "近2天",
            ],
        );
    let time_required = matches!(profile, RequirementIntentProfile::ComparativeAnalysis);
    if !time_required {
        if has_time {
            confirmed.push("时间范围已明确".to_string());
        }
    } else if has_time {
        confirmed.push("时间范围已明确".to_string());
    } else {
        let requirement = "缺少时间范围（例如：最近7天、本月、2026-05）".to_string();
        missing.push(requirement.clone());
        missing_reasons.push(MissingRequirementReason {
            key: "time_range".to_string(),
            requirement,
            why_missing: "当前问题涉及对比/趋势分析，但未给出可计算的时间窗口。".to_string(),
            how_to_provide: "补充明确时间范围（相对时间或绝对日期都可以）。".to_string(),
            examples: vec![
                "最近7天".to_string(),
                "本月".to_string(),
                "2026-05-01 到 2026-05-14".to_string(),
            ],
        });
    }

    // Root-cause fix (not keyword-only):
    // Prefer structured QU signals. If QU identifies comparison/trend intent
    // with time context, treat time grain as implied.
    let granularity_from_qu = qu_result
        .map(|qu| {
            let explicit_granularity = qu
                .entities
                .time
                .as_ref()
                .map(|t| !t.granularity.trim().is_empty() && t.granularity.to_lowercase() != "none")
                .unwrap_or(false);

            let has_time_context = qu
                .entities
                .time
                .as_ref()
                .map(|t| !t.ranges.is_empty() || !t.raw.trim().is_empty())
                .unwrap_or(false);

            let has_compare_signal = !qu.entities.comparisons.is_empty()
                || matches!(qu.intent, Intent::Compare | Intent::Trend);

            let compare_keyword_signal = has_any_keyword(
                &q,
                &[
                    "环比",
                    "同比",
                    "较昨日",
                    "较上日",
                    "较上周",
                    "较上月",
                    "对比",
                    "比较",
                ],
            );

            let multi_period_ranges = qu
                .entities
                .time
                .as_ref()
                .map(|t| t.ranges.len() >= 2)
                .unwrap_or(false);

            explicit_granularity
                || ((has_compare_signal || compare_keyword_signal || multi_period_ranges)
                    && has_time_context)
        })
        .unwrap_or(false);

    let has_granularity = qu_result
        .and_then(|qu| qu.entities.time.as_ref())
        .map(|t| !t.granularity.trim().is_empty() && t.granularity.to_lowercase() != "none")
        .unwrap_or(false)
        || granularity_from_qu
        || should_infer_daily_granularity(question, qu_result, has_time)
        || has_any_keyword(
            &q,
            &[
                "按天",
                "按周",
                "按月",
                "按年",
                "按日",
                "按小时",
                "按季度",
                "daily",
                "weekly",
                "monthly",
                "yearly",
                "hourly",
                "quarterly",
                "趋势",
                "trend",
                "每日",
                "每天",
                "逐日",
                "周度",
                "月度",
            ],
        );
    let granularity_required =
        matches!(profile, RequirementIntentProfile::ComparativeAnalysis) && has_time;
    if !granularity_required {
        if has_granularity {
            confirmed.push("统计粒度已明确".to_string());
        }
    } else if has_granularity {
        confirmed.push("统计粒度已明确".to_string());
    } else {
        let requirement = "缺少统计粒度（例如：按天/按周/按月）".to_string();
        missing.push(requirement.clone());
        missing_reasons.push(MissingRequirementReason {
            key: "granularity".to_string(),
            requirement,
            why_missing:
                "时间范围与统计粒度是两个概念：前者决定看哪段时间，后者决定结果按天/周/月如何汇总。"
                    .to_string(),
            how_to_provide: "补充“按天/按周/按月”；若希望系统默认按天，也可直接写“按天”。"
                .to_string(),
            examples: vec![
                "按天统计".to_string(),
                "按周汇总".to_string(),
                "按月看趋势".to_string(),
            ],
        });
    }

    let mut dimension_terms: Vec<String> = vec![
        "by ".to_string(),
        "group by".to_string(),
        "按".to_string(),
        "分组".to_string(),
        "维度".to_string(),
    ];
    if let Some(tables) = schema_tables.as_array() {
        for table in tables {
            let table_name = table
                .get("table_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            dimension_terms.extend(split_terms(table_name));
            if let Some(cols) = table.get("columns").and_then(|v| v.as_array()) {
                for col in cols {
                    let col_name = col
                        .get("name")
                        .or_else(|| col.get("column_name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    dimension_terms.extend(split_terms(col_name));
                    let col_name_lc = col_name.to_lowercase();
                    if col_name_lc.chars().count() >= 2 {
                        dimension_terms.push(col_name_lc);
                    }
                }
            }
            let table_name_lc = table_name.to_lowercase();
            if table_name_lc.chars().count() >= 2 {
                dimension_terms.push(table_name_lc);
            }
        }
    }
    let has_dimension_phrase = has_dimension_request_signal(question);
    let has_dimension_from_schema = dimension_terms
        .iter()
        .any(|d| d.len() >= 2 && q.contains(d))
        || has_any_keyword(
            &q,
            &[
                "按部门",
                "按地区",
                "按渠道",
                "按产品",
                "按用户",
                "按租户",
                "每个租户",
                "各租户",
            ],
        );
    let has_dimension = has_dimension_phrase || has_dimension_from_schema;
    let dimension_required = matches!(profile, RequirementIntentProfile::ComparativeAnalysis)
        && has_dimension_request_signal(question);
    if !dimension_required {
        if has_dimension {
            confirmed.push("分组维度已明确".to_string());
        }
    } else if has_dimension {
        confirmed.push("分组维度已明确".to_string());
    } else {
        let requirement = "缺少分组维度（例如：按地区、按产品、按部门）".to_string();
        missing.push(requirement.clone());
        missing_reasons.push(MissingRequirementReason {
            key: "dimension".to_string(),
            requirement,
            why_missing: "当前问题表达了分组/对比诉求，但未明确按哪个维度拆分。".to_string(),
            how_to_provide: "补充“按 X”或“每个 X”的维度字段。".to_string(),
            examples: vec![
                "按租户分组".to_string(),
                "按地区统计".to_string(),
                "每个产品线分别看".to_string(),
            ],
        });
    }

    let has_filter = qu_result
        .map(|qu| !qu.entities.filters.is_empty())
        .unwrap_or(false)
        || has_any_keyword(
            &q,
            &[
                "where", "仅", "只看", "排除", "过滤", ">= ", "<= ", "=", "大于", "小于", "等于",
                "between", " and ",
            ],
        );
    if has_filter {
        confirmed.push("筛选范围已明确".to_string());
    }

    RequirementCheckResult {
        confirmed,
        missing,
        missing_reasons,
    }
}

#[cfg(test)]
mod requirement_check_tests {
    use super::{
        augment_follow_up_requirement_context, enforce_metric_hard_constraint_sql,
        llm_clarification_reasks_metric, matched_metric_names, normalize_sql_time_filters_with_qu,
        parse_metric_aliases, parse_requirements_from_question, resolve_metric_hard_constraint,
        MetricMatchCandidate,
    };
    use crate::query_understanding::{
        ComparisonEntity, FilterEntity, Intent, QueryEntities, QueryUnderstandingResult, TimeEntity,
    };

    fn qu_with(
        intent: Intent,
        time: Option<TimeEntity>,
        filters: Vec<FilterEntity>,
        comparisons: Vec<ComparisonEntity>,
        aggregations: Vec<&str>,
    ) -> QueryUnderstandingResult {
        QueryUnderstandingResult {
            rewritten_question: String::new(),
            intent,
            entities: QueryEntities {
                time,
                subject: None,
                filters,
                aggregations: aggregations.into_iter().map(|s| s.to_string()).collect(),
                comparisons,
            },
            confidence: 1.0,
        }
    }

    #[test]
    fn comparative_question_with_tenant_and_daily_mom_should_not_miss_dimension_or_granularity() {
        let question = "查一下最近两天每个租户的新用户，每日要环比";
        let qu = qu_with(
            Intent::Compare,
            Some(TimeEntity {
                raw: "最近两天".to_string(),
                resolved_type: "relative".to_string(),
                granularity: "none".to_string(),
                ranges: vec![
                    ("2026-05-13".to_string(), "2026-05-13".to_string()),
                    ("2026-05-14".to_string(), "2026-05-14".to_string()),
                ],
            }),
            vec![],
            vec![ComparisonEntity {
                comparison_type: "mom".to_string(),
                raw: "环比".to_string(),
            }],
            vec!["count"],
        );

        let result =
            parse_requirements_from_question(question, Some(&qu), &serde_json::json!([]), &[]);
        assert!(!result
            .missing
            .iter()
            .any(|m| m.contains("统计粒度") || m.contains("分组维度")));
    }

    #[test]
    fn comparative_question_with_recent_three_days_should_infer_daily_granularity() {
        let question = "查最近三天每个租户新用户，做环比";
        let qu = qu_with(
            Intent::Compare,
            Some(TimeEntity {
                raw: "最近三天".to_string(),
                resolved_type: "relative".to_string(),
                granularity: "none".to_string(),
                ranges: vec![("2026-05-12".to_string(), "2026-05-14".to_string())],
            }),
            vec![],
            vec![ComparisonEntity {
                comparison_type: "mom".to_string(),
                raw: "环比".to_string(),
            }],
            vec!["count"],
        );

        let result =
            parse_requirements_from_question(question, Some(&qu), &serde_json::json!([]), &[]);
        assert!(!result.missing.iter().any(|m| m.contains("统计粒度")));
    }

    #[test]
    fn entity_lookup_should_not_force_metric_time_or_dimension_clarification() {
        let question = "查一下叫小明的用户信息";
        let qu = qu_with(
            Intent::Detail,
            None,
            vec![FilterEntity {
                column: "name".to_string(),
                value: "小明".to_string(),
                op: "=".to_string(),
                raw: "叫小明".to_string(),
            }],
            vec![],
            vec![],
        );
        let result =
            parse_requirements_from_question(question, Some(&qu), &serde_json::json!([]), &[]);
        assert!(result.missing.is_empty());
    }

    #[test]
    fn aggregate_question_without_metric_should_still_request_metric() {
        let question = "统计最近两天的数据";
        let qu = qu_with(
            Intent::Aggregate,
            Some(TimeEntity {
                raw: "最近两天".to_string(),
                resolved_type: "relative".to_string(),
                granularity: "day".to_string(),
                ranges: vec![("2026-05-13".to_string(), "2026-05-14".to_string())],
            }),
            vec![],
            vec![],
            vec![],
        );
        let result =
            parse_requirements_from_question(question, Some(&qu), &serde_json::json!([]), &[]);
        assert!(result.missing.iter().any(|m| m.contains("指标/度量")));
        assert!(result.missing_reasons.iter().any(|r| r.key == "metric"));
    }

    #[test]
    fn ranking_detail_lookup_should_not_require_metric_or_granularity() {
        let question = "查最近注册的用户top10，按照注册日期倒序排序，查详情，全部信息";
        let qu = qu_with(
            Intent::Ranking,
            Some(TimeEntity {
                raw: "最近".to_string(),
                resolved_type: "relative".to_string(),
                granularity: "none".to_string(),
                ranges: vec![("2026-05-08".to_string(), "2026-05-15".to_string())],
            }),
            vec![],
            vec![],
            vec![],
        );
        let result =
            parse_requirements_from_question(question, Some(&qu), &serde_json::json!([]), &[]);
        assert!(!result.missing.iter().any(|m| m.contains("指标/度量")));
        assert!(!result.missing.iter().any(|m| m.contains("统计粒度")));
    }

    #[test]
    fn count_question_with_natural_quantity_word_does_not_require_metric_clarification() {
        let question = "查询昨天有多少个对象";
        let qu = qu_with(Intent::Count, None, vec![], vec![], vec![]);
        let result =
            parse_requirements_from_question(question, Some(&qu), &serde_json::json!([]), &[]);
        assert!(!result.missing.iter().any(|m| m.contains("指标/度量")));
    }

    #[test]
    fn elliptical_grouping_follow_up_inherits_previous_count_metric() {
        let augmented = augment_follow_up_requirement_context(
            "按照日期统计下",
            Some((
                "查下都有哪些对象",
                "SELECT object_id, COUNT(*) AS object_count FROM records GROUP BY object_id",
            )),
        );
        let qu = qu_with(Intent::Aggregate, None, vec![], vec![], vec![]);
        let result =
            parse_requirements_from_question(&augmented, Some(&qu), &serde_json::json!([]), &[]);
        assert!(!result.missing.iter().any(|m| m.contains("指标/度量")));
    }

    #[test]
    fn chinese_metric_name_should_be_recognized_as_metric() {
        let question = "查一下用户表的测试指标";
        let qu = qu_with(Intent::Aggregate, None, vec![], vec![], vec![]);
        let metrics = vec![(
            "测试指标".to_string(),
            "count(distinct id)".to_string(),
            None,
        )];
        let result =
            parse_requirements_from_question(question, Some(&qu), &serde_json::json!([]), &metrics);
        assert!(!result.missing.iter().any(|m| m.contains("指标/度量")));
    }

    #[test]
    fn metric_alias_should_be_recognized_as_metric() {
        let question = "查询用户表的zb";
        let qu = qu_with(Intent::Aggregate, None, vec![], vec![], vec![]);
        let metrics = vec![(
            "测试指标".to_string(),
            "count(distinct id)".to_string(),
            None,
        )];
        let candidates = vec![MetricMatchCandidate {
            name: "测试指标".to_string(),
            aliases: vec!["zb".to_string(), "test_zb".to_string()],
            expression: None,
            filter_conditions: None,
        }];
        let matched = matched_metric_names(question, &candidates);
        assert_eq!(matched, vec!["测试指标".to_string()]);

        let result = parse_requirements_from_question(
            &format!("{question}\n指标：测试指标"),
            Some(&qu),
            &serde_json::json!([]),
            &metrics,
        );
        assert!(!result.missing.iter().any(|m| m.contains("指标/度量")));
    }

    #[test]
    fn parse_metric_aliases_should_handle_json_array() {
        let aliases = serde_json::json!(["zb", "test_zb", "  "]);
        let parsed = parse_metric_aliases(Some(&aliases));
        assert_eq!(parsed, vec!["zb".to_string(), "test_zb".to_string()]);
    }

    #[test]
    fn resolve_metric_hard_constraint_should_return_expression_and_filter() {
        let candidates = vec![MetricMatchCandidate {
            name: "测试指标".to_string(),
            aliases: vec!["zb".to_string()],
            expression: Some("count(distinct id)".to_string()),
            filter_conditions: Some(serde_json::Value::String(
                "where id is not null".to_string(),
            )),
        }];
        let matched = vec!["测试指标".to_string()];
        let constraint = resolve_metric_hard_constraint(&matched, &candidates)
            .expect("hard constraint should be resolved");
        assert_eq!(constraint.metric_name, "测试指标");
        assert_eq!(constraint.expression, "count(distinct id)");
        assert_eq!(constraint.filter_clause.as_deref(), Some("id is not null"));
    }

    #[test]
    fn enforce_metric_hard_constraint_sql_should_replace_projection_and_merge_filter() {
        let sql = "SELECT u.tenant_id, COUNT(*) AS c FROM users u WHERE u.enabled = 1 GROUP BY u.tenant_id";
        let constraint = super::MetricHardConstraint {
            metric_name: "测试指标".to_string(),
            expression: "count(distinct u.id)".to_string(),
            filter_clause: Some("u.id is not null".to_string()),
        };
        let rewritten =
            enforce_metric_hard_constraint_sql(sql, &constraint).expect("rewrite should succeed");
        let normalized = rewritten.to_lowercase();
        assert!(normalized.contains("count(distinct u.id) as metric_value"));
        assert!(normalized.contains("where u.enabled = 1 and u.id is not null"));
        assert!(normalized.contains("group by u.tenant_id"));
    }

    #[test]
    fn redundant_metric_clarification_should_be_blocked_for_unique_metric_match() {
        let clarification = "您想查看 users 表的哪些“测试指标”？例如用户总数、活跃用户数。";
        let original = "查询用户表的测试指标";
        let metrics = vec![("测试指标".to_string(), "count(*)".to_string(), None)];
        let matched = vec!["测试指标".to_string()];
        assert!(llm_clarification_reasks_metric(
            clarification,
            original,
            &metrics,
            &matched
        ));
    }

    #[test]
    fn normalize_sql_time_filters_should_drop_redundant_relative_clause_when_qu_has_bounds() {
        let qu = qu_with(
            Intent::Detail,
            Some(TimeEntity {
                raw: "年初至今".to_string(),
                resolved_type: "ytd".to_string(),
                granularity: "day".to_string(),
                ranges: vec![("2026-01-01".to_string(), "2026-05-16".to_string())],
            }),
            vec![],
            vec![],
            vec![],
        );
        let sql = "SELECT * FROM users AS u WHERE u.created_at >= DATE_SUB('2026-05-16', INTERVAL 10 DAY) AND u.created_at >= '2026-01-01' AND u.created_at < '2026-05-16' LIMIT 1000";
        let normalized = normalize_sql_time_filters_with_qu(sql, Some(&qu));
        assert!(!normalized.contains("DATE_SUB('2026-05-16', INTERVAL 10 DAY)"));
        assert!(normalized.contains("u.created_at >= '2026-01-01'"));
        assert!(normalized.contains("u.created_at < '2026-05-16'"));
    }
}

pub fn build_requirement_clarification_question(missing: &[String]) -> String {
    let compact: Vec<&str> = missing.iter().map(|m| m.as_str()).collect();
    format!(
        "为保证结果准确，我还缺少关键信息：{}。请补充后我再生成 SQL。",
        compact.join("；")
    )
}
