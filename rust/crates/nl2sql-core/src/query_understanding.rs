//! Public domain models for NL2SQL query understanding.
//!
//! This module intentionally contains data contracts only. The LLM/DB-backed
//! QueryUnderstanding service remains in `web-server`, while pure rules in
//! `nl2sql-core` can depend on these shared models without depending on Axum,
//! AppState, or database adapters.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryUnderstandingResult {
    pub rewritten_question: String,
    pub intent: Intent,
    pub entities: QueryEntities,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Intent {
    Select,
    Aggregate,
    Compare,
    Trend,
    Ranking,
    List,
    Detail,
    Count,
    Sum,
    Avg,
    Max,
    Min,
    Unknown,
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Intent::Select => write!(f, "select"),
            Intent::Aggregate => write!(f, "aggregate"),
            Intent::Compare => write!(f, "compare"),
            Intent::Trend => write!(f, "trend"),
            Intent::Ranking => write!(f, "ranking"),
            Intent::List => write!(f, "list"),
            Intent::Detail => write!(f, "detail"),
            Intent::Count => write!(f, "count"),
            Intent::Sum => write!(f, "sum"),
            Intent::Avg => write!(f, "avg"),
            Intent::Max => write!(f, "max"),
            Intent::Min => write!(f, "min"),
            Intent::Unknown => write!(f, "unknown"),
        }
    }
}

pub fn intent_from_label(label: &str) -> Option<Intent> {
    match label {
        "select" => Some(Intent::Select),
        "aggregate" => Some(Intent::Aggregate),
        "compare" => Some(Intent::Compare),
        "trend" => Some(Intent::Trend),
        "ranking" => Some(Intent::Ranking),
        "list" => Some(Intent::List),
        "detail" => Some(Intent::Detail),
        "count" => Some(Intent::Count),
        "sum" => Some(Intent::Sum),
        "avg" => Some(Intent::Avg),
        "max" => Some(Intent::Max),
        "min" => Some(Intent::Min),
        "unknown" => Some(Intent::Unknown),
        _ => None,
    }
}

fn intent_from_question_heuristic(question: &str) -> Intent {
    let q = question.trim().to_lowercase();
    if q.is_empty() {
        return Intent::Unknown;
    }

    if q.contains("同比")
        || q.contains("环比")
        || q.contains("对比")
        || q.contains("比较")
        || q.contains("mom")
        || q.contains("yoy")
        || q.contains("wow")
        || q.contains("qoq")
        || q.contains("versus")
    {
        return Intent::Compare;
    }

    if q.contains("趋势")
        || q.contains("trend")
        || q.contains("变化")
        || q.contains("变化率")
        || q.contains("增长")
        || q.contains("持续上升")
        || q.contains("持续下降")
        || q.contains("连续上升")
        || q.contains("连续下降")
        || q.contains("骤升")
        || q.contains("骤降")
        || q.contains("暴涨")
        || q.contains("暴跌")
        || q.contains("decline")
        || q.contains("drop")
        || q.contains("increase")
        || q.contains("decrease")
    {
        return Intent::Trend;
    }

    if q.contains("top")
        || q.contains("排行")
        || q.contains("排名")
        || q.contains("前十")
        || q.contains("前10")
        || q.contains("前五")
        || q.contains("前5")
    {
        return Intent::Ranking;
    }

    if q.contains("count") || q.contains("数量") || q.contains("个数") || q.contains("总数") {
        return Intent::Count;
    }
    if q.contains("sum") || q.contains("总和") || q.contains("合计") || q.contains("总额") {
        return Intent::Sum;
    }
    if q.contains("avg") || q.contains("average") || q.contains("平均") {
        return Intent::Avg;
    }
    if q.contains("max") || q.contains("最大") || q.contains("最高") {
        return Intent::Max;
    }
    if q.contains("min") || q.contains("最小") || q.contains("最低") {
        return Intent::Min;
    }

    if q.contains("统计")
        || q.contains("汇总")
        || q.contains("聚合")
        || q.contains("占比")
        || q.contains("比例")
    {
        return Intent::Aggregate;
    }

    if q.contains("详情")
        || q.contains("明细")
        || q.contains("信息")
        || q.contains("记录")
        || q.contains("查看")
        || q.contains("查询")
    {
        return Intent::Detail;
    }

    Intent::Select
}

pub fn extract_intent_from_text(raw: &str, question: &str) -> Intent {
    let deterministic = intent_from_question_heuristic(question);
    let apply_strong_override = |model_intent: Intent| {
        if matches!(
            model_intent,
            Intent::Select | Intent::Detail | Intent::List | Intent::Unknown
        ) && matches!(
            deterministic,
            Intent::Compare | Intent::Trend | Intent::Ranking
        ) {
            deterministic
        } else {
            model_intent
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return deterministic;
    }

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let candidate = json
            .get("intent")
            .and_then(|v| v.as_str())
            .or_else(|| json.get("label").and_then(|v| v.as_str()))
            .or_else(|| json.get("type").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if let Some(intent) = intent_from_label(candidate.as_str()) {
            return apply_strong_override(intent);
        }
    }

    let normalized = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_lowercase();

    if let Some(intent) = intent_from_label(normalized.as_str()) {
        return apply_strong_override(intent);
    }

    const LABELS: [&str; 13] = [
        "aggregate",
        "compare",
        "trend",
        "ranking",
        "detail",
        "select",
        "count",
        "sum",
        "avg",
        "max",
        "min",
        "list",
        "unknown",
    ];
    if let Some(found) = LABELS
        .iter()
        .find(|label| normalized.contains(**label))
        .and_then(|label| intent_from_label(label))
    {
        return apply_strong_override(found);
    }

    if normalized.contains("环比")
        || normalized.contains("同比")
        || normalized.contains("对比")
        || normalized.contains("比较")
    {
        return Intent::Compare;
    }
    if normalized.contains("趋势") || normalized.contains("变化") || normalized.contains("增长")
    {
        return Intent::Trend;
    }
    if normalized.contains("排名") || normalized.contains("排行") || normalized.contains("top")
    {
        return Intent::Ranking;
    }
    if normalized.contains("计数") || normalized.contains("数量") || normalized.contains("总数")
    {
        return Intent::Count;
    }
    if normalized.contains("求和") || normalized.contains("总和") || normalized.contains("合计")
    {
        return Intent::Sum;
    }
    if normalized.contains("平均") {
        return Intent::Avg;
    }
    if normalized.contains("最大") || normalized.contains("最高") {
        return Intent::Max;
    }
    if normalized.contains("最小") || normalized.contains("最低") {
        return Intent::Min;
    }
    if normalized.contains("聚合") || normalized.contains("统计") {
        return Intent::Aggregate;
    }
    if normalized.contains("明细") || normalized.contains("详情") {
        return Intent::Detail;
    }
    if normalized.contains("列表") {
        return Intent::List;
    }

    deterministic
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryEntities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeEntity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<SubjectEntity>,
    #[serde(default)]
    pub filters: Vec<FilterEntity>,
    #[serde(default)]
    pub aggregations: Vec<String>,
    #[serde(default)]
    pub comparisons: Vec<ComparisonEntity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeEntity {
    pub raw: String,
    pub resolved_type: String,
    pub granularity: String,
    pub ranges: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectEntity {
    pub tables: Vec<String>,
    pub columns: Vec<String>,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterEntity {
    pub column: String,
    pub value: String,
    pub op: String,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonEntity {
    #[serde(alias = "type", rename = "type")]
    pub comparison_type: String,
    #[serde(rename = "raw")]
    pub raw: String,
}
