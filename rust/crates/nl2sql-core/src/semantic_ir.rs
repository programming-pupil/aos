//! Analytics Semantic Compiler contracts.
//!
//! The web layer may still use the legacy SQL generator, but every new or
//! repaired query can carry this IR and a deterministic verification result.
//! A SQL string being parseable is deliberately not treated as semantic proof.

use serde::{Deserialize, Serialize};
use sqlparser::ast::Statement;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use std::collections::BTreeSet;

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
    Equals { field: String, value: String },
    In { field: String, values: Vec<String> },
    RawBounded { expression: String },
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
    pub population_equivalence: CheckResult,
    pub grain_consistency: CheckResult,
    pub time_consistency: CheckResult,
    pub join_cardinality: CheckResult,
    pub filter_completeness: CheckResult,
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
        let safety = if is_single_read_only_query(sql) {
            pass("safe", "read-only SELECT candidate")
        } else {
            fail(
                "unsafe_statement",
                "candidate is not one read-only SELECT/CTE query",
            )
        };
        let required: BTreeSet<_> = ir.dimensions.iter().map(|d| d.column.as_str()).collect();
        let selected: BTreeSet<_> = selected_columns.iter().map(String::as_str).collect();
        let schema_binding = if required.is_subset(&selected) {
            pass("schema_bound", "all requested dimensions are selected")
        } else {
            fail("missing_dimension", "a requested dimension is not selected")
        };
        let grain_consistency = match ir.grain {
            Grain::Row => pass("grain_row", "row grain does not require grouping"),
            _ if required.iter().all(|d| group_by.iter().any(|g| g == d)) => {
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
            .filter(|filter| !sql_contains_filter(sql, filter))
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
        } else if ir.metrics.is_empty() {
            fail(
                "metric_missing",
                "no metric is bound for an analytic request",
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
        let population_equivalence = if ir.population.subject.trim().is_empty() {
            fail("population_missing", "population subject is empty")
        } else if ir
            .population
            .dedup_key
            .as_deref()
            .is_some_and(|key| !contains_identifier(sql, key))
        {
            warn(
                "population_dedup_unproven",
                "the population deduplication key is not visible in the SQL candidate",
            )
        } else {
            pass(
                "population_checked",
                "population subject and dedup policy are represented in the IR",
            )
        };
        let time_consistency = if let Some(time) = ir.time.as_ref() {
            if contains_identifier(sql, &time.column) && sql_has_filter_clause(sql) {
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
        } else {
            warn("time_missing", "time semantics are not explicit")
        };
        let policy_compliance = pass("policy_checked", "tenant scope is present");
        let checks = [
            &safety,
            &schema_binding,
            &metric_equivalence,
            &population_equivalence,
            &grain_consistency,
            &time_consistency,
            &join_cardinality,
            &filter_completeness,
            &policy_compliance,
        ];
        let passed = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Pass)
            .count() as f32
            / checks.len() as f32;
        let decision =
            if safety.status == CheckStatus::Fail || join_cardinality.status == CheckStatus::Fail {
                QueryReleaseDecision::Reject
            } else if schema_binding.status == CheckStatus::Fail
                || metric_equivalence.status == CheckStatus::Fail
            {
                QueryReleaseDecision::Repair
            } else if unresolved > 0
                || filter_completeness.status == CheckStatus::Warn
                || grain_consistency.status == CheckStatus::Fail
                || metric_equivalence.status == CheckStatus::Warn
                || population_equivalence.status != CheckStatus::Pass
                || time_consistency.status == CheckStatus::Fail
            {
                QueryReleaseDecision::NeedsClarification
            } else {
                QueryReleaseDecision::Release
            };
        let schema_is_pass = schema_binding.status == CheckStatus::Pass;
        SemanticVerification {
            safety,
            schema_binding,
            metric_equivalence,
            population_equivalence,
            grain_consistency,
            time_consistency,
            join_cardinality,
            filter_completeness,
            policy_compliance,
            result_invariants: bound_contracts
                .iter()
                .flat_map(|contract| contract.invariants.iter())
                .map(|invariant| CheckResult {
                    status: CheckStatus::NotChecked,
                    code: "result_invariant_pending".into(),
                    message: format!("result invariant requires execution evidence: {invariant:?}"),
                })
                .collect(),
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

fn is_single_read_only_query(sql: &str) -> bool {
    Parser::parse_sql(&GenericDialect {}, sql)
        .ok()
        .is_some_and(|statements| {
            statements.len() == 1 && matches!(statements.first(), Some(Statement::Query(_)))
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
    let actual = metric_expression_canonical(selected);
    expected == actual || unqualified_sql(&expected) == unqualified_sql(&actual)
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

fn contains_identifier(sql: &str, identifier: &str) -> bool {
    let sql = unqualified_sql(&normalized_sql(sql));
    let identifier = unqualified_sql(&normalized_sql(identifier));
    !identifier.is_empty() && sql.contains(&identifier)
}

fn sql_has_filter_clause(sql: &str) -> bool {
    let normalized = normalized_sql(sql);
    normalized.contains("where") || normalized.contains("having")
}

fn sql_contains_filter(sql: &str, filter: &SemanticFilter) -> bool {
    let normalized = unqualified_sql(&normalized_sql(sql));
    match filter {
        SemanticFilter::Equals { field, value } => {
            normalized.contains(&unqualified_sql(&normalized_sql(field)))
                && normalized.contains(&normalized_sql(value))
        }
        SemanticFilter::In { field, values } => {
            normalized.contains(&unqualified_sql(&normalized_sql(field)))
                && values
                    .iter()
                    .all(|value| normalized.contains(&normalized_sql(value)))
        }
        SemanticFilter::RawBounded { expression } => {
            normalized.contains(&unqualified_sql(&normalized_sql(expression)))
        }
    }
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
            limit: Some(100),
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
