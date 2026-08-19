//! Analytics Semantic Compiler contracts.
//!
//! Provider-backed SQL generation is downstream of this IR. Every new or
//! repaired query must carry the same immutable intent into deterministic
//! verification; a parseable SQL string is never semantic proof by itself.

use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    BinaryOperator, DuplicateTreatment, Expr, FunctionArg, FunctionArgExpr, FunctionArguments,
    ObjectName, Query, SelectItem, SetExpr, Statement, Visit, Visitor,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use std::collections::BTreeSet;
use std::ops::ControlFlow;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnalyticObjective {
    Lookup,
    Aggregate,
    Trend,
    Comparison,
    Attribution,
    DataQuality,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceLevel {
    L0Descriptive,
    L1Decomposition,
    L2QuasiExperimental,
    L3RandomizedExperiment,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CausalAnalysisContract {
    pub evidence_level: EvidenceLevel,
    pub treatment: String,
    pub outcome: String,
    pub unit: String,
    pub pre_window: String,
    pub post_window: String,
    pub control: Option<String>,
    pub confounders: Vec<String>,
    pub interference_assumptions: Vec<String>,
    pub missingness_policy: String,
    pub estimator: String,
    pub uncertainty_interval: String,
    pub robustness_checks: Vec<String>,
}
impl CausalAnalysisContract {
    pub fn permits_causal_language(&self) -> bool {
        matches!(
            self.evidence_level,
            EvidenceLevel::L2QuasiExperimental | EvidenceLevel::L3RandomizedExperiment
        ) && !self.estimator.trim().is_empty()
            && !self.uncertainty_interval.trim().is_empty()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetricRef {
    pub id: String,
    pub version: Option<u64>,
    pub display_name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DimensionRef {
    pub name: String,
    pub column: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Grain {
    Row,
    Entity,
    Hour,
    Day,
    Week,
    Month,
    Custom(String),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PopulationDefinition {
    pub subject: String,
    pub dedup_key: Option<String>,
    pub exclude_test_users: bool,
    pub exclude_internal_users: bool,
    pub valid_record_rule: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SemanticFilter {
    Equals {
        field: String,
        value: String,
    },
    In {
        field: String,
        values: Vec<String>,
    },
    Compare {
        field: String,
        operator: String,
        value: String,
    },
    RawBounded {
        expression: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeSemantics {
    pub column: String,
    pub timezone: String,
    pub start_inclusive: String,
    pub end_exclusive: String,
    pub business_calendar: Option<String>,
    pub as_of: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComparisonSpec {
    pub baseline: String,
    pub treatment: String,
    pub method: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DenominatorSpec {
    pub expression: String,
    pub population: PopulationDefinition,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderSpec {
    pub expression: String,
    pub descending: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NullPolicy {
    Ignore,
    Zero,
    SeparateBucket,
    Fail,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DataQualityPolicy {
    BestEffort,
    RequireFreshness,
    FailOnAnomaly,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecurityScopeRef {
    pub tenant_id: String,
    pub datasource_id: String,
    pub scope_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticAmbiguity {
    pub field: String,
    pub candidates: Vec<String>,
    pub impact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalyticIntentIR {
    pub objective: AnalyticObjective,
    pub metrics: Vec<MetricRef>,
    pub dimensions: Vec<DimensionRef>,
    pub grain: Grain,
    pub population: PopulationDefinition,
    pub filters: Vec<SemanticFilter>,
    pub time: Option<TimeSemantics>,
    pub comparison: Option<ComparisonSpec>,
    pub denominator: Option<DenominatorSpec>,
    pub ordering: Vec<OrderSpec>,
    pub limit: Option<u64>,
    pub null_policy: NullPolicy,
    pub data_quality_policy: DataQualityPolicy,
    pub security_scope: SecurityScopeRef,
    pub unresolved: Vec<SemanticAmbiguity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MetricExpressionIR {
    Column(String),
    Aggregate {
        function: String,
        expression: Box<MetricExpressionIR>,
        distinct: bool,
    },
    Ratio {
        numerator: Box<MetricExpressionIR>,
        denominator: Box<MetricExpressionIR>,
    },
    Literal(String),
}

/// Parse one metric expression into the canonical contract representation.
/// The parser rejects statement injection and preserves complex, valid SQL as
/// a literal expression rather than inventing semantics for syntax it cannot
/// decompose deterministically.
pub fn parse_metric_expression_ir(sql: &str) -> Result<MetricExpressionIR, String> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err("metric expression is empty".into());
    }
    let wrapped = format!("SELECT {trimmed}");
    let statements = Parser::parse_sql(&GenericDialect {}, &wrapped)
        .map_err(|error| format!("invalid metric expression: {error}"))?;
    if statements.len() != 1 {
        return Err("metric expression must contain exactly one expression".into());
    }
    let Statement::Query(query) = &statements[0] else {
        return Err("metric expression must be a SELECT expression".into());
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err("metric expression cannot contain set operations".into());
    };
    if select.projection.len() != 1 || !select.from.is_empty() || select.selection.is_some() {
        return Err("metric expression cannot contain a query body".into());
    }
    let expression = match &select.projection[0] {
        SelectItem::UnnamedExpr(expression)
        | SelectItem::ExprWithAlias {
            expr: expression, ..
        } => expression,
        _ => return Err("metric expression cannot be a wildcard projection".into()),
    };
    Ok(metric_expression_from_ast(expression))
}

fn metric_expression_from_ast(expression: &Expr) -> MetricExpressionIR {
    match expression {
        Expr::Identifier(identifier) => MetricExpressionIR::Column(identifier.value.clone()),
        Expr::CompoundIdentifier(identifiers) => MetricExpressionIR::Column(
            identifiers
                .iter()
                .map(|identifier| identifier.value.as_str())
                .collect::<Vec<_>>()
                .join("."),
        ),
        Expr::Nested(expression) => metric_expression_from_ast(expression),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Divide,
            right,
        } => MetricExpressionIR::Ratio {
            numerator: Box::new(metric_expression_from_ast(left)),
            denominator: Box::new(metric_expression_from_ast(right)),
        },
        Expr::Function(function) => {
            let FunctionArguments::List(arguments) = &function.args else {
                return MetricExpressionIR::Literal(expression.to_string());
            };
            if arguments.args.len() != 1 || !arguments.clauses.is_empty() {
                return MetricExpressionIR::Literal(expression.to_string());
            }
            let argument = match &arguments.args[0] {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(argument)) => {
                    metric_expression_from_ast(argument)
                }
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
                    MetricExpressionIR::Literal("*".into())
                }
                _ => return MetricExpressionIR::Literal(expression.to_string()),
            };
            MetricExpressionIR::Aggregate {
                function: function.name.to_string().to_ascii_uppercase(),
                expression: Box::new(argument),
                distinct: matches!(
                    arguments.duplicate_treatment,
                    Some(DuplicateTreatment::Distinct)
                ),
            }
        }
        _ => MetricExpressionIR::Literal(expression.to_string()),
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetricContract {
    pub id: String,
    pub version: u64,
    pub names: Vec<String>,
    pub expression: MetricExpressionIR,
    pub denominator: Option<MetricExpressionIR>,
    pub population: PopulationDefinition,
    pub default_grain: Grain,
    pub allowed_grains: Vec<Grain>,
    pub time_column: String,
    pub timezone: String,
    pub mandatory_filters: Vec<SemanticFilter>,
    pub join_contracts: Vec<String>,
    pub invariants: Vec<ResultInvariant>,
    pub valid_from: String,
    pub valid_until: Option<String>,
    pub owner: Option<String>,
    pub evidence_refs: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JoinCardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JoinContract {
    pub id: String,
    pub left_table: String,
    pub right_table: String,
    pub left_keys: Vec<String>,
    pub right_keys: Vec<String>,
    pub cardinality: JoinCardinality,
    pub temporal_condition: Option<String>,
    pub nullable: bool,
    pub dedup_strategy: Option<String>,
    pub allowed_grains: Vec<Grain>,
    pub fanout_risk: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResultInvariant {
    NonNegative {
        field: String,
    },
    RatioBounded {
        field: String,
        lower: i64,
        upper: i64,
    },
    SumMatches {
        total_field: String,
        parts_field: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResultInvariantObservation {
    pub invariant: ResultInvariant,
    pub status: CheckStatus,
    pub code: String,
    pub message: String,
    pub rows_checked: usize,
}

fn numeric_result_field(row: &serde_json::Value, field: &str) -> Option<f64> {
    let value = row.as_object()?.get(field)?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
}

/// Evaluate metric-contract invariants against the bounded result returned by
/// the datasource. Missing fields and empty results are `NotChecked`, never a
/// vacuous pass. This is the second phase of semantic release.
pub fn evaluate_result_invariants(
    invariants: &[ResultInvariant],
    rows: &[serde_json::Value],
) -> Vec<ResultInvariantObservation> {
    invariants
        .iter()
        .cloned()
        .map(|invariant| {
            if rows.is_empty() {
                return ResultInvariantObservation {
                    invariant,
                    status: CheckStatus::NotChecked,
                    code: "result_invariant_no_rows".into(),
                    message: "the datasource returned no rows, so the invariant was not observed"
                        .into(),
                    rows_checked: 0,
                };
            }
            let (status, code, message, rows_checked) = match &invariant {
                ResultInvariant::NonNegative { field } => {
                    let values = rows
                        .iter()
                        .filter_map(|row| numeric_result_field(row, field))
                        .collect::<Vec<_>>();
                    if values.len() != rows.len() {
                        (
                            CheckStatus::NotChecked,
                            "result_invariant_field_missing",
                            format!("result field `{field}` is missing or non-numeric"),
                            values.len(),
                        )
                    } else if values.iter().all(|value| *value >= 0.0) {
                        (
                            CheckStatus::Pass,
                            "result_non_negative",
                            format!("all `{field}` values are non-negative"),
                            values.len(),
                        )
                    } else {
                        (
                            CheckStatus::Fail,
                            "result_negative_value",
                            format!("`{field}` contains a negative value"),
                            values.len(),
                        )
                    }
                }
                ResultInvariant::RatioBounded {
                    field,
                    lower,
                    upper,
                } => {
                    let values = rows
                        .iter()
                        .filter_map(|row| numeric_result_field(row, field))
                        .collect::<Vec<_>>();
                    if values.len() != rows.len() {
                        (
                            CheckStatus::NotChecked,
                            "result_invariant_field_missing",
                            format!("result field `{field}` is missing or non-numeric"),
                            values.len(),
                        )
                    } else if values
                        .iter()
                        .all(|value| *value >= *lower as f64 && *value <= *upper as f64)
                    {
                        (
                            CheckStatus::Pass,
                            "result_ratio_bounded",
                            format!("all `{field}` values are within [{lower}, {upper}]"),
                            values.len(),
                        )
                    } else {
                        (
                            CheckStatus::Fail,
                            "result_ratio_out_of_bounds",
                            format!("`{field}` contains a value outside [{lower}, {upper}]"),
                            values.len(),
                        )
                    }
                }
                ResultInvariant::SumMatches {
                    total_field,
                    parts_field,
                } => {
                    let matches = rows
                        .iter()
                        .filter_map(|row| {
                            let object = row.as_object()?;
                            let total = numeric_result_field(row, total_field)?;
                            let parts = object.get(parts_field)?;
                            let sum = if let Some(values) = parts.as_array() {
                                values.iter().map(serde_json::Value::as_f64).sum::<Option<f64>>()?
                            } else {
                                numeric_result_field(row, parts_field)?
                            };
                            Some((total - sum).abs() <= 1e-9)
                        })
                        .collect::<Vec<_>>();
                    if matches.len() != rows.len() {
                        (
                            CheckStatus::NotChecked,
                            "result_invariant_field_missing",
                            format!(
                                "result fields `{total_field}` and `{parts_field}` are missing or non-numeric"
                            ),
                            matches.len(),
                        )
                    } else if matches.iter().all(|matches| *matches) {
                        (
                            CheckStatus::Pass,
                            "result_sum_matches",
                            format!("`{total_field}` equals `{parts_field}` for every row"),
                            matches.len(),
                        )
                    } else {
                        (
                            CheckStatus::Fail,
                            "result_sum_mismatch",
                            format!("`{total_field}` does not equal `{parts_field}`"),
                            matches.len(),
                        )
                    }
                }
            };
            ResultInvariantObservation {
                invariant,
                status,
                code: code.into(),
                message,
                rows_checked,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    NotChecked,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckResult {
    pub status: CheckStatus,
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryReleaseDecision {
    /// Static semantics are proven, but metric-contract result invariants
    /// still require bounded datasource execution. This state may cross the
    /// execution gate, but it is not a releasable user result.
    ValidateResult,
    Release,
    NeedsClarification,
    Repair,
    Reject,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticVerification {
    pub safety: CheckResult,
    pub schema_binding: CheckResult,
    pub metric_equivalence: CheckResult,
    pub denominator_equivalence: CheckResult,
    pub population_equivalence: CheckResult,
    pub grain_consistency: CheckResult,
    pub time_consistency: CheckResult,
    pub comparison_consistency: CheckResult,
    pub join_cardinality: CheckResult,
    pub filter_completeness: CheckResult,
    pub null_semantics: CheckResult,
    pub ordering_consistency: CheckResult,
    pub limit_consistency: CheckResult,
    pub policy_compliance: CheckResult,
    pub result_invariants: Vec<CheckResult>,
    pub executable: Option<CheckResult>,
    pub confidence_basis: ConfidenceBasis,
    pub release_decision: QueryReleaseDecision,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfidenceBasis {
    pub binding_margin: f32,
    pub ambiguity_count: u32,
    pub schema_coverage: f32,
    pub verifier_pass_rate: f32,
    pub execution_passed: bool,
    pub model_disagreement: f32,
    pub calibrated_score: f32,
}

/// Calibration diagnostics used by offline evaluation and the feedback
/// dashboard.  Scores are probabilities and labels are human/verified
/// outcomes; empty input is intentionally defined as zero rather than NaN so
/// telemetry jobs remain total.
pub fn calibration_metrics(observations: &[(f32, bool)], bins: usize) -> (f32, f32) {
    if observations.is_empty() {
        return (0.0, 0.0);
    }
    let bin_count = bins.max(1);
    let mut ece = 0.0_f32;
    let mut brier = 0.0_f32;
    for (score, label) in observations {
        let p = score.clamp(0.0, 1.0);
        let target = if *label { 1.0 } else { 0.0 };
        brier += (p - target).powi(2);
    }
    for bin in 0..bin_count {
        let lower = bin as f32 / bin_count as f32;
        let upper = (bin + 1) as f32 / bin_count as f32;
        let members = observations
            .iter()
            .filter(|(score, _)| {
                let p = score.clamp(0.0, 1.0);
                p >= lower && (p < upper || (bin + 1 == bin_count && p <= upper))
            })
            .collect::<Vec<_>>();
        if members.is_empty() {
            continue;
        }
        let confidence = members
            .iter()
            .map(|(score, _)| score.clamp(0.0, 1.0))
            .sum::<f32>()
            / members.len() as f32;
        let accuracy =
            members.iter().filter(|(_, label)| *label).count() as f32 / members.len() as f32;
        ece += members.len() as f32 / observations.len() as f32 * (confidence - accuracy).abs();
    }
    (ece, brier / observations.len() as f32)
}

#[derive(Debug, Clone, Default)]
pub struct SemanticVerifier;
impl SemanticVerifier {
    pub fn verify(
        &self,
        ir: &AnalyticIntentIR,
        selected_columns: &[String],
        group_by: &[String],
        joins: &[JoinContract],
        sql: &str,
    ) -> SemanticVerification {
        self.verify_with_metric_contracts(ir, selected_columns, group_by, joins, &[], sql)
    }

    pub fn verify_with_metric_contracts(
        &self,
        ir: &AnalyticIntentIR,
        selected_columns: &[String],
        group_by: &[String],
        joins: &[JoinContract],
        metric_contracts: &[MetricContract],
        sql: &str,
    ) -> SemanticVerification {
        let query_shape = ParsedQueryShape::parse(sql);
        let safety = if query_shape.is_some() {
            pass("safe", "read-only SELECT candidate")
        } else {
            fail(
                "unsafe_statement",
                "candidate is not one read-only SELECT/CTE query",
            )
        };
        let required: BTreeSet<_> = ir.dimensions.iter().map(|d| d.column.as_str()).collect();
        let dimension_is_selected = |dimension: &&str| {
            selected_columns
                .iter()
                .any(|selected| semantic_identifier_matches(dimension, selected))
        };
        let schema_binding = if required.iter().all(dimension_is_selected) {
            pass("schema_bound", "all requested dimensions are selected")
        } else {
            fail("missing_dimension", "a requested dimension is not selected")
        };
        let grain_consistency = match ir.grain {
            Grain::Row => pass("grain_row", "row grain does not require grouping"),
            _ if required.iter().all(|dimension| {
                group_by
                    .iter()
                    .any(|group| semantic_identifier_matches(dimension, group))
            }) =>
            {
                pass("grain_bound", "grouping matches requested dimensions")
            }
            _ => fail("grain_mismatch", "GROUP BY does not match requested grain"),
        };
        let fanout = joins.iter().any(|join| {
            join.fanout_risk
                || (join.cardinality == JoinCardinality::ManyToMany
                    && join.dedup_strategy.as_deref().is_none_or(str::is_empty))
        });
        let join_cardinality = if fanout {
            fail(
                "join_fanout",
                "join contract has many-to-many or unverified fanout risk",
            )
        } else {
            pass("join_safe", "join cardinality is bounded")
        };
        let missing_mandatory = ir
            .filters
            .iter()
            .filter(|filter| {
                query_shape
                    .as_ref()
                    .is_none_or(|shape| !shape_contains_filter(shape, filter))
            })
            .count();
        let filter_completeness = if missing_mandatory > 0 {
            warn(
                "filter_review",
                &format!(
                    "{missing_mandatory} mandatory metric/population filter(s) are not proven in SQL"
                ),
            )
        } else {
            pass(
                "filters_ok",
                "all represented mandatory filters are present",
            )
        };
        let unresolved = ir.unresolved.len() as u32;
        let bound_contracts = ir
            .metrics
            .iter()
            .filter_map(|metric| {
                metric_contracts.iter().find(|contract| {
                    contract.id.eq_ignore_ascii_case(&metric.id)
                        && metric.version == Some(contract.version)
                })
            })
            .collect::<Vec<_>>();
        let metric_required = !matches!(ir.objective, AnalyticObjective::Lookup);
        let metric_equivalence = if !ir.metrics.is_empty()
            && bound_contracts.len() == ir.metrics.len()
            && bound_contracts.iter().all(|contract| {
                selected_columns
                    .iter()
                    .any(|selected| metric_expression_matches(&contract.expression, selected))
            }) {
            pass(
                "metric_equivalent",
                "selected metric expressions match their contracts",
            )
        } else if !ir.metrics.is_empty()
            && bound_contracts.is_empty()
            && inferred_metrics_match(&ir.metrics, selected_columns)
        {
            pass(
                "metric_inferred_equivalent",
                "requested metrics are represented by selected aggregate expressions",
            )
        } else if ir.metrics.is_empty() && metric_required {
            fail(
                "metric_missing",
                "no metric is bound for an analytic request",
            )
        } else if ir.metrics.is_empty() {
            pass(
                "metric_not_required",
                "lookup request does not require an aggregate metric",
            )
        } else if bound_contracts.len() == ir.metrics.len() {
            fail(
                "metric_expression_mismatch",
                "a selected metric expression is not equivalent to its versioned contract",
            )
        } else {
            warn(
                "metric_unverified",
                "metric name is inferred but no versioned contract is bound",
            )
        };
        let denominator_equivalence =
            verify_denominator(ir, &bound_contracts, query_shape.as_ref());
        let population_equivalence = if ir.population.subject.trim().is_empty() {
            fail("population_missing", "population subject is empty")
        } else if query_shape
            .as_ref()
            .is_none_or(|shape| !population_subject_is_proven(shape, &ir.population.subject))
        {
            fail(
                "population_subject_unproven",
                "the requested population is not represented by the SQL candidate",
            )
        } else if ir.population.dedup_key.as_deref().is_some_and(|key| {
            query_shape
                .as_ref()
                .is_none_or(|shape| !population_dedup_is_proven(shape, key))
        }) {
            fail(
                "population_dedup_unproven",
                "the population deduplication policy is not proven by DISTINCT semantics",
            )
        } else if ir.population.exclude_test_users
            && query_shape
                .as_ref()
                .is_none_or(|shape| !population_exclusion_is_proven(shape, "test"))
        {
            fail(
                "test_population_exclusion_unproven",
                "test-user exclusion is required but is not proven in SQL",
            )
        } else if ir.population.exclude_internal_users
            && query_shape
                .as_ref()
                .is_none_or(|shape| !population_exclusion_is_proven(shape, "internal"))
        {
            fail(
                "internal_population_exclusion_unproven",
                "internal-user exclusion is required but is not proven in SQL",
            )
        } else if ir
            .population
            .valid_record_rule
            .as_deref()
            .is_some_and(|rule| {
                let expected = parse_sql_expression(rule);
                query_shape.as_ref().is_none_or(|shape| {
                    expected.as_ref().is_none_or(|expected| {
                        !shape.predicates.iter().any(|predicate| {
                            any_expression(predicate, &|actual| {
                                expression_equivalent(actual, expected)
                            })
                        })
                    })
                })
            })
        {
            fail(
                "valid_record_rule_unproven",
                "the certified valid-record rule is not present in SQL",
            )
        } else {
            pass(
                "population_checked",
                "population subject and dedup policy are represented in the IR",
            )
        };
        let time_consistency = if let Some(time) = ir.time.as_ref() {
            if query_shape.as_ref().is_some_and(|shape| {
                sql_matches_time_semantics(shape, time)
                    && sql_matches_timezone_semantics(shape, time)
            }) {
                pass(
                    "time_bound",
                    "time column, boundary semantics and timezone are represented",
                )
            } else {
                fail(
                    "time_filter_missing",
                    "the requested time column/filter is not proven in SQL",
                )
            }
        } else if matches!(
            ir.objective,
            AnalyticObjective::Trend | AnalyticObjective::Comparison
        ) {
            fail(
                "time_missing",
                "trend/comparison intent requires explicit time semantics",
            )
        } else {
            pass(
                "time_not_requested",
                "the intent does not require a time window",
            )
        };
        let comparison_consistency = verify_comparison(ir, sql);
        let null_semantics = verify_null_semantics(ir, query_shape.as_ref());
        let ordering_consistency = verify_ordering(ir, query_shape.as_ref());
        let limit_consistency = verify_limit(ir, query_shape.as_ref());
        let policy_compliance = if ir.security_scope.tenant_id.trim().is_empty()
            || ir.security_scope.datasource_id.trim().is_empty()
            || ir.security_scope.scope_hash.trim().is_empty()
        {
            fail(
                "policy_scope_unproven",
                "tenant, datasource and policy scope hash must all be bound",
            )
        } else {
            pass(
                "policy_scope_bound",
                "tenant, datasource and immutable policy scope are bound",
            )
        };
        let result_invariants = bound_contracts
            .iter()
            .flat_map(|contract| contract.invariants.iter())
            .map(|invariant| CheckResult {
                status: CheckStatus::NotChecked,
                code: "result_invariant_pending".into(),
                message: format!("result invariant requires execution evidence: {invariant:?}"),
            })
            .collect::<Vec<_>>();
        let checks = [
            &safety,
            &schema_binding,
            &metric_equivalence,
            &denominator_equivalence,
            &population_equivalence,
            &grain_consistency,
            &time_consistency,
            &comparison_consistency,
            &join_cardinality,
            &filter_completeness,
            &null_semantics,
            &ordering_consistency,
            &limit_consistency,
            &policy_compliance,
        ];
        let passed = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Pass)
            .count() as f32
            / checks.len() as f32;
        let decision = if safety.status == CheckStatus::Fail
            || join_cardinality.status == CheckStatus::Fail
            || policy_compliance.status == CheckStatus::Fail
        {
            QueryReleaseDecision::Reject
        } else if schema_binding.status == CheckStatus::Fail
            || metric_equivalence.status == CheckStatus::Fail
            || denominator_equivalence.status == CheckStatus::Fail
        {
            QueryReleaseDecision::Repair
        } else if unresolved > 0
            || filter_completeness.status == CheckStatus::Warn
            || grain_consistency.status == CheckStatus::Fail
            || metric_equivalence.status == CheckStatus::Warn
            || population_equivalence.status != CheckStatus::Pass
            || time_consistency.status != CheckStatus::Pass
            || comparison_consistency.status != CheckStatus::Pass
            || null_semantics.status != CheckStatus::Pass
            || ordering_consistency.status != CheckStatus::Pass
            || limit_consistency.status != CheckStatus::Pass
        {
            QueryReleaseDecision::NeedsClarification
        } else if result_invariants
            .iter()
            .any(|check| check.status != CheckStatus::Pass)
        {
            QueryReleaseDecision::ValidateResult
        } else {
            QueryReleaseDecision::Release
        };
        let schema_is_pass = schema_binding.status == CheckStatus::Pass;
        SemanticVerification {
            safety,
            schema_binding,
            metric_equivalence,
            denominator_equivalence,
            population_equivalence,
            grain_consistency,
            time_consistency,
            comparison_consistency,
            join_cardinality,
            filter_completeness,
            null_semantics,
            ordering_consistency,
            limit_consistency,
            policy_compliance,
            result_invariants,
            executable: None,
            confidence_basis: ConfidenceBasis {
                binding_margin: if schema_is_pass { 1.0 } else { 0.0 },
                ambiguity_count: unresolved,
                schema_coverage: if schema_is_pass { 1.0 } else { 0.0 },
                verifier_pass_rate: passed,
                execution_passed: false,
                model_disagreement: 0.0,
                // Calibration deliberately leaves headroom until execution and
                // result invariants have passed; a parseable SQL candidate is
                // never allowed to become an artificial 1.0.
                calibrated_score: (0.5 + (passed * 0.45) - (unresolved as f32 * 0.1))
                    .clamp(0.0, 0.95),
            },
            release_decision: decision,
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedQueryShape {
    projection: Vec<String>,
    projection_exprs: Vec<Expr>,
    projection_aliases: Vec<String>,
    selection: Option<String>,
    predicates: Vec<Expr>,
    relations: BTreeSet<String>,
    select_distinct: bool,
    order_by: Vec<(String, bool)>,
    limit: Option<u64>,
    has_aggregate: bool,
}

impl ParsedQueryShape {
    fn parse(sql: &str) -> Option<Self> {
        let statements = Parser::parse_sql(&GenericDialect {}, sql).ok()?;
        if statements.len() != 1 {
            return None;
        }
        let Statement::Query(query) = statements.into_iter().next()? else {
            return None;
        };
        Self::from_query(&query)
    }

    fn from_query(query: &Query) -> Option<Self> {
        let SetExpr::Select(select) = query.body.as_ref() else {
            return None;
        };
        let projection = select
            .projection
            .iter()
            .filter_map(|item| match item {
                SelectItem::UnnamedExpr(expression) => Some(expression.to_string()),
                SelectItem::ExprWithAlias { expr, alias } => Some(format!("{expr} AS {alias}")),
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => None,
            })
            .collect::<Vec<_>>();
        let projection_exprs = select
            .projection
            .iter()
            .filter_map(|item| match item {
                SelectItem::UnnamedExpr(expression)
                | SelectItem::ExprWithAlias {
                    expr: expression, ..
                } => Some(expression.clone()),
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => None,
            })
            .collect::<Vec<_>>();
        let projection_aliases = select
            .projection
            .iter()
            .filter_map(|item| match item {
                SelectItem::ExprWithAlias { alias, .. } => Some(canonical_identifier(&alias.value)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let has_aggregate = projection_exprs.iter().any(expr_has_aggregate);
        let mut facts = AstFacts::default();
        let _ = query.visit(&mut facts);
        let order_by = query
            .order_by
            .as_ref()
            .map(|order| {
                order
                    .exprs
                    .iter()
                    .map(|item| (item.expr.to_string(), item.asc != Some(false)))
                    .collect()
            })
            .unwrap_or_default();
        let limit = query
            .limit
            .as_ref()
            .and_then(|value| value.to_string().parse::<u64>().ok());
        Some(Self {
            projection,
            projection_exprs,
            projection_aliases,
            selection: select.selection.as_ref().map(ToString::to_string),
            predicates: select
                .selection
                .iter()
                .chain(select.having.iter())
                .cloned()
                .collect(),
            relations: facts.relations,
            select_distinct: select.distinct.is_some(),
            order_by,
            limit,
            has_aggregate,
        })
    }
}

#[derive(Default)]
struct AstFacts {
    relations: BTreeSet<String>,
}

impl Visitor for AstFacts {
    type Break = ();

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        if let Some(identifier) = relation.0.last() {
            self.relations.insert(identifier.value.to_ascii_lowercase());
        }
        ControlFlow::Continue(())
    }
}

fn parse_sql_expression(value: &str) -> Option<Expr> {
    let statements = Parser::parse_sql(&GenericDialect {}, &format!("SELECT {value}")).ok()?;
    let Statement::Query(query) = statements.first()? else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    if statements.len() != 1 || select.projection.len() != 1 || !select.from.is_empty() {
        return None;
    }
    match &select.projection[0] {
        SelectItem::UnnamedExpr(expression)
        | SelectItem::ExprWithAlias {
            expr: expression, ..
        } => Some(expression.clone()),
        _ => None,
    }
}

fn canonical_identifier(value: &str) -> String {
    value
        .trim_matches(|ch| matches!(ch, '`' | '"'))
        .to_ascii_lowercase()
}

fn identifier_parts(expression: &Expr) -> Option<Vec<String>> {
    match expression {
        Expr::Identifier(identifier) => Some(vec![canonical_identifier(&identifier.value)]),
        Expr::CompoundIdentifier(identifiers) => Some(
            identifiers
                .iter()
                .map(|identifier| canonical_identifier(&identifier.value))
                .collect(),
        ),
        Expr::Nested(expression) => identifier_parts(expression),
        _ => None,
    }
}

fn identifier_matches(expression: &Expr, expected: &str) -> bool {
    let Some(actual) = identifier_parts(expression) else {
        return false;
    };
    let expected = expected
        .split('.')
        .map(canonical_identifier)
        .collect::<Vec<_>>();
    !expected.is_empty()
        && (actual == expected || (expected.len() == 1 && actual.last() == expected.last()))
}

fn expression_equivalent(left: &Expr, right: &Expr) -> bool {
    match (identifier_parts(left), identifier_parts(right)) {
        (Some(left), Some(right)) => left == right || left.last() == right.last(),
        _ => normalized_sql(&left.to_string()) == normalized_sql(&right.to_string()),
    }
}

fn any_expression(expression: &Expr, predicate: &impl Fn(&Expr) -> bool) -> bool {
    if predicate(expression) {
        return true;
    }
    struct Finder<'a, F> {
        predicate: &'a F,
        found: bool,
        root_seen: bool,
    }
    impl<F: Fn(&Expr) -> bool> Visitor for Finder<'_, F> {
        type Break = ();
        fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
            if self.root_seen && (self.predicate)(expression) {
                self.found = true;
                ControlFlow::Break(())
            } else {
                self.root_seen = true;
                ControlFlow::Continue(())
            }
        }
    }
    let mut finder = Finder {
        predicate,
        found: false,
        root_seen: false,
    };
    let _ = expression.visit(&mut finder);
    finder.found
}

fn expr_has_aggregate(expression: &Expr) -> bool {
    any_expression(expression, &|candidate| match candidate {
        Expr::Function(function) => matches!(
            function.name.to_string().to_ascii_lowercase().as_str(),
            "count" | "sum" | "avg" | "min" | "max" | "approx_distinct"
        ),
        _ => false,
    })
}

fn verify_denominator(
    ir: &AnalyticIntentIR,
    contracts: &[&MetricContract],
    shape: Option<&ParsedQueryShape>,
) -> CheckResult {
    let mut expected = Vec::new();
    if let Some(denominator) = ir.denominator.as_ref() {
        expected.push(denominator.expression.clone());
    }
    expected.extend(
        contracts
            .iter()
            .filter_map(|contract| contract.denominator.as_ref())
            .map(metric_expression_sql),
    );
    if expected.is_empty() {
        return pass(
            "denominator_not_required",
            "the intent and metric contracts do not require a denominator",
        );
    }
    let Some(shape) = shape else {
        return fail("denominator_unproven", "SQL shape could not be parsed");
    };
    if expected.iter().all(|denominator| {
        parse_sql_expression(denominator).is_some_and(|expected| {
            shape.projection_exprs.iter().any(|actual| {
                any_expression(actual, &|candidate| {
                    expression_equivalent(candidate, &expected)
                })
            })
        })
    }) {
        pass(
            "denominator_equivalent",
            "every required denominator is present in a selected metric expression",
        )
    } else {
        fail(
            "denominator_unproven",
            "a required metric denominator is missing or semantically different",
        )
    }
}

fn population_subject_is_proven(shape: &ParsedQueryShape, subject: &str) -> bool {
    let subject = subject.trim();
    if subject.eq_ignore_ascii_case("query_rows") {
        return true;
    }
    let subject = canonical_identifier(subject);
    if shape.relations.iter().any(|relation| {
        relation == &subject
            // Natural-language IR commonly stores a singular entity while
            // SQL uses its conventional plural table name (order/orders,
            // user/users). Treat that morphology as equivalent only after
            // the relation has been parsed from the candidate AST.
            || (relation.strip_suffix('s').unwrap_or(relation)
                == subject.strip_suffix('s').unwrap_or(&subject))
    }) {
        return true;
    }
    // Some governed schemas use opaque table names (for example
    // `task_offer`) while the canonical IR still names the business entity
    // (`order`). A count aggregate with an explicitly named subject alias is
    // sufficient additional AST evidence; arbitrary SQL text is not.
    shape.projection_aliases.iter().any(|alias| {
        alias == &subject
            || alias.strip_suffix('s').unwrap_or(alias)
                == subject.strip_suffix('s').unwrap_or(&subject)
            || alias.starts_with(&format!("{subject}_"))
    }) || shape.projection_exprs.iter().any(|expression| {
        expr_has_aggregate(expression)
            && any_expression(expression, &|candidate| {
                identifier_parts(candidate).is_some_and(|parts| {
                    parts.last().is_some_and(|identifier| {
                        identifier == &subject
                            || identifier.strip_suffix('s').unwrap_or(identifier)
                                == subject.strip_suffix('s').unwrap_or(&subject)
                    })
                })
            })
    })
}

fn population_dedup_is_proven(shape: &ParsedQueryShape, key: &str) -> bool {
    if shape.select_distinct
        && shape
            .projection_exprs
            .iter()
            .any(|expression| identifier_matches(expression, key))
    {
        return true;
    }
    shape.projection_exprs.iter().any(|expression| {
        any_expression(expression, &|candidate| {
            let Expr::Function(function) = candidate else {
                return false;
            };
            let name = function.name.to_string().to_ascii_lowercase();
            let FunctionArguments::List(arguments) = &function.args else {
                return false;
            };
            let distinct = name == "approx_distinct"
                || (name == "count"
                    && matches!(
                        arguments.duplicate_treatment,
                        Some(DuplicateTreatment::Distinct)
                    ));
            distinct
                && arguments.args.iter().any(|argument| match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) => {
                        identifier_matches(expression, key)
                    }
                    _ => false,
                })
        })
    })
}

fn population_exclusion_is_proven(shape: &ParsedQueryShape, marker: &str) -> bool {
    shape.predicates.iter().any(|predicate| {
        any_expression(predicate, &|candidate| match candidate {
            Expr::IsFalse(expression) | Expr::IsNotTrue(expression) => {
                identifier_has_semantic_token(expression, marker)
            }
            Expr::BinaryOp { left, op, right } => {
                let excludes = matches!(op, BinaryOperator::NotEq)
                    || (matches!(op, BinaryOperator::Eq) && expression_is_false_or_zero(right));
                excludes
                    && (identifier_has_semantic_token(left, marker)
                        || identifier_has_semantic_token(right, marker))
            }
            Expr::InList { expr, negated, .. } => {
                *negated && identifier_has_semantic_token(expr, marker)
            }
            _ => false,
        })
    })
}

fn identifier_has_semantic_token(expression: &Expr, token: &str) -> bool {
    identifier_parts(expression).is_some_and(|parts| {
        parts.iter().any(|part| {
            part.split('_')
                .any(|component| component.eq_ignore_ascii_case(token))
        })
    })
}

fn expression_is_false_or_zero(expression: &Expr) -> bool {
    match expression {
        Expr::Value(value) => match value {
            sqlparser::ast::Value::Boolean(false) => true,
            sqlparser::ast::Value::Number(number, false) => number == "0",
            _ => false,
        },
        _ => false,
    }
}

fn verify_comparison(ir: &AnalyticIntentIR, sql: &str) -> CheckResult {
    let Some(comparison) = ir.comparison.as_ref() else {
        return pass(
            "comparison_not_required",
            "the intent does not require a comparison",
        );
    };
    let sql = normalized_sql(sql);
    let baseline = normalized_sql(&comparison.baseline);
    let treatment = normalized_sql(&comparison.treatment);
    let method = normalized_sql(&comparison.method);
    let values_proven = !baseline.is_empty()
        && !treatment.is_empty()
        && sql.contains(&baseline)
        && sql.contains(&treatment);
    let method_proven = method.is_empty()
        || sql.contains(&method)
        || (method.contains("difference") && (sql.contains('-') || sql.contains("lag(")))
        || (method.contains("ratio") && sql.contains('/'))
        || (method.contains("percent") && sql.contains('/'));
    if values_proven && method_proven {
        pass(
            "comparison_proven",
            "comparison cohorts/windows and method are represented in SQL",
        )
    } else {
        fail(
            "comparison_unproven",
            "comparison baseline, treatment or method is not proven in SQL",
        )
    }
}

fn verify_null_semantics(ir: &AnalyticIntentIR, shape: Option<&ParsedQueryShape>) -> CheckResult {
    let Some(shape) = shape else {
        return fail("null_semantics_unproven", "SQL shape could not be parsed");
    };
    let rendered = shape
        .projection
        .iter()
        .chain(shape.selection.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    match ir.null_policy {
        NullPolicy::Ignore if shape.has_aggregate || rendered.contains("is not null") => pass(
            "nulls_ignored",
            "aggregate or explicit predicate implements ignore-null semantics",
        ),
        NullPolicy::Ignore => fail(
            "null_ignore_unproven",
            "ignore-null semantics require an aggregate or explicit IS NOT NULL predicate",
        ),
        NullPolicy::Zero
            if rendered.contains("coalesce(")
                || rendered.contains("ifnull(")
                || rendered.contains("nvl(") =>
        {
            pass(
                "nulls_zeroed",
                "NULL values are explicitly converted to zero",
            )
        }
        NullPolicy::SeparateBucket
            if (rendered.contains("coalesce(") || rendered.contains("case"))
                && rendered.contains("null") =>
        {
            pass(
                "null_bucketed",
                "NULL values are represented as a separate result bucket",
            )
        }
        NullPolicy::Fail => CheckResult {
            status: CheckStatus::NotChecked,
            code: "null_fail_requires_result_evidence".into(),
            message: "fail-on-null requires execution/result evidence".into(),
        },
        _ => fail(
            "null_policy_mismatch",
            "the SQL candidate does not implement the requested NULL policy",
        ),
    }
}

fn verify_ordering(ir: &AnalyticIntentIR, shape: Option<&ParsedQueryShape>) -> CheckResult {
    if ir.ordering.is_empty() {
        return pass(
            "ordering_not_required",
            "the intent does not require ordering",
        );
    }
    let Some(shape) = shape else {
        return fail("ordering_unproven", "SQL shape could not be parsed");
    };
    if ir.ordering.len() != shape.order_by.len() {
        return fail(
            "ordering_mismatch",
            "ORDER BY expression count differs from the canonical intent",
        );
    }
    if ir
        .ordering
        .iter()
        .zip(&shape.order_by)
        .all(|(expected, actual)| {
            semantic_identifier_matches(&expected.expression, &actual.0)
                && expected.descending == !actual.1
        })
    {
        pass(
            "ordering_proven",
            "ORDER BY expressions and directions match the canonical intent",
        )
    } else {
        fail(
            "ordering_mismatch",
            "ORDER BY expressions or directions differ from the canonical intent",
        )
    }
}

fn verify_limit(ir: &AnalyticIntentIR, shape: Option<&ParsedQueryShape>) -> CheckResult {
    let Some(expected) = ir.limit else {
        return pass(
            "limit_not_required",
            "the intent does not require a row limit",
        );
    };
    let Some(actual) = shape.and_then(|shape| shape.limit) else {
        return fail(
            "limit_missing",
            "the canonical row limit is missing from SQL",
        );
    };
    if actual <= expected {
        pass(
            "limit_proven",
            "the SQL row limit is no broader than the canonical intent",
        )
    } else {
        fail(
            "limit_broadened",
            "the SQL row limit is broader than the canonical intent",
        )
    }
}

fn semantic_identifier_matches(required: &str, actual: &str) -> bool {
    let Some(required) = parse_sql_expression(required) else {
        return false;
    };
    let Some(actual) = parse_sql_expression(actual) else {
        return false;
    };
    expression_equivalent(&required, &actual)
}

fn inferred_metrics_match(metrics: &[MetricRef], selected_columns: &[String]) -> bool {
    let aggregates = selected_columns
        .iter()
        .filter_map(|expression| parse_sql_expression(expression))
        .filter(expr_has_aggregate)
        .collect::<Vec<_>>();
    if aggregates.len() < metrics.len() {
        return false;
    }
    metrics.iter().all(|metric| {
        let id = metric.id.to_ascii_lowercase();
        let display_name = metric.display_name.to_ascii_lowercase();
        aggregates.iter().any(|expression| {
            let count_shape = any_expression(expression, &|candidate| {
                matches!(candidate, Expr::Function(function) if matches!(
                    function.name.to_string().to_ascii_lowercase().as_str(),
                    "count" | "approx_distinct"
                ))
            });
            let sum_shape = any_expression(expression, &|candidate| {
                matches!(candidate, Expr::Function(function) if function.name.to_string().eq_ignore_ascii_case("sum"))
            });
            let ratio_shape = any_expression(expression, &|candidate| {
                matches!(candidate, Expr::BinaryOp { op: BinaryOperator::Divide, .. })
                    || matches!(candidate, Expr::Function(function) if matches!(
                        function.name.to_string().to_ascii_lowercase().as_str(),
                        "nullif" | "divide"
                    ))
            });
            let id_stem = id.strip_suffix('s').unwrap_or(&id);
            let named = any_expression(expression, &|candidate| {
                identifier_parts(candidate).is_some_and(|parts| {
                    parts.last().is_some_and(|identifier| {
                        identifier == &id
                            || identifier == id_stem
                            || identifier == &display_name
                    })
                })
            });
            match id.as_str() {
                "orders" | "order_count" | "users" | "user_count" => count_shape,
                "revenue" | "gmv" => sum_shape && named,
                "roi" | "roas" => ratio_shape && sum_shape,
                _ => named,
            }
        })
    })
}

fn normalized_sql(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '`' | '"'))
        .flat_map(char::to_lowercase)
        .collect()
}

fn unqualified_sql(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let chars = value.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if chars[index].is_ascii_alphabetic() || chars[index] == '_' {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            if index < chars.len() && chars[index] == '.' {
                index += 1;
                continue;
            }
            output.extend(chars[start..index].iter());
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn metric_expression_sql(expression: &MetricExpressionIR) -> String {
    match expression {
        MetricExpressionIR::Column(column) | MetricExpressionIR::Literal(column) => column.clone(),
        MetricExpressionIR::Aggregate {
            function,
            expression,
            distinct,
        } => format!(
            "{}({}{})",
            function,
            if *distinct { "DISTINCT " } else { "" },
            metric_expression_sql(expression)
        ),
        MetricExpressionIR::Ratio {
            numerator,
            denominator,
        } => format!(
            "({})/({})",
            metric_expression_sql(numerator),
            metric_expression_sql(denominator)
        ),
    }
}

fn metric_expression_matches(contract: &MetricExpressionIR, selected: &str) -> bool {
    let expected = metric_expression_canonical(&metric_expression_sql(contract));
    let actual = metric_expression_canonical(strip_select_alias(selected));
    expected == actual || unqualified_sql(&expected) == unqualified_sql(&actual)
}

fn strip_select_alias(selected: &str) -> &str {
    let lower = selected.to_ascii_lowercase();
    lower
        .rfind(" as ")
        .map_or(selected, |index| &selected[..index])
}

fn metric_expression_canonical(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, '`' | '"' | '(' | ')'))
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn expression_contains_identifier(expression: &Expr, identifier: &str) -> bool {
    any_expression(expression, &|candidate| {
        identifier_matches(candidate, identifier)
    })
}

fn expression_contains_literal(expression: &Expr, expected: &str) -> bool {
    any_expression(expression, &|candidate| match candidate {
        Expr::Value(value) => match value {
            sqlparser::ast::Value::SingleQuotedString(value)
            | sqlparser::ast::Value::DoubleQuotedString(value)
            | sqlparser::ast::Value::Number(value, _) => value == expected,
            sqlparser::ast::Value::Boolean(value) => {
                value.to_string().eq_ignore_ascii_case(expected)
            }
            _ => false,
        },
        Expr::TypedString { value, .. } => value == expected,
        _ => false,
    })
}

fn comparison_proven(
    shape: &ParsedQueryShape,
    column: &str,
    literal: &str,
    accepted: &[BinaryOperator],
) -> bool {
    shape.predicates.iter().any(|predicate| {
        any_expression(predicate, &|candidate| {
            let Expr::BinaryOp { left, op, right } = candidate else {
                return false;
            };
            accepted.contains(op)
                && ((expression_contains_identifier(left, column)
                    && expression_contains_literal(right, literal))
                    || (expression_contains_identifier(right, column)
                        && expression_contains_literal(left, literal)))
        })
    })
}

fn sql_matches_time_semantics(shape: &ParsedQueryShape, time: &TimeSemantics) -> bool {
    let equality = comparison_proven(
        shape,
        &time.column,
        &time.start_inclusive,
        &[BinaryOperator::Eq],
    );
    if equality {
        return chrono::NaiveDate::parse_from_str(&time.start_inclusive, "%Y-%m-%d")
            .ok()
            .zip(chrono::NaiveDate::parse_from_str(&time.end_exclusive, "%Y-%m-%d").ok())
            .is_some_and(|(start, end)| end.signed_duration_since(start).num_days() == 1);
    }
    comparison_proven(
        shape,
        &time.column,
        &time.start_inclusive,
        &[BinaryOperator::GtEq],
    ) && comparison_proven(
        shape,
        &time.column,
        &time.end_exclusive,
        &[BinaryOperator::Lt],
    )
}

fn sql_matches_timezone_semantics(shape: &ParsedQueryShape, time: &TimeSemantics) -> bool {
    let column = time.column.to_ascii_lowercase();
    if ["date", "day", "dt"]
        .iter()
        .any(|suffix| column == *suffix || column.ends_with(&format!("_{suffix}")))
    {
        return true;
    }
    let timezone = normalized_sql(&time.timezone);
    if timezone.is_empty() {
        return false;
    }
    shape.predicates.iter().any(|predicate| {
        any_expression(predicate, &|candidate| match candidate {
            Expr::AtTimeZone {
                timestamp,
                time_zone,
            } => {
                expression_contains_identifier(timestamp, &time.column)
                    && expression_contains_literal(time_zone, &time.timezone)
            }
            Expr::Function(function)
                if matches!(
                    function.name.to_string().to_ascii_lowercase().as_str(),
                    "convert_tz" | "timezone" | "from_utc_timestamp" | "date_format"
                ) =>
            {
                expression_contains_identifier(candidate, &time.column)
                    && expression_contains_literal(candidate, &time.timezone)
            }
            _ => false,
        })
    })
}

fn shape_contains_filter(shape: &ParsedQueryShape, filter: &SemanticFilter) -> bool {
    let predicates = shape
        .predicates
        .iter()
        .flat_map(|predicate| {
            let mut nodes = Vec::new();
            collect_expression_clones(predicate, &mut nodes);
            nodes
        })
        .collect::<Vec<_>>();
    match filter {
        SemanticFilter::Equals { field, value } => predicates.iter().any(|predicate| {
            matches!(predicate, Expr::BinaryOp { left, op: BinaryOperator::Eq, right }
                if (identifier_matches(&left, field) && expression_contains_literal(&right, value))
                    || (identifier_matches(&right, field) && expression_contains_literal(&left, value)))
        }),
        SemanticFilter::In { field, values } => predicates.iter().any(|predicate| {
            matches!(predicate, Expr::InList { expr, list, negated: false }
                if identifier_matches(&expr, field)
                    && values.iter().all(|value| list.iter().any(|item| expression_contains_literal(item, value))))
        }),
        SemanticFilter::Compare {
            field,
            operator,
            value,
        } => parse_binary_operator(operator).is_some_and(|expected_operator| {
            predicates.iter().any(|predicate| {
                matches!(predicate, Expr::BinaryOp { left, op, right }
                    if *op == expected_operator
                        && identifier_matches(&left, field)
                        && expression_contains_literal(&right, value))
            })
        }),
        SemanticFilter::RawBounded { expression } => parse_sql_expression(expression).is_some_and(
            |expected| {
                predicates
                    .iter()
                    .any(|actual| expression_equivalent(&actual, &expected))
            },
        ),
    }
}

fn parse_binary_operator(operator: &str) -> Option<BinaryOperator> {
    match operator.trim() {
        ">" => Some(BinaryOperator::Gt),
        ">=" => Some(BinaryOperator::GtEq),
        "<" => Some(BinaryOperator::Lt),
        "<=" => Some(BinaryOperator::LtEq),
        "!=" | "<>" => Some(BinaryOperator::NotEq),
        _ => None,
    }
}

fn collect_expression_clones(expression: &Expr, output: &mut Vec<Expr>) {
    output.push(expression.clone());
    struct Collector<'a> {
        output: &'a mut Vec<Expr>,
        root_seen: bool,
    }
    impl Visitor for Collector<'_> {
        type Break = ();
        fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
            if self.root_seen {
                self.output.push(expression.clone());
            } else {
                self.root_seen = true;
            }
            ControlFlow::Continue(())
        }
    }
    let mut collector = Collector {
        output,
        root_seen: false,
    };
    let _ = expression.visit(&mut collector);
}
fn pass(code: &str, message: &str) -> CheckResult {
    CheckResult {
        status: CheckStatus::Pass,
        code: code.into(),
        message: message.into(),
    }
}
fn warn(code: &str, message: &str) -> CheckResult {
    CheckResult {
        status: CheckStatus::Warn,
        code: code.into(),
        message: message.into(),
    }
}
fn fail(code: &str, message: &str) -> CheckResult {
    CheckResult {
        status: CheckStatus::Fail,
        code: code.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ir() -> AnalyticIntentIR {
        AnalyticIntentIR {
            objective: AnalyticObjective::Aggregate,
            metrics: vec![MetricRef {
                id: "orders".into(),
                version: Some(1),
                display_name: "订单数".into(),
            }],
            dimensions: vec![DimensionRef {
                name: "day".into(),
                column: "business_date".into(),
            }],
            grain: Grain::Day,
            population: PopulationDefinition {
                subject: "order".into(),
                dedup_key: Some("order_id".into()),
                exclude_test_users: false,
                exclude_internal_users: false,
                valid_record_rule: None,
            },
            filters: vec![],
            time: Some(TimeSemantics {
                column: "created_at".into(),
                timezone: "Asia/Shanghai".into(),
                start_inclusive: "2026-01-01".into(),
                end_exclusive: "2026-01-02".into(),
                business_calendar: None,
                as_of: None,
            }),
            comparison: None,
            denominator: None,
            ordering: vec![],
            limit: None,
            null_policy: NullPolicy::Ignore,
            data_quality_policy: DataQualityPolicy::BestEffort,
            security_scope: SecurityScopeRef {
                tenant_id: "t".into(),
                datasource_id: "d".into(),
                scope_hash: "h".into(),
            },
            unresolved: vec![],
        }
    }
    #[test]
    fn verifier_rejects_fanout_and_mismatched_grain() {
        let mut input = ir();
        let result = SemanticVerifier::default().verify(
            &input,
            &["business_date".into()],
            &[],
            &[JoinContract {
                id: "j".into(),
                left_table: "a".into(),
                right_table: "b".into(),
                left_keys: vec!["id".into()],
                right_keys: vec!["id".into()],
                cardinality: JoinCardinality::ManyToMany,
                temporal_condition: None,
                nullable: false,
                dedup_strategy: None,
                allowed_grains: vec![],
                fanout_risk: true,
            }],
            "SELECT business_date FROM a",
        );
        assert_eq!(result.release_decision, QueryReleaseDecision::Reject);
        input.unresolved.push(SemanticAmbiguity {
            field: "metric".into(),
            candidates: vec!["x".into()],
            impact: "high".into(),
        });
        let result = SemanticVerifier::default().verify(
            &input,
            &["business_date".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date FROM a",
        );
        assert_eq!(
            result.release_decision,
            QueryReleaseDecision::NeedsClarification
        );
    }
    #[test]
    fn confidence_is_not_a_fixed_constant() {
        let result = SemanticVerifier::default().verify(
            &ir(),
            &["business_date".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date FROM a",
        );
        assert!(
            result.confidence_basis.calibrated_score > 0.0
                && result.confidence_basis.calibrated_score < 1.0
        );
    }

    #[test]
    fn verifier_accepts_read_only_cte_and_checks_metric_expression() {
        let mut input = ir();
        input.metrics[0].id = "orders".into();
        let contract = MetricContract {
            id: "orders".into(),
            version: 1,
            names: vec!["订单数".into()],
            expression: MetricExpressionIR::Aggregate {
                function: "COUNT".into(),
                expression: Box::new(MetricExpressionIR::Column("order_id".into())),
                distinct: true,
            },
            denominator: None,
            population: input.population.clone(),
            default_grain: Grain::Day,
            allowed_grains: vec![Grain::Day],
            time_column: "created_at".into(),
            timezone: "UTC".into(),
            mandatory_filters: vec![],
            join_contracts: vec![],
            invariants: vec![],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: None,
            evidence_refs: vec![],
        };
        let result = SemanticVerifier::default().verify_with_metric_contracts(
            &input,
            &["business_date".into(), "COUNT(DISTINCT o.order_id)".into()],
            &["business_date".into()],
            &[],
            &[contract],
            "WITH orders AS (SELECT order_id, business_date FROM fact) SELECT business_date, COUNT(DISTINCT o.order_id) FROM orders o GROUP BY business_date",
        );
        assert_eq!(result.safety.status, CheckStatus::Pass);
        assert_eq!(result.metric_equivalence.status, CheckStatus::Pass);
    }

    #[test]
    fn verifier_requires_a_bounded_filter_for_requested_time_semantics() {
        let input = ir();
        let unbounded = SemanticVerifier::default().verify(
            &input,
            &["business_date".into(), "COUNT(*) AS orders".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date, COUNT(*) AS orders FROM fact WHERE created_at IS NOT NULL GROUP BY business_date",
        );
        assert_eq!(unbounded.time_consistency.status, CheckStatus::Fail);
        assert_ne!(unbounded.release_decision, QueryReleaseDecision::Release);

        let wrong_window = SemanticVerifier::default().verify(
            &input,
            &["business_date".into(), "COUNT(*) AS orders".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date, COUNT(*) AS orders FROM fact WHERE created_at >= DATE '2026-02-01' AND created_at < DATE '2026-02-02' GROUP BY business_date",
        );
        assert_eq!(wrong_window.time_consistency.status, CheckStatus::Fail);
        assert_ne!(wrong_window.release_decision, QueryReleaseDecision::Release);

        let bounded = SemanticVerifier::default().verify(
            &input,
            &["business_date".into(), "COUNT(*) AS orders".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date, COUNT(*) AS orders FROM fact WHERE (created_at AT TIME ZONE 'Asia/Shanghai') >= DATE '2026-01-01' AND (created_at AT TIME ZONE 'Asia/Shanghai') < DATE '2026-01-02' GROUP BY business_date",
        );
        assert_eq!(bounded.time_consistency.status, CheckStatus::Pass);
    }

    #[test]
    fn verifier_fails_closed_for_unproven_denominator_population_and_policy() {
        let mut input = ir();
        input.time = None;
        input.denominator = Some(DenominatorSpec {
            expression: "SUM(cost)".into(),
            population: input.population.clone(),
        });
        input.population.exclude_test_users = true;
        input.security_scope.scope_hash.clear();
        let result = SemanticVerifier::default().verify(
            &input,
            &["business_date".into(), "SUM(revenue) AS revenue".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date, SUM(revenue) AS revenue FROM orders GROUP BY business_date",
        );
        assert_eq!(result.denominator_equivalence.status, CheckStatus::Fail);
        assert_eq!(result.population_equivalence.status, CheckStatus::Fail);
        assert_eq!(result.policy_compliance.status, CheckStatus::Fail);
        assert_eq!(result.release_decision, QueryReleaseDecision::Reject);
    }

    #[test]
    fn verifier_proves_comparison_null_order_and_limit_from_the_parsed_query() {
        let mut input = ir();
        input.time = None;
        input.objective = AnalyticObjective::Comparison;
        input.comparison = Some(ComparisonSpec {
            baseline: "control".into(),
            treatment: "treatment".into(),
            method: "difference".into(),
        });
        input.ordering = vec![OrderSpec {
            expression: "delta".into(),
            descending: true,
        }];
        input.limit = Some(20);
        input.null_policy = NullPolicy::Zero;
        let result = SemanticVerifier::default().verify(
            &input,
            &[
                "business_date".into(),
                "COALESCE(SUM(CASE WHEN cohort = 'treatment' THEN revenue ELSE 0 END), 0) - COALESCE(SUM(CASE WHEN cohort = 'control' THEN revenue ELSE 0 END), 0) AS delta".into(),
            ],
            &["business_date".into()],
            &[],
            "SELECT business_date, COALESCE(SUM(CASE WHEN cohort = 'treatment' THEN revenue ELSE 0 END), 0) - COALESCE(SUM(CASE WHEN cohort = 'control' THEN revenue ELSE 0 END), 0) AS delta FROM orders GROUP BY business_date ORDER BY delta DESC LIMIT 20",
        );
        assert_eq!(result.comparison_consistency.status, CheckStatus::Pass);
        assert_eq!(result.null_semantics.status, CheckStatus::Pass);
        assert_eq!(result.ordering_consistency.status, CheckStatus::Pass);
        assert_eq!(result.limit_consistency.status, CheckStatus::Pass);
    }

    #[test]
    fn verifier_never_releases_unchecked_result_invariants() {
        let mut input = ir();
        input.time = None;
        let contract = MetricContract {
            id: "orders".into(),
            version: 1,
            names: vec!["orders".into()],
            expression: MetricExpressionIR::Aggregate {
                function: "COUNT".into(),
                expression: Box::new(MetricExpressionIR::Literal("*".into())),
                distinct: false,
            },
            denominator: None,
            population: input.population.clone(),
            default_grain: Grain::Day,
            allowed_grains: vec![Grain::Day],
            time_column: "business_date".into(),
            timezone: "Asia/Shanghai".into(),
            mandatory_filters: vec![],
            join_contracts: vec![],
            invariants: vec![ResultInvariant::NonNegative {
                field: "orders".into(),
            }],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: None,
            evidence_refs: vec![],
        };
        let result = SemanticVerifier::default().verify_with_metric_contracts(
            &input,
            &["business_date".into(), "COUNT(*) AS orders".into()],
            &["business_date".into()],
            &[],
            &[contract],
            "SELECT business_date, COUNT(*) AS orders FROM orders GROUP BY business_date",
        );
        assert_eq!(result.result_invariants[0].status, CheckStatus::NotChecked);
        assert_ne!(result.release_decision, QueryReleaseDecision::Release);
    }

    #[test]
    fn result_invariants_fail_closed_for_missing_empty_and_invalid_values() {
        let invariants = vec![
            ResultInvariant::NonNegative {
                field: "orders".into(),
            },
            ResultInvariant::RatioBounded {
                field: "roi".into(),
                lower: 0,
                upper: 10,
            },
            ResultInvariant::SumMatches {
                total_field: "total".into(),
                parts_field: "parts".into(),
            },
        ];
        let empty = evaluate_result_invariants(&invariants, &[]);
        assert!(empty
            .iter()
            .all(|observation| observation.status == CheckStatus::NotChecked));

        let valid = evaluate_result_invariants(
            &invariants,
            &[serde_json::json!({
                "orders": 4,
                "roi": 1.25,
                "total": 6,
                "parts": [1, 2, 3]
            })],
        );
        assert!(valid
            .iter()
            .all(|observation| observation.status == CheckStatus::Pass));

        let invalid = evaluate_result_invariants(
            &invariants,
            &[serde_json::json!({
                "orders": -1,
                "roi": 12,
                "total": 6,
                "parts": [1, 2]
            })],
        );
        assert!(invalid
            .iter()
            .all(|observation| observation.status == CheckStatus::Fail));

        let missing = evaluate_result_invariants(&invariants, &[serde_json::json!({"orders": 1})]);
        assert_eq!(missing[0].status, CheckStatus::Pass);
        assert_eq!(missing[1].status, CheckStatus::NotChecked);
        assert_eq!(missing[2].status, CheckStatus::NotChecked);
    }

    #[test]
    fn time_verifier_preserves_half_open_semantics_and_allows_one_day_equality() {
        let one_day = TimeSemantics {
            column: "business_date".into(),
            timezone: "Asia/Shanghai".into(),
            start_inclusive: "2026-08-15".into(),
            end_exclusive: "2026-08-16".into(),
            business_calendar: None,
            as_of: None,
        };
        assert!(sql_matches_time_semantics(
            &ParsedQueryShape::parse(
                "SELECT COUNT(*) FROM orders WHERE business_date = DATE '2026-08-15'"
            )
            .unwrap(),
            &one_day
        ));
        assert!(sql_matches_time_semantics(
            &ParsedQueryShape::parse("SELECT COUNT(*) FROM orders WHERE business_date >= DATE '2026-08-15' AND business_date < DATE '2026-08-16'").unwrap(),
            &one_day
        ));
        assert!(!sql_matches_time_semantics(
            &ParsedQueryShape::parse("SELECT COUNT(*) FROM orders WHERE business_date >= DATE '2026-08-15' AND business_date <= DATE '2026-08-16'").unwrap(),
            &one_day
        ));

        let multi_day = TimeSemantics {
            end_exclusive: "2026-08-22".into(),
            ..one_day
        };
        assert!(!sql_matches_time_semantics(
            &ParsedQueryShape::parse(
                "SELECT COUNT(*) FROM orders WHERE business_date = DATE '2026-08-15'"
            )
            .unwrap(),
            &multi_day
        ));
    }

    #[test]
    fn ast_verifier_rejects_substring_and_comment_spoofing() {
        let mut input = ir();
        input.time = None;
        input.population.subject = "orders".into();
        input.population.dedup_key = Some("user_id".into());
        input.population.exclude_test_users = true;
        input.filters = vec![SemanticFilter::Equals {
            field: "country".into(),
            value: "US".into(),
        }];
        let result = SemanticVerifier::default().verify(
            &input,
            &["business_date".into(), "COUNT(DISTINCT fake_user_id)".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date, COUNT(DISTINCT fake_user_id) FROM preorders WHERE contest = 'US' AND latest = 0 /* orders user_id country test */ GROUP BY business_date",
        );
        assert_eq!(result.population_equivalence.status, CheckStatus::Fail);
        assert_eq!(result.filter_completeness.status, CheckStatus::Warn);
        assert_ne!(result.release_decision, QueryReleaseDecision::Release);
    }

    #[test]
    fn verifier_rejects_unverified_join_but_does_not_treat_unrelated_contract_as_a_join() {
        let input = ir();
        let unrelated = JoinContract {
            id: "unrelated".into(),
            left_table: "other_a".into(),
            right_table: "other_b".into(),
            left_keys: vec!["id".into()],
            right_keys: vec!["id".into()],
            cardinality: JoinCardinality::ManyToMany,
            temporal_condition: None,
            nullable: false,
            dedup_strategy: None,
            allowed_grains: vec![],
            fanout_risk: true,
        };
        let safe = SemanticVerifier::default().verify_with_metric_contracts(
            &input,
            &["business_date".into()],
            &["business_date".into()],
            &[],
            &[],
            "SELECT business_date FROM fact",
        );
        assert_ne!(safe.join_cardinality.status, CheckStatus::Fail);
        let unsafe_join = SemanticVerifier::default().verify_with_metric_contracts(
            &input,
            &["business_date".into()],
            &["business_date".into()],
            &[unrelated],
            &[],
            "SELECT business_date FROM fact JOIN other_a ON fact.id = other_a.id",
        );
        assert_eq!(unsafe_join.join_cardinality.status, CheckStatus::Fail);
    }

    #[test]
    fn calibration_metrics_are_bounded_and_deterministic() {
        let observations = vec![(0.9, true), (0.8, true), (0.2, false), (0.1, false)];
        let first = calibration_metrics(&observations, 10);
        assert_eq!(first, calibration_metrics(&observations, 10));
        assert!((0.0..=1.0).contains(&first.0));
        assert!((0.0..=1.0).contains(&first.1));
        assert_eq!(calibration_metrics(&[], 10), (0.0, 0.0));
    }

    #[test]
    fn metric_contract_expression_parser_preserves_certified_semantics() {
        assert_eq!(
            parse_metric_expression_ir("COUNT(DISTINCT order_id)").unwrap(),
            MetricExpressionIR::Aggregate {
                function: "COUNT".into(),
                expression: Box::new(MetricExpressionIR::Column("order_id".into())),
                distinct: true,
            }
        );
        assert!(matches!(
            parse_metric_expression_ir("SUM(revenue) / NULLIF(SUM(cost), 0)").unwrap(),
            MetricExpressionIR::Ratio { .. }
        ));
        assert!(parse_metric_expression_ir("SUM(revenue); DELETE FROM facts").is_err());
    }

    #[test]
    fn attribution_levels_block_causal_overclaim_without_identification() {
        let descriptive = CausalAnalysisContract {
            evidence_level: EvidenceLevel::L1Decomposition,
            treatment: "x".into(),
            outcome: "y".into(),
            unit: "user".into(),
            pre_window: "before".into(),
            post_window: "after".into(),
            control: None,
            confounders: vec![],
            interference_assumptions: vec![],
            missingness_policy: "report".into(),
            estimator: "".into(),
            uncertainty_interval: "".into(),
            robustness_checks: vec![],
        };
        assert!(!descriptive.permits_causal_language());
        let quasi = CausalAnalysisContract {
            evidence_level: EvidenceLevel::L2QuasiExperimental,
            estimator: "DiD".into(),
            uncertainty_interval: "95% CI".into(),
            ..descriptive
        };
        assert!(quasi.permits_causal_language());
    }
}
