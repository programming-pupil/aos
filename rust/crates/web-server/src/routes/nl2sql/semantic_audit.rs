//! Runtime adapter for the pure Analytics Semantic Compiler contracts.
//!
//! SQL generation remains provider-backed, but the generated statement is
//! parsed into a small, deterministic intent IR before it is released.  This
//! module intentionally never asks a model to "judge" SQL semantics: model
//! output is evidence for the compiler, while the verifier decides what was
//! actually bound and what still needs review.

use nl2sql_core::semantic_ir::{
    AnalyticIntentIR, AnalyticObjective, DataQualityPolicy, DimensionRef, Grain, JoinContract,
    MetricContract, MetricRef, NullPolicy, PopulationDefinition, SecurityScopeRef,
    SemanticAmbiguity, SemanticVerifier,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlparser::ast::{
    GroupByExpr, JoinOperator, Query, SelectItem, SetExpr, Statement, TableFactor,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

#[derive(Debug, Clone)]
pub(crate) struct SemanticAudit {
    pub intent: AnalyticIntentIR,
    pub verification: nl2sql_core::semantic_ir::SemanticVerification,
}

/// Bind natural-language metric candidates to the tenant's versioned contracts.
/// This is deliberately deterministic: aliases are matched case-insensitively,
/// and an ambiguous alias remains unresolved instead of choosing an arbitrary
/// version.  The verifier then prevents an unbound aggregate from being
/// released as a semantically confident answer.
pub(crate) fn bind_metric_contracts(intent: &mut AnalyticIntentIR, contracts: &[MetricContract]) {
    for metric in &mut intent.metrics {
        let matches = contracts
            .iter()
            .filter(|contract| {
                contract.id.eq_ignore_ascii_case(&metric.id)
                    || contract.names.iter().any(|name| {
                        name.eq_ignore_ascii_case(&metric.id)
                            || name.eq_ignore_ascii_case(&metric.display_name)
                    })
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [contract] => {
                metric.id = contract.id.clone();
                metric.version = Some(contract.version);
                metric.display_name = contract
                    .names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| metric.display_name.clone());
                if intent.population.subject == "query_rows" {
                    intent.population = contract.population.clone();
                }
                if intent.time.is_some() {
                    if let Some(time) = intent.time.as_mut() {
                        time.column = contract.time_column.clone();
                        time.timezone = contract.timezone.clone();
                    }
                }
                for filter in &contract.mandatory_filters {
                    if !intent.filters.contains(filter) {
                        intent.filters.push(filter.clone());
                    }
                }
                if !contract.allowed_grains.is_empty()
                    && !contract.allowed_grains.contains(&intent.grain)
                    && intent.grain != contract.default_grain
                {
                    intent.unresolved.push(SemanticAmbiguity {
                        field: "grain".into(),
                        candidates: contract
                            .allowed_grains
                            .iter()
                            .map(|grain| format!("{grain:?}"))
                            .collect(),
                        impact: format!(
                            "metric contract {} does not allow the requested grain",
                            contract.id
                        ),
                    });
                }
            }
            [] => {
                if !intent.unresolved.iter().any(|item| {
                    item.field == "metric"
                        && item
                            .candidates
                            .iter()
                            .any(|candidate| candidate.eq_ignore_ascii_case(&metric.id))
                }) {
                    intent.unresolved.push(SemanticAmbiguity {
                        field: "metric".into(),
                        candidates: vec![metric.id.clone()],
                        impact: "no active versioned metric contract is bound".into(),
                    });
                }
            }
            _ => {
                intent.unresolved.push(SemanticAmbiguity {
                    field: "metric".into(),
                    candidates: matches
                        .iter()
                        .map(|contract| format!("{}@{}", contract.id, contract.version))
                        .collect(),
                    impact: "multiple active metric contracts match this name".into(),
                });
            }
        }
    }
}

/// Compile the user's request before SQL generation. Providers may enrich this
/// proposal, but they cannot remove its unresolved semantics.
pub(crate) fn compile_question_intent(
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    matched_metrics: &[String],
) -> AnalyticIntentIR {
    let lower = question.to_ascii_lowercase();
    let objective = objective_for(
        &lower,
        true,
        lower.contains("按") || lower.contains("group"),
    );
    let mut metrics = matched_metrics
        .iter()
        .map(|metric| MetricRef {
            id: metric.clone(),
            version: None,
            display_name: metric.clone(),
        })
        .collect::<Vec<_>>();
    if metrics.is_empty() {
        let candidates = [
            ("roi", ["roi", "回报", "投资回报"].as_slice()),
            ("orders", ["订单", "order"].as_slice()),
            ("revenue", ["收入", "流水", "revenue"].as_slice()),
            ("users", ["用户", "活跃", "user"].as_slice()),
        ];
        if let Some((id, _)) = candidates
            .iter()
            .find(|(_, aliases)| aliases.iter().any(|alias| lower.contains(alias)))
        {
            metrics.push(MetricRef {
                id: (*id).to_string(),
                version: None,
                display_name: (*id).to_string(),
            });
        }
    }
    let mut dimensions = Vec::new();
    if lower.contains("日期")
        || lower.contains("每天")
        || lower.contains("按天")
        || lower.contains("date")
    {
        dimensions.push(DimensionRef {
            name: "日期".into(),
            column: "business_date".into(),
        });
    } else if lower.contains("设备") || lower.contains("device") {
        dimensions.push(DimensionRef {
            name: "设备".into(),
            column: "device_id".into(),
        });
    } else if lower.contains("app") || lower.contains("产品") || lower.contains("渠道") {
        dimensions.push(DimensionRef {
            name: "应用/产品".into(),
            column: "app".into(),
        });
    }
    let time = infer_question_time(&lower);
    let mut unresolved = Vec::new();
    if metrics.is_empty()
        && matches!(
            objective,
            AnalyticObjective::Aggregate
                | AnalyticObjective::Trend
                | AnalyticObjective::Comparison
                | AnalyticObjective::Attribution
        )
    {
        unresolved.push(SemanticAmbiguity {
            field: "metric".into(),
            candidates: vec!["count".into(), "sum".into(), "average".into()],
            impact: "the requested measure changes the answer".into(),
        });
    }
    AnalyticIntentIR {
        objective,
        metrics,
        dimensions: dimensions.clone(),
        grain: infer_grain(&dimensions),
        population: PopulationDefinition {
            subject: if lower.contains("订单") || lower.contains("order") {
                "order".into()
            } else {
                "query_rows".into()
            },
            dedup_key: if lower.contains("订单") || lower.contains("order") {
                Some("order_id".into())
            } else {
                None
            },
            exclude_test_users: false,
            exclude_internal_users: false,
            valid_record_rule: None,
        },
        filters: Vec::new(),
        time,
        comparison: None,
        denominator: None,
        ordering: Vec::new(),
        limit: Some(100),
        null_policy: NullPolicy::Ignore,
        data_quality_policy: DataQualityPolicy::BestEffort,
        security_scope: SecurityScopeRef {
            tenant_id: tenant_id.into(),
            datasource_id: datasource_id.into(),
            scope_hash: hash_text(&format!("{tenant_id}:{datasource_id}")),
        },
        unresolved,
    }
}

fn infer_question_time(lower: &str) -> Option<nl2sql_core::semantic_ir::TimeSemantics> {
    let (start, end) = if lower.contains("昨天") || lower.contains("yesterday") {
        ("yesterday", "today")
    } else if lower.contains("今天") || lower.contains("today") {
        ("today", "tomorrow")
    } else if lower.contains("本月") || lower.contains("this month") {
        ("month_start", "month_end")
    } else {
        return None;
    };
    Some(nl2sql_core::semantic_ir::TimeSemantics {
        column: "business_date".into(),
        timezone: "Asia/Shanghai".into(),
        start_inclusive: start.into(),
        end_exclusive: end.into(),
        business_calendar: None,
        as_of: None,
    })
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
    compile_and_verify_with_contracts(
        tenant_id,
        datasource_id,
        question,
        sql,
        matched_metrics,
        &[],
    )
}

pub(crate) fn compile_and_verify_with_contracts(
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    sql: &str,
    matched_metrics: &[String],
    metric_contracts: &[MetricContract],
) -> Option<SemanticAudit> {
    compile_and_verify_with_contracts_and_joins(
        tenant_id,
        datasource_id,
        question,
        sql,
        matched_metrics,
        metric_contracts,
        &[],
    )
}

pub(crate) fn compile_and_verify_with_contracts_and_joins(
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
    sql: &str,
    matched_metrics: &[String],
    metric_contracts: &[MetricContract],
    joins: &[JoinContract],
) -> Option<SemanticAudit> {
    let question_intent =
        compile_question_intent(tenant_id, datasource_id, question, matched_metrics);
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
    let mut intent = AnalyticIntentIR {
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
    if !question_intent.metrics.is_empty() {
        intent.metrics = question_intent.metrics;
    }
    if question_intent.time.is_some() {
        intent.time = question_intent.time;
    }
    if !question_intent.unresolved.is_empty() && intent.unresolved.is_empty() {
        intent.unresolved = question_intent.unresolved;
    }
    bind_metric_contracts(&mut intent, metric_contracts);
    let relevant_joins = bind_query_join_contracts(&query, joins);
    let verification = SemanticVerifier::default().verify_with_metric_contracts(
        &intent,
        &selected_columns,
        &group_by,
        &relevant_joins,
        metric_contracts,
        sql,
    );
    Some(SemanticAudit {
        intent,
        verification,
    })
}

/// Keep only contracts for joins that are present in this statement.  A
/// tenant may have unrelated risky joins; they must not poison an otherwise
/// independent query.  Conversely, an actual join without exactly one
/// matching contract is represented as a conservative fanout risk and is
/// rejected by the verifier.
fn bind_query_join_contracts(query: &Query, contracts: &[JoinContract]) -> Vec<JoinContract> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return vec![unverified_join_contract()];
    };
    let mut result = Vec::new();
    for table in &select.from {
        let mut seen = Vec::new();
        if let Some(base) = table_factor_name(&table.relation) {
            seen.push(base);
        } else if !table.joins.is_empty() {
            result.push(unverified_join_contract());
            continue;
        }
        for join in &table.joins {
            let Some(right) = table_factor_name(&join.relation) else {
                result.push(unverified_join_contract());
                continue;
            };
            let is_join = !matches!(join.join_operator, JoinOperator::CrossJoin);
            if is_join {
                let matches = contracts
                    .iter()
                    .filter(|contract| {
                        seen.iter()
                            .any(|left| contract_matches_pair(contract, left, &right))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if matches.len() == 1 {
                    result.push(matches.into_iter().next().expect("length checked"));
                } else {
                    result.push(unverified_join_contract());
                }
            } else {
                result.push(unverified_join_contract());
            }
            seen.push(right);
        }
    }
    result
}

fn table_factor_name(factor: &TableFactor) -> Option<String> {
    match factor {
        TableFactor::Table { name, .. } => name.0.last().map(|ident| {
            ident
                .value
                .trim_matches(|ch| ch == '`' || ch == '"')
                .to_ascii_lowercase()
        }),
        _ => None,
    }
}

fn contract_matches_pair(contract: &JoinContract, left: &str, right: &str) -> bool {
    let names_match = |expected: &str, actual: &str| {
        let expected = expected
            .rsplit('.')
            .next()
            .unwrap_or(expected)
            .trim_matches(|ch| ch == '`' || ch == '"')
            .to_ascii_lowercase();
        expected == actual
    };
    (names_match(&contract.left_table, left) && names_match(&contract.right_table, right))
        || (names_match(&contract.left_table, right) && names_match(&contract.right_table, left))
}

fn unverified_join_contract() -> JoinContract {
    JoinContract {
        id: "unverified-sql-join".into(),
        left_table: String::new(),
        right_table: String::new(),
        left_keys: Vec::new(),
        right_keys: Vec::new(),
        cardinality: nl2sql_core::semantic_ir::JoinCardinality::ManyToMany,
        temporal_condition: None,
        nullable: true,
        dedup_strategy: None,
        allowed_grains: Vec::new(),
        fanout_risk: true,
    }
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

    #[test]
    fn unrelated_risky_join_contract_does_not_poison_single_table_query() {
        let contract = JoinContract {
            id: "risky-other".into(),
            left_table: "other_a".into(),
            right_table: "other_b".into(),
            left_keys: vec!["id".into()],
            right_keys: vec!["id".into()],
            cardinality: nl2sql_core::semantic_ir::JoinCardinality::ManyToMany,
            temporal_condition: None,
            nullable: false,
            dedup_strategy: None,
            allowed_grains: vec![],
            fanout_risk: true,
        };
        let audit = compile_and_verify_with_contracts_and_joins(
            "tenant",
            "ds",
            "查订单",
            "SELECT order_id FROM orders",
            &[],
            &[],
            &[contract],
        )
        .expect("semantic audit");
        assert_ne!(
            audit.verification.join_cardinality.status,
            nl2sql_core::semantic_ir::CheckStatus::Fail
        );
    }

    #[test]
    fn actual_unverified_join_is_fail_closed() {
        let audit = compile_and_verify_with_contracts_and_joins(
            "tenant",
            "ds",
            "按天统计订单",
            "SELECT o.business_date, COUNT(*) FROM orders o JOIN users u ON o.user_id = u.id GROUP BY o.business_date",
            &[],
            &[],
            &[],
        )
        .expect("semantic audit");
        assert_eq!(
            audit.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Reject
        );
    }
}
