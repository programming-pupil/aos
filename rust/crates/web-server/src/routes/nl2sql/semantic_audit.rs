//! Runtime adapter for the pure Analytics Semantic Compiler contracts.
//!
//! SQL generation remains provider-backed, but the generated statement is
//! parsed into a small, deterministic intent IR before it is released.  This
//! module intentionally never asks a model to "judge" SQL semantics: model
//! output is evidence for the compiler, while the verifier decides what was
//! actually bound and what still needs review.

use nl2sql_core::semantic_ir::{
    AnalyticIntentIR, AnalyticObjective, DataQualityPolicy, DimensionRef, Grain, MetricRef,
    NullPolicy, PopulationDefinition, SecurityScopeRef, SemanticAmbiguity, SemanticVerifier,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlparser::ast::{GroupByExpr, Query, SelectItem, SetExpr, Statement};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

#[derive(Debug, Clone)]
pub(crate) struct SemanticAudit {
    pub intent: AnalyticIntentIR,
    pub verification: nl2sql_core::semantic_ir::SemanticVerification,
}

/// Build a conservative IR from the parsed SQL.  The compiler does not invent
/// metric definitions or time windows: when the statement does not expose
/// enough evidence, the ambiguity is retained in the IR for the verifier and
/// the audit UI instead of being silently treated as a confident answer.
pub(crate) fn compile_and_verify(
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    sql: &str,
    matched_metrics: &[String],
) -> Option<SemanticAudit> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql).ok()?;
    let statement = statements.into_iter().next()?;
    let Statement::Query(query) = statement else {
        return None;
    };
    let (selected_columns, group_by, has_aggregate) = select_shape(&query)?;
    let dimensions = group_by
        .iter()
        .map(|column| DimensionRef {
            name: column.clone(),
            column: column.clone(),
        })
        .collect::<Vec<_>>();
    let objective = objective_for(question, has_aggregate, !dimensions.is_empty());
    let metrics: Vec<MetricRef> = if matched_metrics.is_empty() {
        selected_columns
            .iter()
            .filter(|column| looks_like_aggregate(column))
            .map(|column| MetricRef {
                id: column.clone(),
                version: None,
                display_name: column.clone(),
            })
            .collect()
    } else {
        matched_metrics
            .iter()
            .map(|metric| MetricRef {
                id: metric.clone(),
                version: None,
                display_name: metric.clone(),
            })
            .collect()
    };
    let grain = infer_grain(&dimensions);
    let mut unresolved = Vec::new();
    if matches!(
        objective,
        AnalyticObjective::Aggregate
            | AnalyticObjective::Trend
            | AnalyticObjective::Comparison
            | AnalyticObjective::Attribution
    ) && metrics.is_empty()
    {
        unresolved.push(SemanticAmbiguity {
            field: "metric".to_string(),
            candidates: Vec::new(),
            impact: "the SQL has no selected aggregate and no certified metric binding".to_string(),
        });
    }
    let intent = AnalyticIntentIR {
        objective,
        metrics,
        dimensions,
        grain,
        population: PopulationDefinition {
            subject: "query_rows".to_string(),
            dedup_key: None,
            exclude_test_users: false,
            exclude_internal_users: false,
            valid_record_rule: None,
        },
        filters: Vec::new(),
        time: None,
        comparison: None,
        denominator: None,
        ordering: Vec::new(),
        limit: None,
        null_policy: NullPolicy::Ignore,
        data_quality_policy: DataQualityPolicy::BestEffort,
        security_scope: SecurityScopeRef {
            tenant_id: tenant_id.to_string(),
            datasource_id: datasource_id.to_string(),
            scope_hash: hash_text(&format!("{tenant_id}:{datasource_id}")),
        },
        unresolved,
    };
    let verification =
        SemanticVerifier::default().verify(&intent, &selected_columns, &group_by, &[], sql);
    Some(SemanticAudit {
        intent,
        verification,
    })
}

fn select_shape(query: &Query) -> Option<(Vec<String>, Vec<String>, bool)> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let mut selected = Vec::new();
    let mut has_aggregate = false;
    for item in &select.projection {
        let expr = match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => expr,
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => continue,
        };
        let text = expr.to_string();
        if text.trim().is_empty() {
            continue;
        }
        has_aggregate |= looks_like_aggregate(&text);
        selected.push(text);
    }
    let group_by = match &select.group_by {
        GroupByExpr::Expressions(expressions, _) => expressions
            .iter()
            .map(ToString::to_string)
            .filter(|value| !value.trim().is_empty())
            .collect(),
        GroupByExpr::All(_) => Vec::new(),
    };
    Some((selected, group_by, has_aggregate))
}

fn looks_like_aggregate(expression: &str) -> bool {
    let lower = expression.to_ascii_lowercase();
    ["count(", "sum(", "avg(", "min(", "max(", "approx_distinct("]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn infer_grain(dimensions: &[DimensionRef]) -> Grain {
    let joined = dimensions
        .iter()
        .map(|dimension| dimension.column.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.contains("hour") || joined.contains("hourly") {
        Grain::Hour
    } else if joined.contains("week") {
        Grain::Week
    } else if joined.contains("month") {
        Grain::Month
    } else if joined.contains("day") || joined.contains("date") || joined.contains("dt") {
        Grain::Day
    } else if dimensions.is_empty() {
        Grain::Entity
    } else {
        Grain::Custom("grouped".to_string())
    }
}

fn objective_for(question: &str, aggregate: bool, grouped: bool) -> AnalyticObjective {
    let lower = question.to_ascii_lowercase();
    if lower.contains("归因") || lower.contains("原因") || lower.contains("why") {
        AnalyticObjective::Attribution
    } else if lower.contains("趋势")
        || lower.contains("按日期")
        || lower.contains("每天")
        || lower.contains("trend")
    {
        AnalyticObjective::Trend
    } else if lower.contains("对比")
        || lower.contains("比较")
        || lower.contains("compare")
        || lower.contains("vs")
    {
        AnalyticObjective::Comparison
    } else if aggregate || grouped {
        AnalyticObjective::Aggregate
    } else {
        AnalyticObjective::Lookup
    }
}

pub(crate) fn intent_json(audit: &SemanticAudit) -> Value {
    serde_json::to_value(&audit.intent).unwrap_or_else(|_| serde_json::json!({}))
}

pub(crate) fn verification_json(audit: &SemanticAudit) -> Value {
    serde_json::to_value(&audit.verification).unwrap_or_else(|_| serde_json::json!({}))
}

pub(crate) fn hash_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

fn hash_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_grouped_metric_without_fixed_confidence() {
        let audit = compile_and_verify(
            "tenant",
            "ds",
            "按日期统计订单数",
            "SELECT business_date, COUNT(*) AS orders FROM task_offer GROUP BY business_date",
            &[],
        )
        .expect("semantic audit");
        assert_eq!(audit.intent.dimensions.len(), 1);
        assert_eq!(audit.intent.grain, Grain::Day);
        assert!(audit.verification.confidence_basis.calibrated_score < 1.0);
    }

    #[test]
    fn keeps_missing_metric_as_an_unresolved_ambiguity() {
        let audit = compile_and_verify(
            "tenant",
            "ds",
            "按日期分析趋势",
            "SELECT business_date FROM task_offer GROUP BY business_date",
            &[],
        )
        .expect("semantic audit");
        assert!(audit
            .intent
            .unresolved
            .iter()
            .any(|item| item.field == "metric"));
    }
}
