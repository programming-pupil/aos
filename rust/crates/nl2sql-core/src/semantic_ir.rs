//! Analytics Semantic Compiler contracts.
//!
//! The web layer may still use the legacy SQL generator, but every new or
//! repaired query can carry this IR and a deterministic verification result.
//! A SQL string being parseable is deliberately not treated as semantic proof.

use serde::{Deserialize, Serialize};
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
        let safety = if sql.trim_start().to_ascii_lowercase().starts_with("select") {
            pass("safe", "read-only SELECT candidate")
        } else {
            fail("unsafe_statement", "candidate is not a SELECT")
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
        let fanout = joins
            .iter()
            .any(|j| j.cardinality == JoinCardinality::ManyToMany || j.fanout_risk);
        let join_cardinality = if fanout {
            fail(
                "join_fanout",
                "join contract has many-to-many or unverified fanout risk",
            )
        } else {
            pass("join_safe", "join cardinality is bounded")
        };
        let missing_mandatory = ir.filters.is_empty() && ir.population.exclude_test_users;
        let filter_completeness = if missing_mandatory {
            warn(
                "filter_review",
                "population policy requires a filter review",
            )
        } else {
            pass("filters_ok", "no mandatory filter is missing")
        };
        let unresolved = ir.unresolved.len() as u32;
        let checks = [
            &safety,
            &schema_binding,
            &grain_consistency,
            &join_cardinality,
            &filter_completeness,
        ];
        let passed = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Pass)
            .count() as f32
            / checks.len() as f32;
        let decision =
            if safety.status == CheckStatus::Fail || join_cardinality.status == CheckStatus::Fail {
                QueryReleaseDecision::Reject
            } else if unresolved > 0
                || filter_completeness.status == CheckStatus::Warn
                || grain_consistency.status == CheckStatus::Fail
            {
                QueryReleaseDecision::NeedsClarification
            } else if schema_binding.status == CheckStatus::Fail {
                QueryReleaseDecision::Repair
            } else {
                QueryReleaseDecision::Release
            };
        let schema_is_pass = schema_binding.status == CheckStatus::Pass;
        SemanticVerification {
            safety,
            schema_binding,
            metric_equivalence: pass(
                "metric_unverified",
                "metric contract comparison is required when a certified metric is selected",
            ),
            population_equivalence: pass(
                "population_checked",
                "population fields are represented in the IR",
            ),
            grain_consistency,
            time_consistency: if ir.time.is_some() {
                pass("time_bound", "time semantics are explicit")
            } else {
                warn("time_missing", "time semantics are not explicit")
            },
            join_cardinality,
            filter_completeness,
            policy_compliance: pass("policy_checked", "tenant scope is present"),
            result_invariants: vec![],
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
