//! Runtime adapter for the pure Analytics Semantic Compiler contracts.
//!
//! SQL generation remains provider-backed, but it receives a canonical intent
//! compiled before generation. The generated statement contributes physical
//! shape evidence only and can never replace or re-infer that intent. This
//! module intentionally never asks a model to "judge" SQL semantics: model
//! output is evidence for the compiler, while the verifier decides what was
//! actually bound and what still needs review.

use chrono::Datelike;
use nl2sql_core::semantic_ir::{
    AnalyticIntentIR, AnalyticObjective, ComparisonWindow, DataQualityPolicy, DimensionRef, Grain,
    JoinContract, MetricContract, MetricRef, NullPolicy, PopulationDefinition, SecurityScopeRef,
    SemanticAmbiguity, SemanticFilter, SemanticVerifier,
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

#[derive(Debug, Clone)]
pub(crate) struct DurableAnalyticIntent {
    pub intent: AnalyticIntentIR,
    pub metric_contracts: Vec<MetricContract>,
    pub join_contracts: Vec<JoinContract>,
}

impl DurableAnalyticIntent {
    pub(crate) fn intent_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.intent)
    }
}

/// Compile, physically bind, and durably freeze the canonical intent before
/// any provider is allowed to generate SQL. All HTTP and agent entry points
/// share this boundary so a follow-up/clarification route cannot bypass the
/// semantic compiler by calling the SQL generator directly.
pub(crate) async fn compile_bind_and_persist_intent(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    conversation_id: &str,
    intent_id: &str,
    question: &str,
    matched_metrics: &[String],
    schema: &Value,
    evidence_columns: &[String],
    understanding: Option<&crate::nl2sql::query_understanding::QueryUnderstandingResult>,
) -> Result<DurableAnalyticIntent, crate::semantic_kernel_store::SemanticStoreError> {
    let mut intent = compile_question_intent(tenant_id, datasource_id, question, matched_metrics);
    if let Some(understanding) = understanding {
        apply_query_understanding(&mut intent, understanding);
    }
    let metric_ids = intent
        .metrics
        .iter()
        .map(|metric| metric.id.clone())
        .collect::<Vec<_>>();
    let metric_contracts = crate::semantic_kernel_store::load_metric_contracts(
        db,
        tenant_id,
        datasource_id,
        &metric_ids,
    )
    .await?
    .into_iter()
    .map(|stored| stored.contract)
    .collect::<Vec<_>>();
    bind_metric_contracts(&mut intent, &metric_contracts);
    bind_schema_dimensions(&mut intent, schema, evidence_columns);
    let join_contracts =
        crate::semantic_kernel_store::load_join_contracts(db, tenant_id, datasource_id)
            .await?
            .into_iter()
            .map(|stored| stored.contract)
            .collect::<Vec<_>>();
    crate::semantic_kernel_store::persist_nl2sql_intent_ir(
        db,
        tenant_id,
        conversation_id,
        intent_id,
        intent_id,
        &serde_json::to_value(&intent).map_err(|error| {
            crate::semantic_kernel_store::SemanticStoreError::InvalidEvent(format!(
                "failed to encode canonical analytic intent: {error}"
            ))
        })?,
    )
    .await?;
    Ok(DurableAnalyticIntent {
        intent,
        metric_contracts,
        join_contracts,
    })
}

/// Merge the model's structured query-understanding proposal into the
/// canonical IR before schema binding. The proposal can add semantics, but it
/// cannot bypass later deterministic ambiguity, schema, metric, time, filter,
/// or join verification.
pub(crate) fn apply_query_understanding(
    intent: &mut AnalyticIntentIR,
    understanding: &crate::nl2sql::query_understanding::QueryUnderstandingResult,
) {
    use crate::nl2sql::query_understanding::Intent;

    intent.objective = match understanding.intent {
        Intent::Select | Intent::List | Intent::Detail => AnalyticObjective::Lookup,
        Intent::Compare => AnalyticObjective::Comparison,
        Intent::Trend => AnalyticObjective::Trend,
        Intent::Aggregate
        | Intent::Ranking
        | Intent::Count
        | Intent::Sum
        | Intent::Avg
        | Intent::Max
        | Intent::Min => AnalyticObjective::Aggregate,
        Intent::Unknown => intent.objective.clone(),
    };

    if understanding.confidence < 0.55 {
        intent.unresolved.push(SemanticAmbiguity {
            field: "query_understanding".into(),
            candidates: vec![understanding.intent.to_string()],
            impact: format!(
                "structured query-understanding confidence {:.2} is below the release threshold",
                understanding.confidence
            ),
        });
    }

    if let Some(subject) = understanding.entities.subject.as_ref() {
        let proposed_subject = (!subject.raw.trim().is_empty())
            .then(|| subject.raw.trim().to_string())
            .or_else(|| subject.tables.first().cloned());
        if intent.population.subject == "query_rows" {
            if let Some(subject) = proposed_subject {
                intent.population.subject = subject;
            }
        }
    }

    for filter in &understanding.entities.filters {
        let field = filter.column.trim();
        let value = filter.value.trim();
        let operator = filter.op.trim().to_ascii_lowercase();
        if field.is_empty() || value.is_empty() {
            intent.unresolved.push(SemanticAmbiguity {
                field: "filter".into(),
                candidates: vec![filter.raw.clone()],
                impact: "query-understanding proposed an incomplete filter".into(),
            });
            continue;
        }
        let semantic_filter = match operator.as_str() {
            "=" | "==" | "eq" => SemanticFilter::Equals {
                field: field.to_string(),
                value: value.to_string(),
            },
            "in" => SemanticFilter::In {
                field: field.to_string(),
                values: value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect(),
            },
            ">" | ">=" | "<" | "<=" | "!=" | "<>" => SemanticFilter::Compare {
                field: field.to_string(),
                operator,
                value: value.to_string(),
            },
            _ => {
                intent.unresolved.push(SemanticAmbiguity {
                    field: format!("filter:{field}"),
                    candidates: vec![filter.op.clone()],
                    impact: "unsupported filter operator requires clarification".into(),
                });
                continue;
            }
        };
        if !intent.filters.contains(&semantic_filter) {
            intent.filters.push(semantic_filter);
        }
    }

    if let Some(time) = understanding.entities.time.as_ref() {
        if intent.time.is_none() {
            if let Some((start, end)) = time.ranges.first() {
                if !start.trim().is_empty() && !end.trim().is_empty() {
                    let existing_column = intent
                        .time
                        .as_ref()
                        .map(|value| value.column.clone())
                        .unwrap_or_else(|| "business_date".into());
                    intent.time = Some(nl2sql_core::semantic_ir::TimeSemantics {
                        column: existing_column,
                        timezone: intent
                            .time
                            .as_ref()
                            .map(|value| value.timezone.clone())
                            .unwrap_or_else(|| "Asia/Shanghai".into()),
                        start_inclusive: start.clone(),
                        end_exclusive: inclusive_date_end_to_exclusive(end),
                        business_calendar: None,
                        as_of: None,
                    });
                }
            }
        }
        let grouped_by_time = matches!(intent.objective, AnalyticObjective::Trend)
            || [
                "按天", "每天", "按周", "每周", "按月", "每月", "by day", "by week", "by month",
            ]
            .iter()
            .any(|marker| {
                understanding
                    .rewritten_question
                    .to_ascii_lowercase()
                    .contains(marker)
            });
        if grouped_by_time {
            intent.grain = match time.granularity.trim().to_ascii_lowercase().as_str() {
                "hour" => Grain::Hour,
                "day" => Grain::Day,
                "week" => Grain::Week,
                "month" => Grain::Month,
                "" => intent.grain.clone(),
                other => Grain::Custom(other.to_string()),
            };
            if !intent.dimensions.iter().any(|dimension| {
                let lower = format!("{} {}", dimension.name, dimension.column).to_ascii_lowercase();
                lower.contains("date")
                    || lower.contains("day")
                    || lower.contains("日期")
                    || lower.contains("时间")
            }) {
                intent.dimensions.push(DimensionRef {
                    name: "日期".into(),
                    column: intent
                        .time
                        .as_ref()
                        .map(|value| value.column.clone())
                        .unwrap_or_else(|| "business_date".into()),
                });
            }
        }
    }

    if let Some(comparison) = understanding.entities.comparisons.first() {
        let (treatment_window, baseline_window) = understanding
            .entities
            .time
            .as_ref()
            .filter(|time| time.ranges.len() >= 2)
            .map(|time| {
                let to_window = |(start, end): &(String, String)| ComparisonWindow {
                    column: intent
                        .time
                        .as_ref()
                        .map(|value| value.column.clone())
                        .unwrap_or_else(|| "business_date".into()),
                    start_inclusive: start.clone(),
                    end_exclusive: inclusive_date_end_to_exclusive(end),
                    timezone: intent
                        .time
                        .as_ref()
                        .map(|value| value.timezone.clone())
                        .unwrap_or_else(|| "Asia/Shanghai".into()),
                    business_calendar: intent
                        .time
                        .as_ref()
                        .and_then(|value| value.business_calendar.clone()),
                };
                (
                    Some(to_window(&time.ranges[0])),
                    Some(to_window(&time.ranges[1])),
                )
            })
            .unwrap_or((None, None));
        intent.comparison = Some(nl2sql_core::semantic_ir::ComparisonSpec {
            baseline: comparison.raw.clone(),
            treatment: understanding.rewritten_question.clone(),
            method: comparison.comparison_type.clone(),
            baseline_window,
            treatment_window,
        });
    }
}

fn inclusive_date_end_to_exclusive(value: &str) -> String {
    chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|date| date.succ_opt())
        .map_or_else(|| value.to_string(), |date| date.to_string())
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
                if intent.time.is_some() && !contract.time_column.trim().is_empty() {
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
            // A domain without a certified contract may still execute a
            // simple inferred aggregate when the deterministic SQL-shape
            // verifier proves it. Keep version=None so confidence remains
            // below a certified metric, but do not invent an ambiguity where
            // there is no competing definition. Multiple matching contracts
            // remain a hard clarification below.
            [] => {}
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

/// Bind logical dimensions to physical columns visible in the final schema
/// packet. A unique highest-scoring candidate is required; ties and misses are
/// preserved as explicit ambiguity instead of being delegated to the SQL
/// model as an undocumented guess.
pub(crate) fn bind_schema_dimensions(
    intent: &mut AnalyticIntentIR,
    schema_tables: &Value,
    explicit_columns: &[String],
) {
    crate::behavior_trace("SQL-005");
    let mut columns = schema_tables
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|table| {
            table
                .get("columns")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|column| {
            column
                .get("name")
                .or_else(|| column.get("column_name"))
                .and_then(Value::as_str)
        })
        .map(|column| column.trim().to_string())
        .filter(|column| !column.is_empty())
        .collect::<Vec<_>>();
    columns.extend(
        explicit_columns
            .iter()
            .map(|column| column.trim().to_string())
            .filter(|column| !column.is_empty()),
    );
    columns.sort_unstable_by_key(|column| column.to_ascii_lowercase());
    columns.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    if columns.is_empty() {
        for dimension in &intent.dimensions {
            intent.unresolved.push(SemanticAmbiguity {
                field: format!("dimension:physical_column:{}", dimension.name),
                candidates: Vec::new(),
                impact: "no live-schema, synonym, or SQL-knowledge column can prove this dimension binding"
                    .into(),
            });
        }
        if intent.time.is_some() {
            intent.unresolved.push(SemanticAmbiguity {
                field: "time:physical_column".into(),
                candidates: Vec::new(),
                impact:
                    "no live-schema or SQL-knowledge column can prove the requested time semantics"
                        .into(),
            });
        }
        for filter in &intent.filters {
            if let Some(field) = filter_field(filter) {
                intent.unresolved.push(SemanticAmbiguity {
                    field: format!("filter:physical_column:{field}"),
                    candidates: Vec::new(),
                    impact: "no live-schema, synonym, or SQL-knowledge column can prove this filter binding"
                        .into(),
                });
            }
        }
        return;
    }

    for dimension in &mut intent.dimensions {
        let logical = format!("{} {}", dimension.name, dimension.column).to_ascii_lowercase();
        let score = |column: &str| {
            let lower = column.to_ascii_lowercase();
            if lower == dimension.column.to_ascii_lowercase() {
                return 900;
            }
            let base_score =
                if logical.contains("日期") || logical.contains("date") || logical.contains("day")
                {
                    match lower.as_str() {
                        "business_date" => 850,
                        "stat_date" | "event_date" => 825,
                        "date" | "dt" => 800,
                        _ if lower.ends_with("_date") || lower.ends_with("_dt") => 700,
                        _ if lower.contains("date") || lower.contains("day") => 650,
                        _ => 0,
                    }
                } else if logical.contains("设备") || logical.contains("device") {
                    match lower.as_str() {
                        "device_id" => 850,
                        _ if lower.ends_with("device_id") => 800,
                        _ if lower.contains("device") => 650,
                        _ => 0,
                    }
                } else if logical.contains("应用")
                    || logical.contains("产品")
                    || logical.contains("app")
                    || logical.contains("product")
                {
                    match lower.as_str() {
                        "app" => 850,
                        "app_id" | "product_id" => 825,
                        "package_name" | "app_name" | "product_name" => 800,
                        _ if lower.contains("app")
                            || lower.contains("product")
                            || lower.contains("package") =>
                        {
                            650
                        }
                        _ => 0,
                    }
                } else {
                    0
                };
            if base_score > 0
                && explicit_columns
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(column))
            {
                1_000
            } else {
                base_score
            }
        };
        let scored = columns
            .iter()
            .filter_map(|column| {
                let score = score(column);
                (score > 0).then(|| (column, score))
            })
            .collect::<Vec<_>>();
        let Some(max_score) = scored.iter().map(|(_, score)| *score).max() else {
            intent.unresolved.push(SemanticAmbiguity {
                field: format!("dimension:physical_column:{}", dimension.name),
                candidates: columns.iter().take(20).cloned().collect(),
                impact: "no schema column can be bound to the requested dimension".into(),
            });
            continue;
        };
        let winners = scored
            .into_iter()
            .filter(|(_, score)| *score == max_score)
            .map(|(column, _)| column.clone())
            .collect::<Vec<_>>();
        if let [column] = winners.as_slice() {
            dimension.column.clone_from(column);
        } else {
            intent.unresolved.push(SemanticAmbiguity {
                field: format!("dimension:physical_column:{}", dimension.name),
                candidates: winners,
                impact: "multiple schema columns are equally plausible for the requested dimension"
                    .into(),
            });
        }
    }

    if let Some(time) = intent.time.as_mut() {
        if !columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(&time.column))
        {
            let time_score = |column: &str| match column.to_ascii_lowercase().as_str() {
                "business_date" => 1_000,
                "stat_date" | "event_date" => 950,
                "date" | "dt" => 900,
                "event_time" | "event_at" | "occurred_at" => 850,
                "created_at" => 800,
                "updated_at" => 700,
                value if value.ends_with("_date") || value.ends_with("_dt") => 650,
                value if value.ends_with("_time") || value.ends_with("_at") => 600,
                _ => 0,
            };
            let scored = columns
                .iter()
                .filter_map(|column| {
                    let score = time_score(column);
                    (score > 0).then(|| (column, score))
                })
                .collect::<Vec<_>>();
            if let Some(max_score) = scored.iter().map(|(_, score)| *score).max() {
                let winners = scored
                    .into_iter()
                    .filter(|(_, score)| *score == max_score)
                    .map(|(column, _)| column.clone())
                    .collect::<Vec<_>>();
                if let [column] = winners.as_slice() {
                    time.column.clone_from(column);
                } else {
                    intent.unresolved.push(SemanticAmbiguity {
                        field: "time:physical_column".into(),
                        candidates: winners,
                        impact: "multiple schema columns are equally plausible for the requested time semantics"
                            .into(),
                    });
                }
            } else {
                intent.unresolved.push(SemanticAmbiguity {
                    field: "time:physical_column".into(),
                    candidates: columns.iter().take(20).cloned().collect(),
                    impact: "no schema column can be bound to the requested time semantics".into(),
                });
            }
        }
    }

    for filter in &intent.filters {
        let Some(field) = filter_field(filter) else {
            continue;
        };
        if !columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(field))
        {
            intent.unresolved.push(SemanticAmbiguity {
                field: format!("filter:physical_column:{field}"),
                candidates: columns.iter().take(20).cloned().collect(),
                impact: "the proposed filter column is not present in the governed schema".into(),
            });
        }
    }
}

fn filter_field(filter: &SemanticFilter) -> Option<&str> {
    match filter {
        SemanticFilter::Equals { field, .. }
        | SemanticFilter::In { field, .. }
        | SemanticFilter::Compare { field, .. } => Some(field),
        SemanticFilter::RawBounded { .. } => None,
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
    let aggregate_requested = !matched_metrics.is_empty()
        || [
            "多少",
            "数量",
            "总数",
            "count",
            "sum",
            "平均",
            "avg",
            "roi",
            "收入",
            "订单数",
        ]
        .iter()
        .any(|marker| lower.contains(marker));
    let objective = objective_for(
        &lower,
        aggregate_requested,
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
    if metrics.is_empty()
        && ["多少", "数量", "总数", "count", "how many"]
            .iter()
            .any(|marker| lower.contains(marker))
    {
        metrics.push(MetricRef {
            id: "count".into(),
            version: None,
            display_name: "count".into(),
        });
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
    let grain = if matches!(objective, AnalyticObjective::Lookup) {
        Grain::Row
    } else {
        infer_grain(&dimensions)
    };
    let limit = matches!(objective, AnalyticObjective::Lookup).then_some(100);
    AnalyticIntentIR {
        objective,
        metrics,
        dimensions: dimensions.clone(),
        grain,
        population: PopulationDefinition {
            subject: if lower.contains("订单") || lower.contains("order") {
                "order".into()
            } else {
                "query_rows".into()
            },
            dedup_key: if (lower.contains("订单") || lower.contains("order"))
                && (lower.contains("去重")
                    || lower.contains("distinct")
                    || lower.contains("unique"))
            {
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
        limit,
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
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive();
    infer_question_time_at(lower, today)
}

fn infer_question_time_at(
    lower: &str,
    today: chrono::NaiveDate,
) -> Option<nl2sql_core::semantic_ir::TimeSemantics> {
    let (start, end) = if lower.contains("昨天") || lower.contains("yesterday") {
        (today.pred_opt()?, today)
    } else if lower.contains("今天") || lower.contains("today") {
        (today, today.succ_opt()?)
    } else if lower.contains("本月") || lower.contains("this month") {
        let start = chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
        let end = if today.month() == 12 {
            chrono::NaiveDate::from_ymd_opt(today.year().saturating_add(1), 1, 1)?
        } else {
            chrono::NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)?
        };
        (start, end)
    } else {
        return None;
    };
    Some(nl2sql_core::semantic_ir::TimeSemantics {
        column: "business_date".into(),
        timezone: "Asia/Shanghai".into(),
        start_inclusive: start.format("%Y-%m-%d").to_string(),
        end_exclusive: end.format("%Y-%m-%d").to_string(),
        business_calendar: None,
        as_of: None,
    })
}

/// Verify generated SQL against the exact IR that was provided to the SQL
/// generator. The SQL parser contributes physical shape evidence only; it is
/// not allowed to replace the pre-generation business intent with a second,
/// independently inferred intent.
pub(crate) fn compile_canonical_intent_with_contracts_and_joins(
    canonical_intent: &AnalyticIntentIR,
    sql: &str,
    metric_contracts: &[MetricContract],
    joins: &[JoinContract],
) -> Option<SemanticAudit> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql).ok()?;
    let statement = statements.into_iter().next()?;
    let Statement::Query(query) = statement else {
        return None;
    };
    let (selected_columns, group_by, _has_aggregate) = select_shape(&query)?;
    let relevant_joins = bind_query_join_contracts(&query, joins);
    let verification = SemanticVerifier::default().verify_with_metric_contracts(
        canonical_intent,
        &selected_columns,
        &group_by,
        &relevant_joins,
        metric_contracts,
        sql,
    );
    Some(SemanticAudit {
        intent: canonical_intent.clone(),
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
            let is_join = !matches!(join.join_operator, JoinOperator::CrossJoin(_));
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
        TableFactor::Table { name, .. } => {
            let ident = name.0.last()?.as_ident()?;
            Some(
                ident
                    .value
                    .trim_matches(|ch| ch == '`' || ch == '"')
                    .to_ascii_lowercase(),
            )
        }
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
        let text = match item {
            SelectItem::UnnamedExpr(expr) => expr.to_string(),
            SelectItem::ExprWithAlias { expr, alias } => format!("{expr} AS {alias}"),
            SelectItem::ExprWithAliases { .. } => return None,
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => continue,
        };
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
    } else if lower.contains("趋势") || lower.contains("trend") {
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

pub(crate) fn require_release_decision(decision: &str) -> Result<(), String> {
    if decision == "Release" {
        Ok(())
    } else {
        Err(format!(
            "semantic verification did not release this SQL candidate (decision={decision})"
        ))
    }
}

/// Permit a statically verified candidate to run bounded result validation.
/// `ValidateResult` remains rejected by `require_release_decision`; execution
/// observations must promote it to `Release` before a result is final.
pub(crate) fn require_execution_validation_decision(decision: &str) -> Result<(), String> {
    if matches!(decision, "Release" | "ValidateResult") {
        Ok(())
    } else {
        Err(format!(
            "semantic verification did not release this SQL candidate for datasource validation (decision={decision})"
        ))
    }
}

pub(crate) fn hash_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
pub(crate) fn bind_test_count_metric_contract(
    intent: &mut AnalyticIntentIR,
    time_column: &str,
) -> MetricContract {
    use nl2sql_core::semantic_ir::MetricExpressionIR;

    let metric_id = intent
        .metrics
        .first()
        .expect("count fixture requires a compiled metric")
        .id
        .clone();
    let contract = MetricContract {
        id: metric_id.clone(),
        version: 1,
        names: vec![metric_id],
        expression: MetricExpressionIR::Aggregate {
            function: "COUNT".into(),
            expression: Box::new(MetricExpressionIR::Literal("*".into())),
            distinct: false,
        },
        denominator: None,
        population: intent.population.clone(),
        default_grain: intent.grain.clone(),
        allowed_grains: vec![intent.grain.clone()],
        time_column: time_column.into(),
        timezone: intent
            .time
            .as_ref()
            .map(|time| time.timezone.clone())
            .unwrap_or_else(|| "UTC".into()),
        mandatory_filters: intent.filters.clone(),
        join_contracts: vec![],
        invariants: vec![],
        valid_from: "2026-01-01".into(),
        valid_until: None,
        owner: Some("analytics-test".into()),
        evidence_refs: vec!["test://metric/count/v1".into()],
    };
    bind_metric_contracts(intent, std::slice::from_ref(&contract));
    contract
}

fn hash_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_grouped_metric_without_fixed_confidence() {
        let canonical = compile_question_intent("tenant", "ds", "按日期统计订单数", &[]);
        let audit = compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT business_date, COUNT(*) AS orders FROM task_offer GROUP BY business_date",
            &[],
            &[],
        )
        .expect("semantic audit");
        assert_eq!(audit.intent.dimensions.len(), 1);
        assert_eq!(audit.intent.grain, Grain::Day);
        assert!(audit.verification.confidence_basis.calibrated_score < 1.0);
    }

    #[test]
    fn canonical_ir_is_the_audit_source_of_truth_after_sql_generation() {
        let mut canonical =
            compile_question_intent("tenant", "ds", "按日期统计订单数", &["orders".into()]);
        let contract = bind_test_count_metric_contract(&mut canonical, "business_date");
        let audit = compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT business_date, COUNT(*) AS order_count FROM orders GROUP BY business_date",
            &[contract],
            &[],
        )
        .expect("semantic audit");
        assert_eq!(audit.intent, canonical);
        assert_eq!(
            audit.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release
        );

        let mut knowledge_only = compile_question_intent("tenant", "ds", "查询昨天有多少订单", &[]);
        bind_schema_dimensions(
            &mut knowledge_only,
            &serde_json::json!([]),
            &["order_id".into(), "created_at".into()],
        );
        assert_eq!(knowledge_only.time.as_ref().unwrap().column, "created_at");
    }

    #[test]
    fn schema_binding_selects_a_unique_physical_dimension_and_rejects_ties() {
        let mut device = compile_question_intent("tenant", "ds", "按设备统计订单数", &[]);
        bind_schema_dimensions(
            &mut device,
            &serde_json::json!([{
                "table_name": "offers",
                "columns": [{"name": "executor_device_id"}, {"name": "order_id"}]
            }]),
            &[],
        );
        assert_eq!(device.dimensions[0].column, "executor_device_id");
        assert!(device.unresolved.is_empty());

        let mut date = compile_question_intent("tenant", "ds", "按日期统计订单数", &[]);
        bind_schema_dimensions(
            &mut date,
            &serde_json::json!([{
                "table_name": "offers",
                "columns": [{"name": "created_date"}, {"name": "updated_date"}]
            }]),
            &[],
        );
        assert!(date
            .unresolved
            .iter()
            .any(|ambiguity| ambiguity.field.starts_with("dimension:physical_column")));

        let mut explicit = compile_question_intent("tenant", "ds", "按日期统计订单数", &[]);
        bind_schema_dimensions(
            &mut explicit,
            &serde_json::json!([{
                "table_name": "offers",
                "columns": [{"name": "created_date"}, {"name": "updated_date"}]
            }]),
            &["created_date".into()],
        );
        assert_eq!(explicit.dimensions[0].column, "created_date");
        assert!(explicit.unresolved.is_empty());
    }

    #[test]
    fn time_semantics_bind_to_the_real_schema_column_before_sql_generation() {
        let mut canonical = compile_question_intent("tenant", "ds", "查询昨天有多少订单", &[]);
        bind_schema_dimensions(
            &mut canonical,
            &serde_json::json!([{
                "table_name": "orders",
                "columns": [{"name": "order_id"}, {"name": "created_at"}]
            }]),
            &[],
        );
        assert_eq!(canonical.time.as_ref().unwrap().column, "created_at");
        let contract = bind_test_count_metric_contract(&mut canonical, "created_at");
        let time = canonical.time.as_ref().expect("time semantics");
        let sql = format!(
            "SELECT COUNT(*) AS order_count FROM orders WHERE created_at AT TIME ZONE 'Asia/Shanghai' >= DATE '{}' AND created_at AT TIME ZONE 'Asia/Shanghai' < DATE '{}'",
            time.start_inclusive, time.end_exclusive
        );
        let audit =
            compile_canonical_intent_with_contracts_and_joins(&canonical, &sql, &[contract], &[])
                .expect("semantic audit");
        assert_eq!(
            audit.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release
        );
    }

    #[test]
    fn existing_time_binding_does_not_skip_mandatory_filter_schema_validation() {
        let mut canonical = compile_question_intent("tenant", "ds", "查询昨天订单数", &[]);
        canonical.filters.push(SemanticFilter::Equals {
            field: "is_test".into(),
            value: "false".into(),
        });
        bind_schema_dimensions(
            &mut canonical,
            &serde_json::json!([{
                "table_name": "orders",
                "columns": [{"name": "business_date"}, {"name": "order_id"}]
            }]),
            &[],
        );
        assert!(canonical
            .unresolved
            .iter()
            .any(|item| item.field == "filter:physical_column:is_test"));
    }

    #[test]
    fn missing_schema_and_knowledge_columns_remain_unresolved() {
        let mut intent = compile_question_intent(
            "tenant",
            "datasource",
            "按日期统计订单数",
            &["orders".into()],
        );
        bind_schema_dimensions(&mut intent, &serde_json::json!([]), &[]);
        assert!(intent
            .unresolved
            .iter()
            .any(|item| item.field == "dimension:physical_column:日期"));
        assert!(!intent
            .unresolved
            .iter()
            .any(|item| item.field == "time:physical_column"));
    }

    #[test]
    fn inferred_aggregate_without_a_contract_requires_clarification_even_when_shape_matches() {
        let mut canonical =
            compile_question_intent("tenant", "ds", "按日期统计订单数", &["orders".into()]);
        bind_metric_contracts(&mut canonical, &[]);
        assert!(canonical.unresolved.is_empty());
        assert!(canonical
            .metrics
            .iter()
            .all(|metric| metric.version.is_none()));

        let audit = compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT business_date, COUNT(*) AS order_count FROM orders GROUP BY business_date",
            &[],
            &[],
        )
        .expect("semantic audit");
        assert_eq!(
            audit.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::NeedsClarification
        );

        let mismatch = compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT business_date, SUM(amount) AS order_count FROM orders GROUP BY business_date",
            &[],
            &[],
        )
        .expect("semantic audit");
        assert_ne!(
            mismatch.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release
        );
    }

    #[test]
    fn keeps_missing_metric_as_an_unresolved_ambiguity() {
        let canonical = compile_question_intent("tenant", "ds", "按日期分析趋势", &[]);
        let audit = compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT business_date FROM task_offer GROUP BY business_date",
            &[],
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
        let canonical = compile_question_intent("tenant", "ds", "查订单", &[]);
        let audit = compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT order_id FROM orders",
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
        let canonical = compile_question_intent("tenant", "ds", "按天统计订单", &[]);
        let audit = compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT o.business_date, COUNT(*) FROM orders o JOIN users u ON o.user_id = u.id GROUP BY o.business_date",
            &[],
            &[],
        )
        .expect("semantic audit");
        assert_eq!(
            audit.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Reject
        );
    }

    #[test]
    fn every_non_release_semantic_decision_blocks_sql() {
        assert!(require_release_decision("Release").is_ok());
        for decision in ["ValidateResult", "Repair", "NeedsClarification", "Reject"] {
            assert!(require_release_decision(decision).is_err(), "{decision}");
        }
        assert!(require_execution_validation_decision("Release").is_ok());
        assert!(require_execution_validation_decision("ValidateResult").is_ok());
        for decision in ["Repair", "NeedsClarification", "Reject"] {
            assert!(
                require_execution_validation_decision(decision).is_err(),
                "{decision}"
            );
        }
    }
}
