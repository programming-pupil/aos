//! Analytics Semantic Compiler contracts.
//!
//! Provider-backed SQL generation is downstream of this IR. Every new or
//! repaired query must carry the same immutable intent into deterministic
//! verification; a parseable SQL string is never semantic proof by itself.

use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    BinaryOperator, DuplicateTreatment, Expr, FunctionArg, FunctionArgExpr, FunctionArguments,
    GroupByExpr, JoinConstraint, JoinOperator, Query, Select, SelectItem, SetExpr, Statement,
    TableFactor, TableWithJoins, UnaryOperator, Value,
};
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
pub struct ComparisonWindow {
    pub column: String,
    pub start_inclusive: String,
    pub end_exclusive: String,
    pub timezone: String,
    pub business_calendar: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComparisonSpec {
    pub baseline: String,
    pub treatment: String,
    pub method: String,
    #[serde(default)]
    pub baseline_window: Option<ComparisonWindow>,
    #[serde(default)]
    pub treatment_window: Option<ComparisonWindow>,
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NormalizedExpression {
    Column {
        relation: Option<String>,
        name: String,
    },
    Literal {
        value: String,
    },
    Function {
        name: String,
        arguments: Vec<NormalizedExpression>,
        distinct: bool,
        window: Option<String>,
    },
    Binary {
        operator: String,
        left: Box<NormalizedExpression>,
        right: Box<NormalizedExpression>,
    },
    Unary {
        operator: String,
        expression: Box<NormalizedExpression>,
    },
    IsNull {
        expression: Box<NormalizedExpression>,
        negated: bool,
    },
    InList {
        expression: Box<NormalizedExpression>,
        values: Vec<NormalizedExpression>,
        negated: bool,
    },
    Between {
        expression: Box<NormalizedExpression>,
        low: Box<NormalizedExpression>,
        high: Box<NormalizedExpression>,
        negated: bool,
    },
    Cast {
        expression: Box<NormalizedExpression>,
        data_type: String,
    },
    AtTimeZone {
        timestamp: Box<NormalizedExpression>,
        timezone: Box<NormalizedExpression>,
    },
    Case {
        operand: Option<Box<NormalizedExpression>>,
        branches: Vec<(NormalizedExpression, NormalizedExpression)>,
        else_expression: Option<Box<NormalizedExpression>>,
    },
    Unsupported {
        sql: String,
    },
}

impl NormalizedExpression {
    fn column_name(&self) -> Option<&str> {
        match self {
            Self::Column { name, .. } => Some(name),
            _ => None,
        }
    }

    fn is_supported(&self) -> bool {
        match self {
            Self::Column { .. } | Self::Literal { .. } => true,
            Self::Function { arguments, .. } => arguments.iter().all(Self::is_supported),
            Self::Binary { left, right, .. } => left.is_supported() && right.is_supported(),
            Self::Unary { expression, .. }
            | Self::IsNull { expression, .. }
            | Self::Cast { expression, .. } => expression.is_supported(),
            Self::InList {
                expression, values, ..
            } => expression.is_supported() && values.iter().all(Self::is_supported),
            Self::Between {
                expression,
                low,
                high,
                ..
            } => expression.is_supported() && low.is_supported() && high.is_supported(),
            Self::AtTimeZone {
                timestamp,
                timezone,
            } => timestamp.is_supported() && timezone.is_supported(),
            Self::Case {
                operand,
                branches,
                else_expression,
            } => {
                operand.as_deref().is_none_or(Self::is_supported)
                    && branches.iter().all(|(condition, result)| {
                        condition.is_supported() && result.is_supported()
                    })
                    && else_expression.as_deref().is_none_or(Self::is_supported)
            }
            Self::Unsupported { .. } => false,
        }
    }

    fn contains_exact(&self, expected: &Self) -> bool {
        if normalized_expression_equivalent(self, expected) {
            return true;
        }
        match self {
            Self::Function { arguments, .. } => {
                arguments.iter().any(|value| value.contains_exact(expected))
            }
            Self::Binary { left, right, .. } => {
                left.contains_exact(expected) || right.contains_exact(expected)
            }
            Self::Unary { expression, .. }
            | Self::IsNull { expression, .. }
            | Self::Cast { expression, .. } => expression.contains_exact(expected),
            Self::InList {
                expression, values, ..
            } => {
                expression.contains_exact(expected)
                    || values.iter().any(|value| value.contains_exact(expected))
            }
            Self::Between {
                expression,
                low,
                high,
                ..
            } => {
                expression.contains_exact(expected)
                    || low.contains_exact(expected)
                    || high.contains_exact(expected)
            }
            Self::AtTimeZone {
                timestamp,
                timezone,
            } => timestamp.contains_exact(expected) || timezone.contains_exact(expected),
            Self::Case {
                operand,
                branches,
                else_expression,
            } => {
                operand
                    .as_deref()
                    .is_some_and(|value| value.contains_exact(expected))
                    || branches.iter().any(|(condition, result)| {
                        condition.contains_exact(expected) || result.contains_exact(expected)
                    })
                    || else_expression
                        .as_deref()
                        .is_some_and(|value| value.contains_exact(expected))
            }
            Self::Column { .. } | Self::Literal { .. } | Self::Unsupported { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationBinding {
    pub relation: String,
    pub alias: Option<String>,
    pub derived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectionBinding {
    pub expression: NormalizedExpression,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RelationalPlanNode {
    Scan {
        relation: RelationBinding,
    },
    Filter {
        predicate: NormalizedExpression,
    },
    Project {
        expressions: Vec<ProjectionBinding>,
    },
    Aggregate {
        group_by: Vec<NormalizedExpression>,
    },
    Join {
        join_type: String,
        relation: RelationBinding,
        predicate: Option<NormalizedExpression>,
    },
    Window,
    Distinct,
    SetOperation {
        operator: String,
    },
    Cte {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedRelationalPlan {
    pub nodes: Vec<RelationalPlanNode>,
    pub relations: Vec<RelationBinding>,
    pub projections: Vec<ProjectionBinding>,
    pub filters: Vec<NormalizedExpression>,
    pub group_by: Vec<NormalizedExpression>,
    pub order_by: Vec<(NormalizedExpression, bool)>,
    pub limit: Option<u64>,
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProofStatus {
    Proved,
    Disproved,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProofObligation {
    pub name: String,
    pub status: ProofStatus,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationalPlanError {
    Parse(String),
    UnsupportedForSemanticProof(Vec<String>),
}

/// Compile the candidate into a backend-neutral relational plan. The caller
/// may inspect an unsupported plan for diagnostics, but it must not execute it
/// as semantically verified SQL.
pub fn compile_normalized_relational_plan(
    sql: &str,
) -> Result<NormalizedRelationalPlan, RelationalPlanError> {
    let plan = parse_normalized_relational_plan(sql)?;
    if plan.unsupported.is_empty() {
        Ok(plan)
    } else {
        Err(RelationalPlanError::UnsupportedForSemanticProof(
            plan.unsupported,
        ))
    }
}

fn parse_normalized_relational_plan(
    sql: &str,
) -> Result<NormalizedRelationalPlan, RelationalPlanError> {
    let statements = Parser::parse_sql(&GenericDialect {}, sql)
        .map_err(|error| RelationalPlanError::Parse(error.to_string()))?;
    if statements.len() != 1 {
        return Err(RelationalPlanError::Parse(
            "expected exactly one SQL statement".into(),
        ));
    }
    let Statement::Query(query) = &statements[0] else {
        return Err(RelationalPlanError::Parse(
            "only read-only SELECT/CTE queries are supported".into(),
        ));
    };
    let mut plan = NormalizedRelationalPlan {
        nodes: Vec::new(),
        relations: Vec::new(),
        projections: Vec::new(),
        filters: Vec::new(),
        group_by: Vec::new(),
        order_by: Vec::new(),
        limit: query
            .limit
            .as_ref()
            .and_then(|value| value.to_string().parse::<u64>().ok()),
        unsupported: Vec::new(),
    };
    compile_query_into_plan(query, &mut plan);
    canonicalize_plan_relations(&mut plan);
    Ok(plan)
}

fn canonicalize_plan_relations(plan: &mut NormalizedRelationalPlan) {
    let aliases = plan
        .relations
        .iter()
        .filter_map(|binding| {
            binding
                .alias
                .as_ref()
                .map(|alias| (alias.clone(), binding.relation.clone()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let concrete_relations = plan
        .relations
        .iter()
        .filter(|binding| !binding.derived)
        .map(|binding| binding.relation.clone())
        .collect::<BTreeSet<_>>();
    let sole_relation = (concrete_relations.len() == 1)
        .then(|| concrete_relations.iter().next().cloned())
        .flatten();
    let ambiguity_relation_count = if plan
        .nodes
        .iter()
        .any(|node| matches!(node, RelationalPlanNode::Join { .. }))
    {
        concrete_relations.len()
    } else {
        0
    };
    let mut ambiguous = BTreeSet::new();
    let mut normalize = |expression: &mut NormalizedExpression| {
        canonicalize_expression_relation(
            expression,
            &aliases,
            sole_relation.as_deref(),
            ambiguity_relation_count,
            &mut ambiguous,
        );
    };
    for projection in &mut plan.projections {
        normalize(&mut projection.expression);
    }
    for expression in &mut plan.filters {
        normalize(expression);
    }
    for expression in &mut plan.group_by {
        normalize(expression);
    }
    for (expression, _) in &mut plan.order_by {
        normalize(expression);
    }
    for node in &mut plan.nodes {
        match node {
            RelationalPlanNode::Filter { predicate }
            | RelationalPlanNode::Join {
                predicate: Some(predicate),
                ..
            } => normalize(predicate),
            RelationalPlanNode::Project { expressions } => {
                for projection in expressions {
                    normalize(&mut projection.expression);
                }
            }
            RelationalPlanNode::Aggregate { group_by } => {
                for expression in group_by {
                    normalize(expression);
                }
            }
            RelationalPlanNode::Scan { .. }
            | RelationalPlanNode::Join {
                predicate: None, ..
            }
            | RelationalPlanNode::Window
            | RelationalPlanNode::Distinct
            | RelationalPlanNode::SetOperation { .. }
            | RelationalPlanNode::Cte { .. } => {}
        }
    }
    plan.unsupported.extend(
        ambiguous
            .into_iter()
            .map(|column| format!("ambiguous_unqualified_column:{column}")),
    );
}

fn canonicalize_expression_relation(
    expression: &mut NormalizedExpression,
    aliases: &std::collections::BTreeMap<String, String>,
    sole_relation: Option<&str>,
    concrete_relation_count: usize,
    ambiguous: &mut BTreeSet<String>,
) {
    match expression {
        NormalizedExpression::Column { relation, name } => match relation {
            Some(current) => {
                if let Some(canonical) = aliases.get(current) {
                    *current = canonical.clone();
                }
            }
            None => {
                if let Some(canonical) = sole_relation {
                    *relation = Some(canonical.to_string());
                } else if concrete_relation_count > 1 {
                    ambiguous.insert(name.clone());
                }
            }
        },
        NormalizedExpression::Function { arguments, .. } => {
            for value in arguments {
                canonicalize_expression_relation(
                    value,
                    aliases,
                    sole_relation,
                    concrete_relation_count,
                    ambiguous,
                );
            }
        }
        NormalizedExpression::Binary { left, right, .. } => {
            canonicalize_expression_relation(
                left,
                aliases,
                sole_relation,
                concrete_relation_count,
                ambiguous,
            );
            canonicalize_expression_relation(
                right,
                aliases,
                sole_relation,
                concrete_relation_count,
                ambiguous,
            );
        }
        NormalizedExpression::Unary { expression, .. }
        | NormalizedExpression::IsNull { expression, .. }
        | NormalizedExpression::Cast { expression, .. } => canonicalize_expression_relation(
            expression,
            aliases,
            sole_relation,
            concrete_relation_count,
            ambiguous,
        ),
        NormalizedExpression::InList {
            expression, values, ..
        } => {
            canonicalize_expression_relation(
                expression,
                aliases,
                sole_relation,
                concrete_relation_count,
                ambiguous,
            );
            for value in values {
                canonicalize_expression_relation(
                    value,
                    aliases,
                    sole_relation,
                    concrete_relation_count,
                    ambiguous,
                );
            }
        }
        NormalizedExpression::Between {
            expression,
            low,
            high,
            ..
        } => {
            for value in [expression, low, high] {
                canonicalize_expression_relation(
                    value,
                    aliases,
                    sole_relation,
                    concrete_relation_count,
                    ambiguous,
                );
            }
        }
        NormalizedExpression::AtTimeZone {
            timestamp,
            timezone,
        } => {
            canonicalize_expression_relation(
                timestamp,
                aliases,
                sole_relation,
                concrete_relation_count,
                ambiguous,
            );
            canonicalize_expression_relation(
                timezone,
                aliases,
                sole_relation,
                concrete_relation_count,
                ambiguous,
            );
        }
        NormalizedExpression::Case {
            operand,
            branches,
            else_expression,
        } => {
            if let Some(value) = operand {
                canonicalize_expression_relation(
                    value,
                    aliases,
                    sole_relation,
                    concrete_relation_count,
                    ambiguous,
                );
            }
            for (condition, result) in branches {
                canonicalize_expression_relation(
                    condition,
                    aliases,
                    sole_relation,
                    concrete_relation_count,
                    ambiguous,
                );
                canonicalize_expression_relation(
                    result,
                    aliases,
                    sole_relation,
                    concrete_relation_count,
                    ambiguous,
                );
            }
            if let Some(value) = else_expression {
                canonicalize_expression_relation(
                    value,
                    aliases,
                    sole_relation,
                    concrete_relation_count,
                    ambiguous,
                );
            }
        }
        NormalizedExpression::Literal { .. } | NormalizedExpression::Unsupported { .. } => {}
    }
}

fn compile_query_into_plan(query: &Query, plan: &mut NormalizedRelationalPlan) {
    if let Some(with) = query.with.as_ref() {
        if with.recursive {
            plan.unsupported.push("recursive_cte".into());
        }
        for cte in &with.cte_tables {
            let name = normalize_identifier(&cte.alias.name.value);
            plan.nodes.push(RelationalPlanNode::Cte { name });
            compile_query_into_plan(&cte.query, plan);
        }
    }
    match query.body.as_ref() {
        SetExpr::Select(select) => compile_select_into_plan(select, plan),
        SetExpr::Query(nested) => compile_query_into_plan(nested, plan),
        SetExpr::SetOperation {
            op,
            set_quantifier: _,
            left,
            right,
        } => {
            plan.nodes.push(RelationalPlanNode::SetOperation {
                operator: op.to_string().to_ascii_lowercase(),
            });
            compile_set_expr_into_plan(left, plan);
            compile_set_expr_into_plan(right, plan);
        }
        other => plan
            .unsupported
            .push(format!("unsupported_query_body:{other}")),
    }
    if let Some(order_by) = query.order_by.as_ref() {
        if order_by.interpolate.is_some() {
            plan.unsupported.push("order_by_interpolate".into());
        }
        plan.order_by.extend(
            order_by
                .exprs
                .iter()
                .map(|item| (normalize_expression(&item.expr), item.asc.unwrap_or(true))),
        );
    }
    if query.offset.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || !query.limit_by.is_empty()
    {
        plan.unsupported.push("query_pagination_or_locking".into());
    }
}

fn compile_set_expr_into_plan(body: &SetExpr, plan: &mut NormalizedRelationalPlan) {
    match body {
        SetExpr::Select(select) => compile_select_into_plan(select, plan),
        SetExpr::Query(query) => compile_query_into_plan(query, plan),
        SetExpr::SetOperation {
            op,
            set_quantifier: _,
            left,
            right,
        } => {
            plan.nodes.push(RelationalPlanNode::SetOperation {
                operator: op.to_string().to_ascii_lowercase(),
            });
            compile_set_expr_into_plan(left, plan);
            compile_set_expr_into_plan(right, plan);
        }
        other => plan
            .unsupported
            .push(format!("unsupported_set_body:{other}")),
    }
}

fn compile_select_into_plan(select: &Select, plan: &mut NormalizedRelationalPlan) {
    if select.into.is_some()
        || select.top.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || select.connect_by.is_some()
    {
        plan.unsupported.push("vendor_select_extension".into());
    }
    for table in &select.from {
        compile_table_with_joins(table, plan);
    }
    let projections = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expression) => ProjectionBinding {
                expression: normalize_expression(expression),
                alias: None,
            },
            SelectItem::ExprWithAlias { expr, alias } => ProjectionBinding {
                expression: normalize_expression(expr),
                alias: Some(normalize_identifier(&alias.value)),
            },
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => ProjectionBinding {
                expression: NormalizedExpression::Unsupported {
                    sql: item.to_string(),
                },
                alias: None,
            },
        })
        .collect::<Vec<_>>();
    if projections
        .iter()
        .any(|projection| !projection.expression.is_supported())
    {
        plan.unsupported
            .push("unsupported_projection_expression".into());
    }
    plan.nodes.push(RelationalPlanNode::Project {
        expressions: projections.clone(),
    });
    plan.projections.extend(projections);
    for predicate in [select.selection.as_ref(), select.having.as_ref()]
        .into_iter()
        .flatten()
    {
        let predicate = normalize_expression(predicate);
        if !predicate.is_supported() {
            plan.unsupported
                .push("unsupported_filter_expression".into());
        }
        plan.nodes.push(RelationalPlanNode::Filter {
            predicate: predicate.clone(),
        });
        plan.filters.push(predicate);
    }
    match &select.group_by {
        GroupByExpr::Expressions(expressions, modifiers) => {
            if !modifiers.is_empty() {
                plan.unsupported.push("group_by_modifier".into());
            }
            let expressions = expressions
                .iter()
                .map(normalize_expression)
                .collect::<Vec<_>>();
            if expressions.iter().any(|value| !value.is_supported()) {
                plan.unsupported.push("unsupported_group_expression".into());
            }
            if !expressions.is_empty() {
                plan.nodes.push(RelationalPlanNode::Aggregate {
                    group_by: expressions.clone(),
                });
                plan.group_by.extend(expressions);
            }
        }
        GroupByExpr::All(_) => plan.unsupported.push("group_by_all".into()),
    }
    if select.distinct.is_some() {
        plan.nodes.push(RelationalPlanNode::Distinct);
    }
    if !select.named_window.is_empty()
        || select.qualify.is_some()
        || plan.projections.iter().any(|projection| {
            matches!(
                projection.expression,
                NormalizedExpression::Function {
                    window: Some(_),
                    ..
                }
            )
        })
    {
        plan.nodes.push(RelationalPlanNode::Window);
    }
}

fn compile_table_with_joins(table: &TableWithJoins, plan: &mut NormalizedRelationalPlan) {
    let relation = compile_table_factor(&table.relation, plan);
    plan.nodes.push(RelationalPlanNode::Scan {
        relation: relation.clone(),
    });
    plan.relations.push(relation);
    for join in &table.joins {
        let relation = compile_table_factor(&join.relation, plan);
        let (join_type, predicate) = match &join.join_operator {
            JoinOperator::Inner(constraint) => ("inner", normalize_join_constraint(constraint)),
            JoinOperator::LeftOuter(constraint) => {
                ("left_outer", normalize_join_constraint(constraint))
            }
            JoinOperator::RightOuter(constraint) => {
                ("right_outer", normalize_join_constraint(constraint))
            }
            JoinOperator::FullOuter(constraint) => {
                ("full_outer", normalize_join_constraint(constraint))
            }
            JoinOperator::LeftSemi(constraint) => {
                ("left_semi", normalize_join_constraint(constraint))
            }
            JoinOperator::RightSemi(constraint) => {
                ("right_semi", normalize_join_constraint(constraint))
            }
            JoinOperator::LeftAnti(constraint) => {
                ("left_anti", normalize_join_constraint(constraint))
            }
            JoinOperator::RightAnti(constraint) => {
                ("right_anti", normalize_join_constraint(constraint))
            }
            JoinOperator::CrossJoin => ("cross", None),
            JoinOperator::AsOf {
                match_condition,
                constraint,
            } => {
                let mut predicate = normalize_join_constraint(constraint);
                let match_condition = normalize_expression(match_condition);
                predicate = Some(match predicate {
                    Some(existing) => NormalizedExpression::Binary {
                        operator: "and".into(),
                        left: Box::new(existing),
                        right: Box::new(match_condition),
                    },
                    None => match_condition,
                });
                ("as_of", predicate)
            }
            JoinOperator::CrossApply | JoinOperator::OuterApply => {
                plan.unsupported.push("apply_join".into());
                ("unsupported_apply", None)
            }
        };
        if predicate
            .as_ref()
            .is_some_and(|value| !value.is_supported())
        {
            plan.unsupported.push("unsupported_join_predicate".into());
        }
        plan.nodes.push(RelationalPlanNode::Join {
            join_type: join_type.into(),
            relation: relation.clone(),
            predicate,
        });
        plan.relations.push(relation);
    }
}

fn compile_table_factor(
    factor: &TableFactor,
    plan: &mut NormalizedRelationalPlan,
) -> RelationBinding {
    match factor {
        TableFactor::Table {
            name,
            alias,
            args,
            with_hints,
            version,
            ..
        } if args.is_none() && with_hints.is_empty() && version.is_none() => RelationBinding {
            relation: normalize_object_name(name),
            alias: alias
                .as_ref()
                .map(|value| normalize_identifier(&value.name.value)),
            derived: false,
        },
        TableFactor::Derived {
            lateral,
            subquery,
            alias,
        } if !lateral => {
            compile_query_into_plan(subquery, plan);
            RelationBinding {
                relation: alias.as_ref().map_or_else(
                    || "derived".into(),
                    |value| normalize_identifier(&value.name.value),
                ),
                alias: alias
                    .as_ref()
                    .map(|value| normalize_identifier(&value.name.value)),
                derived: true,
            }
        }
        TableFactor::NestedJoin {
            table_with_joins,
            alias,
        } => {
            compile_table_with_joins(table_with_joins, plan);
            RelationBinding {
                relation: "nested_join".into(),
                alias: alias
                    .as_ref()
                    .map(|value| normalize_identifier(&value.name.value)),
                derived: true,
            }
        }
        other => {
            plan.unsupported
                .push(format!("unsupported_table_factor:{other}"));
            RelationBinding {
                relation: "unsupported".into(),
                alias: None,
                derived: true,
            }
        }
    }
}

fn normalize_join_constraint(constraint: &JoinConstraint) -> Option<NormalizedExpression> {
    match constraint {
        JoinConstraint::On(expression) => Some(normalize_expression(expression)),
        JoinConstraint::Using(columns) => Some(NormalizedExpression::Function {
            name: "using".into(),
            arguments: columns
                .iter()
                .map(|column| NormalizedExpression::Column {
                    relation: None,
                    name: normalize_identifier(&column.value),
                })
                .collect(),
            distinct: false,
            window: None,
        }),
        JoinConstraint::Natural => Some(NormalizedExpression::Unsupported {
            sql: "natural_join".into(),
        }),
        JoinConstraint::None => None,
    }
}

fn normalize_expression(expression: &Expr) -> NormalizedExpression {
    match expression {
        Expr::Identifier(identifier) => NormalizedExpression::Column {
            relation: None,
            name: normalize_identifier(&identifier.value),
        },
        Expr::CompoundIdentifier(identifiers) if !identifiers.is_empty() => {
            NormalizedExpression::Column {
                relation: (identifiers.len() > 1).then(|| {
                    identifiers[..identifiers.len() - 1]
                        .iter()
                        .map(|identifier| normalize_identifier(&identifier.value))
                        .collect::<Vec<_>>()
                        .join(".")
                }),
                name: normalize_identifier(&identifiers[identifiers.len() - 1].value),
            }
        }
        Expr::Value(value) => NormalizedExpression::Literal {
            value: normalize_literal(value),
        },
        Expr::TypedString { data_type, value } => NormalizedExpression::Literal {
            value: format!("{}:{}", data_type.to_string().to_ascii_lowercase(), value),
        },
        Expr::Nested(inner) => normalize_expression(inner),
        Expr::BinaryOp { left, op, right } => NormalizedExpression::Binary {
            operator: normalize_binary_operator(op.clone()),
            left: Box::new(normalize_expression(left)),
            right: Box::new(normalize_expression(right)),
        },
        Expr::UnaryOp { op, expr } => NormalizedExpression::Unary {
            operator: normalize_unary_operator(*op),
            expression: Box::new(normalize_expression(expr)),
        },
        Expr::IsNull(inner) => NormalizedExpression::IsNull {
            expression: Box::new(normalize_expression(inner)),
            negated: false,
        },
        Expr::IsNotNull(inner) => NormalizedExpression::IsNull {
            expression: Box::new(normalize_expression(inner)),
            negated: true,
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => NormalizedExpression::InList {
            expression: Box::new(normalize_expression(expr)),
            values: list.iter().map(normalize_expression).collect(),
            negated: *negated,
        },
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => NormalizedExpression::Between {
            expression: Box::new(normalize_expression(expr)),
            low: Box::new(normalize_expression(low)),
            high: Box::new(normalize_expression(high)),
            negated: *negated,
        },
        Expr::Cast {
            expr, data_type, ..
        } => NormalizedExpression::Cast {
            expression: Box::new(normalize_expression(expr)),
            data_type: data_type.to_string().to_ascii_lowercase(),
        },
        Expr::AtTimeZone {
            timestamp,
            time_zone,
        } => NormalizedExpression::AtTimeZone {
            timestamp: Box::new(normalize_expression(timestamp)),
            timezone: Box::new(normalize_expression(time_zone)),
        },
        Expr::Function(function) => {
            let function_name = normalize_object_name(&function.name);
            if !supported_semantic_function(&function_name) {
                return NormalizedExpression::Unsupported {
                    sql: expression.to_string(),
                };
            }
            let FunctionArguments::List(arguments) = &function.args else {
                return NormalizedExpression::Unsupported {
                    sql: expression.to_string(),
                };
            };
            if !arguments.clauses.is_empty() {
                return NormalizedExpression::Unsupported {
                    sql: expression.to_string(),
                };
            }
            let mut normalized = Vec::with_capacity(arguments.args.len());
            for argument in &arguments.args {
                match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(argument)) => {
                        normalized.push(normalize_expression(argument));
                    }
                    FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
                        normalized.push(NormalizedExpression::Literal { value: "*".into() });
                    }
                    _ => {
                        return NormalizedExpression::Unsupported {
                            sql: expression.to_string(),
                        };
                    }
                }
            }
            NormalizedExpression::Function {
                name: function_name,
                arguments: normalized,
                distinct: matches!(
                    arguments.duplicate_treatment,
                    Some(DuplicateTreatment::Distinct)
                ),
                window: function
                    .over
                    .as_ref()
                    .map(|window| normalize_sql_fragment(&window.to_string())),
            }
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } if conditions.len() == results.len() => NormalizedExpression::Case {
            operand: operand.as_deref().map(normalize_expression).map(Box::new),
            branches: conditions
                .iter()
                .zip(results)
                .map(|(condition, result)| {
                    (
                        normalize_expression(condition),
                        normalize_expression(result),
                    )
                })
                .collect(),
            else_expression: else_result
                .as_deref()
                .map(normalize_expression)
                .map(Box::new),
        },
        _ => NormalizedExpression::Unsupported {
            sql: expression.to_string(),
        },
    }
}

fn normalize_identifier(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn supported_semantic_function(name: &str) -> bool {
    matches!(
        name,
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "approx_distinct"
            | "coalesce"
            | "ifnull"
            | "nvl"
            | "nullif"
            | "date"
            | "date_trunc"
            | "date_format"
            | "convert_tz"
            | "timezone"
            | "from_utc_timestamp"
            | "lag"
            | "lead"
            | "row_number"
            | "rank"
            | "dense_rank"
            | "round"
            | "floor"
            | "ceil"
            | "abs"
            | "lower"
            | "upper"
    )
}

fn normalize_object_name(value: &sqlparser::ast::ObjectName) -> String {
    value
        .0
        .iter()
        .map(|part| normalize_identifier(&part.value))
        .collect::<Vec<_>>()
        .join(".")
}

fn normalize_literal(value: &Value) -> String {
    match value {
        Value::SingleQuotedString(value)
        | Value::DoubleQuotedString(value)
        | Value::TripleSingleQuotedString(value)
        | Value::TripleDoubleQuotedString(value)
        | Value::EscapedStringLiteral(value)
        | Value::NationalStringLiteral(value)
        | Value::SingleQuotedRawStringLiteral(value)
        | Value::DoubleQuotedRawStringLiteral(value)
        | Value::TripleSingleQuotedRawStringLiteral(value)
        | Value::TripleDoubleQuotedRawStringLiteral(value) => value.clone(),
        _ => normalize_sql_fragment(&value.to_string()),
    }
}

fn normalize_sql_fragment(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace() && !matches!(character, '`' | '"'))
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_binary_operator(operator: BinaryOperator) -> String {
    operator.to_string().to_ascii_lowercase()
}

fn normalize_unary_operator(operator: UnaryOperator) -> String {
    operator.to_string().to_ascii_lowercase()
}

fn normalized_expression_equivalent(
    left: &NormalizedExpression,
    right: &NormalizedExpression,
) -> bool {
    match (left, right) {
        (
            NormalizedExpression::Column {
                relation: left_relation,
                name: left,
            },
            NormalizedExpression::Column {
                relation: right_relation,
                name: right,
            },
        ) => {
            left == right
                && match (left_relation.as_deref(), right_relation.as_deref()) {
                    (Some(left), Some(right)) => relation_matches(left, right),
                    // An unqualified contract column intentionally means the
                    // uniquely bound column from the compiled plan. A
                    // qualified contract never ignores the candidate's
                    // relation.
                    (None, _) | (_, None) => true,
                }
        }
        (
            NormalizedExpression::Literal { value: left },
            NormalizedExpression::Literal { value: right },
        ) => left == right,
        (
            NormalizedExpression::Function {
                name: left_name,
                arguments: left_arguments,
                distinct: left_distinct,
                window: left_window,
            },
            NormalizedExpression::Function {
                name: right_name,
                arguments: right_arguments,
                distinct: right_distinct,
                window: right_window,
            },
        ) => {
            left_name == right_name
                && left_distinct == right_distinct
                && left_window == right_window
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(left, right)| normalized_expression_equivalent(left, right))
        }
        (
            NormalizedExpression::Binary {
                operator: left_operator,
                left: left_left,
                right: left_right,
            },
            NormalizedExpression::Binary {
                operator: right_operator,
                left: right_left,
                right: right_right,
            },
        ) => {
            left_operator == right_operator
                && normalized_expression_equivalent(left_left, right_left)
                && normalized_expression_equivalent(left_right, right_right)
        }
        (
            NormalizedExpression::Unary {
                operator: left_operator,
                expression: left_expression,
            },
            NormalizedExpression::Unary {
                operator: right_operator,
                expression: right_expression,
            },
        ) => {
            left_operator == right_operator
                && normalized_expression_equivalent(left_expression, right_expression)
        }
        (
            NormalizedExpression::IsNull {
                expression: left_expression,
                negated: left_negated,
            },
            NormalizedExpression::IsNull {
                expression: right_expression,
                negated: right_negated,
            },
        ) => {
            left_negated == right_negated
                && normalized_expression_equivalent(left_expression, right_expression)
        }
        (
            NormalizedExpression::Cast {
                expression: left_expression,
                data_type: left_type,
            },
            NormalizedExpression::Cast {
                expression: right_expression,
                data_type: right_type,
            },
        ) => {
            left_type == right_type
                && normalized_expression_equivalent(left_expression, right_expression)
        }
        (
            NormalizedExpression::AtTimeZone {
                timestamp: left_timestamp,
                timezone: left_timezone,
            },
            NormalizedExpression::AtTimeZone {
                timestamp: right_timestamp,
                timezone: right_timezone,
            },
        ) => {
            normalized_expression_equivalent(left_timestamp, right_timestamp)
                && normalized_expression_equivalent(left_timezone, right_timezone)
        }
        _ => left == right,
    }
}

fn expression_references_column(expression: &NormalizedExpression, column: &str) -> bool {
    let normalized = normalize_identifier(column);
    let (expected_relation, expected_name) = normalized
        .rsplit_once('.')
        .map_or((None, normalized.as_str()), |(relation, name)| {
            (Some(relation), name)
        });
    match expression {
        NormalizedExpression::Column { relation, name } => {
            name == expected_name
                && expected_relation.is_none_or(|expected| {
                    relation
                        .as_deref()
                        .is_some_and(|actual| relation_matches(actual, expected))
                })
        }
        NormalizedExpression::Function { arguments, .. } => arguments
            .iter()
            .any(|value| expression_references_column(value, &normalized)),
        NormalizedExpression::Binary { left, right, .. } => {
            expression_references_column(left, &normalized)
                || expression_references_column(right, &normalized)
        }
        NormalizedExpression::Unary { expression, .. }
        | NormalizedExpression::IsNull { expression, .. }
        | NormalizedExpression::Cast { expression, .. } => {
            expression_references_column(expression, &normalized)
        }
        NormalizedExpression::InList {
            expression, values, ..
        } => {
            expression_references_column(expression, &normalized)
                || values
                    .iter()
                    .any(|value| expression_references_column(value, &normalized))
        }
        NormalizedExpression::Between {
            expression,
            low,
            high,
            ..
        } => {
            expression_references_column(expression, &normalized)
                || expression_references_column(low, &normalized)
                || expression_references_column(high, &normalized)
        }
        NormalizedExpression::AtTimeZone {
            timestamp,
            timezone,
        } => {
            expression_references_column(timestamp, &normalized)
                || expression_references_column(timezone, &normalized)
        }
        NormalizedExpression::Case {
            operand,
            branches,
            else_expression,
        } => {
            operand
                .as_deref()
                .is_some_and(|value| expression_references_column(value, &normalized))
                || branches.iter().any(|(condition, result)| {
                    expression_references_column(condition, &normalized)
                        || expression_references_column(result, &normalized)
                })
                || else_expression
                    .as_deref()
                    .is_some_and(|value| expression_references_column(value, &normalized))
        }
        NormalizedExpression::Literal { .. } | NormalizedExpression::Unsupported { .. } => false,
    }
}

fn relation_matches(actual: &str, expected: &str) -> bool {
    let actual = normalize_identifier(actual);
    let expected = normalize_identifier(expected);
    actual == expected
        || (!expected.contains('.')
            && actual
                .rsplit('.')
                .next()
                .is_some_and(|name| name == expected))
}

fn projection_proves_column(plan: &NormalizedRelationalPlan, column: &str) -> bool {
    let column = normalize_identifier(column);
    let output_name = column.rsplit('.').next().unwrap_or(&column);
    plan.projections.iter().any(|projection| {
        projection.alias.as_deref() == Some(output_name)
            || expression_references_column(&projection.expression, &column)
    })
}

fn group_proves_column(plan: &NormalizedRelationalPlan, column: &str) -> bool {
    plan.group_by
        .iter()
        .any(|expression| expression_references_column(expression, column))
}

fn metric_ir_to_normalized(expression: &MetricExpressionIR) -> NormalizedExpression {
    match expression {
        MetricExpressionIR::Column(column) => NormalizedExpression::Column {
            relation: column
                .rsplit_once('.')
                .map(|(relation, _)| normalize_identifier(relation)),
            name: normalize_identifier(column.rsplit('.').next().unwrap_or(column)),
        },
        MetricExpressionIR::Aggregate {
            function,
            expression,
            distinct,
        } => NormalizedExpression::Function {
            name: normalize_identifier(function),
            arguments: vec![metric_ir_to_normalized(expression)],
            distinct: *distinct,
            window: None,
        },
        MetricExpressionIR::Ratio {
            numerator,
            denominator,
        } => NormalizedExpression::Binary {
            operator: "/".into(),
            left: Box::new(metric_ir_to_normalized(numerator)),
            right: Box::new(metric_ir_to_normalized(denominator)),
        },
        MetricExpressionIR::Literal(value) if value == "*" => {
            NormalizedExpression::Literal { value: "*".into() }
        }
        MetricExpressionIR::Literal(value) => parse_normalized_expression(value)
            .unwrap_or_else(|| NormalizedExpression::Unsupported { sql: value.clone() }),
    }
}

fn parse_normalized_expression(value: &str) -> Option<NormalizedExpression> {
    let statements = Parser::parse_sql(&GenericDialect {}, &format!("SELECT {value}")).ok()?;
    let Statement::Query(query) = statements.first()? else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let item = select.projection.first()?;
    match item {
        SelectItem::UnnamedExpr(expression)
        | SelectItem::ExprWithAlias {
            expr: expression, ..
        } => Some(normalize_expression(expression)),
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => None,
    }
}

fn projection_contains_expression(
    plan: &NormalizedRelationalPlan,
    expression: &NormalizedExpression,
) -> bool {
    plan.projections
        .iter()
        .any(|projection| projection.expression.contains_exact(expression))
}

fn literal_matches(expression: &NormalizedExpression, expected: &str) -> bool {
    let NormalizedExpression::Literal { value } = expression else {
        return false;
    };
    value == expected
        || value
            .split_once(':')
            .is_some_and(|(_, typed_value)| typed_value == expected)
        || normalize_sql_fragment(value) == normalize_sql_fragment(expected)
}

fn column_matches(expression: &NormalizedExpression, expected: &str) -> bool {
    matches!(expression, NormalizedExpression::Column { .. })
        && expression_references_column(expression, expected)
}

fn predicate_terms<'a>(
    expression: &'a NormalizedExpression,
    output: &mut Vec<&'a NormalizedExpression>,
) {
    if let NormalizedExpression::Binary {
        operator,
        left,
        right,
    } = expression
    {
        if operator == "and" {
            predicate_terms(left, output);
            predicate_terms(right, output);
            return;
        }
    }
    output.push(expression);
}

fn plan_predicates(plan: &NormalizedRelationalPlan) -> Vec<&NormalizedExpression> {
    let mut predicates = Vec::new();
    for filter in &plan.filters {
        predicate_terms(filter, &mut predicates);
    }
    predicates
}

fn plan_proves_filter(plan: &NormalizedRelationalPlan, filter: &SemanticFilter) -> bool {
    plan_predicates(plan).into_iter().any(|predicate| match filter {
        SemanticFilter::Equals { field, value } => matches!(
            predicate,
            NormalizedExpression::Binary { operator, left, right }
                if operator == "=" && ((column_matches(left, field) && literal_matches(right, value))
                    || (column_matches(right, field) && literal_matches(left, value)))
        ),
        SemanticFilter::In { field, values } => matches!(
            predicate,
            NormalizedExpression::InList { expression, values: actual, negated: false }
                if column_matches(expression, field)
                    && values.iter().all(|expected| actual.iter().any(|value| literal_matches(value, expected)))
        ),
        SemanticFilter::Compare {
            field,
            operator,
            value,
        } => matches!(
            predicate,
            NormalizedExpression::Binary { operator: actual_operator, left, right }
                if actual_operator == &normalize_sql_fragment(operator)
                    && column_matches(left, field)
                    && literal_matches(right, value)
        ),
        SemanticFilter::RawBounded { expression } => parse_filter_expression(expression)
            .as_ref()
            .is_some_and(|expected| normalized_expression_equivalent(predicate, expected)),
    })
}

fn parse_filter_expression(value: &str) -> Option<NormalizedExpression> {
    let statements =
        Parser::parse_sql(&GenericDialect {}, &format!("SELECT 1 WHERE {value}")).ok()?;
    let Statement::Query(query) = statements.first()? else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    select.selection.as_ref().map(normalize_expression)
}

fn plan_proves_dedup(plan: &NormalizedRelationalPlan, key: &str) -> bool {
    plan.projections
        .iter()
        .any(|projection| match &projection.expression {
            NormalizedExpression::Function {
                name,
                arguments,
                distinct: true,
                ..
            } if matches!(name.as_str(), "count" | "approx_distinct") => arguments
                .first()
                .is_some_and(|argument| expression_references_column(argument, key)),
            _ => false,
        })
        || (plan
            .nodes
            .iter()
            .any(|node| matches!(node, RelationalPlanNode::Distinct))
            && projection_proves_column(plan, key))
}

fn plan_proves_population_subject(plan: &NormalizedRelationalPlan, subject: &str) -> bool {
    if subject.eq_ignore_ascii_case("query_rows") {
        return true;
    }
    let subject = normalize_identifier(subject);
    let singular = subject.strip_suffix('s').unwrap_or(&subject);
    plan.relations.iter().any(|relation| {
        let name = relation
            .relation
            .rsplit('.')
            .next()
            .unwrap_or(&relation.relation);
        name == subject
            || name.strip_suffix('s').unwrap_or(name) == singular
            || relation.alias.as_deref() == Some(subject.as_str())
    }) || plan.projections.iter().any(|projection| {
        projection.expression.column_name().is_some_and(|column| {
            column == subject
                || column.strip_suffix("_id") == Some(singular)
                || column.strip_suffix('s').unwrap_or(column) == singular
        })
    })
}

fn plan_proves_exclusion(plan: &NormalizedRelationalPlan, marker: &str) -> bool {
    let marker = normalize_identifier(marker);
    plan_predicates(plan)
        .into_iter()
        .any(|predicate| match predicate {
            NormalizedExpression::Binary {
                operator,
                left,
                right,
            } if matches!(operator.as_str(), "<>" | "!=" | "=") => {
                let (column, value) =
                    if let NormalizedExpression::Column { name, .. } = left.as_ref() {
                        (name.as_str(), right.as_ref())
                    } else if let NormalizedExpression::Column { name, .. } = right.as_ref() {
                        (name.as_str(), left.as_ref())
                    } else {
                        return false;
                    };
                let role_match = column.split('_').any(|part| part == marker)
                    || column == format!("is_{marker}")
                    || column == format!("{marker}_user");
                role_match
                    && ((operator != "="
                        && (literal_matches(value, "true") || literal_matches(value, "1")))
                        || (operator == "="
                            && (literal_matches(value, "false") || literal_matches(value, "0"))))
            }
            NormalizedExpression::InList {
                expression,
                values,
                negated: true,
            } => {
                expression
                    .column_name()
                    .is_some_and(|name| name.split('_').any(|part| part == marker))
                    && values.iter().any(|value| literal_matches(value, &marker))
            }
            _ => false,
        })
}

fn expression_contains_literal(expression: &NormalizedExpression, expected: &str) -> bool {
    if literal_matches(expression, expected) {
        return true;
    }
    match expression {
        NormalizedExpression::Function { arguments, .. } => arguments
            .iter()
            .any(|value| expression_contains_literal(value, expected)),
        NormalizedExpression::Binary { left, right, .. } => {
            expression_contains_literal(left, expected)
                || expression_contains_literal(right, expected)
        }
        NormalizedExpression::Unary { expression, .. }
        | NormalizedExpression::IsNull { expression, .. }
        | NormalizedExpression::Cast { expression, .. } => {
            expression_contains_literal(expression, expected)
        }
        NormalizedExpression::InList {
            expression, values, ..
        } => {
            expression_contains_literal(expression, expected)
                || values
                    .iter()
                    .any(|value| expression_contains_literal(value, expected))
        }
        NormalizedExpression::Between {
            expression,
            low,
            high,
            ..
        } => {
            expression_contains_literal(expression, expected)
                || expression_contains_literal(low, expected)
                || expression_contains_literal(high, expected)
        }
        NormalizedExpression::AtTimeZone {
            timestamp,
            timezone,
        } => {
            expression_contains_literal(timestamp, expected)
                || expression_contains_literal(timezone, expected)
        }
        NormalizedExpression::Case {
            operand,
            branches,
            else_expression,
        } => {
            operand
                .as_deref()
                .is_some_and(|value| expression_contains_literal(value, expected))
                || branches.iter().any(|(condition, result)| {
                    expression_contains_literal(condition, expected)
                        || expression_contains_literal(result, expected)
                })
                || else_expression
                    .as_deref()
                    .is_some_and(|value| expression_contains_literal(value, expected))
        }
        NormalizedExpression::Column { .. }
        | NormalizedExpression::Literal { .. }
        | NormalizedExpression::Unsupported { .. } => false,
    }
}

fn expression_contains_operator(expression: &NormalizedExpression, expected: &str) -> bool {
    match expression {
        NormalizedExpression::Binary {
            operator,
            left,
            right,
        } => {
            operator == expected
                || expression_contains_operator(left, expected)
                || expression_contains_operator(right, expected)
        }
        NormalizedExpression::Function { arguments, .. } => arguments
            .iter()
            .any(|value| expression_contains_operator(value, expected)),
        NormalizedExpression::Unary { expression, .. }
        | NormalizedExpression::IsNull { expression, .. }
        | NormalizedExpression::Cast { expression, .. } => {
            expression_contains_operator(expression, expected)
        }
        NormalizedExpression::InList {
            expression, values, ..
        } => {
            expression_contains_operator(expression, expected)
                || values
                    .iter()
                    .any(|value| expression_contains_operator(value, expected))
        }
        NormalizedExpression::Between {
            expression,
            low,
            high,
            ..
        } => {
            expression_contains_operator(expression, expected)
                || expression_contains_operator(low, expected)
                || expression_contains_operator(high, expected)
        }
        NormalizedExpression::AtTimeZone {
            timestamp,
            timezone,
        } => {
            expression_contains_operator(timestamp, expected)
                || expression_contains_operator(timezone, expected)
        }
        NormalizedExpression::Case {
            operand,
            branches,
            else_expression,
        } => {
            operand
                .as_deref()
                .is_some_and(|value| expression_contains_operator(value, expected))
                || branches.iter().any(|(condition, result)| {
                    expression_contains_operator(condition, expected)
                        || expression_contains_operator(result, expected)
                })
                || else_expression
                    .as_deref()
                    .is_some_and(|value| expression_contains_operator(value, expected))
        }
        NormalizedExpression::Column { .. }
        | NormalizedExpression::Literal { .. }
        | NormalizedExpression::Unsupported { .. } => false,
    }
}

fn expression_contains_function(expression: &NormalizedExpression, expected: &str) -> bool {
    match expression {
        NormalizedExpression::Function {
            name, arguments, ..
        } => {
            name == expected
                || arguments
                    .iter()
                    .any(|value| expression_contains_function(value, expected))
        }
        NormalizedExpression::Binary { left, right, .. } => {
            expression_contains_function(left, expected)
                || expression_contains_function(right, expected)
        }
        NormalizedExpression::Unary { expression, .. }
        | NormalizedExpression::IsNull { expression, .. }
        | NormalizedExpression::Cast { expression, .. } => {
            expression_contains_function(expression, expected)
        }
        NormalizedExpression::InList {
            expression, values, ..
        } => {
            expression_contains_function(expression, expected)
                || values
                    .iter()
                    .any(|value| expression_contains_function(value, expected))
        }
        NormalizedExpression::Between {
            expression,
            low,
            high,
            ..
        } => {
            expression_contains_function(expression, expected)
                || expression_contains_function(low, expected)
                || expression_contains_function(high, expected)
        }
        NormalizedExpression::AtTimeZone {
            timestamp,
            timezone,
        } => {
            expression_contains_function(timestamp, expected)
                || expression_contains_function(timezone, expected)
        }
        NormalizedExpression::Case {
            operand,
            branches,
            else_expression,
        } => {
            operand
                .as_deref()
                .is_some_and(|value| expression_contains_function(value, expected))
                || branches.iter().any(|(condition, result)| {
                    expression_contains_function(condition, expected)
                        || expression_contains_function(result, expected)
                })
                || else_expression
                    .as_deref()
                    .is_some_and(|value| expression_contains_function(value, expected))
        }
        NormalizedExpression::Column { .. }
        | NormalizedExpression::Literal { .. }
        | NormalizedExpression::Unsupported { .. } => false,
    }
}

fn plan_proves_time(plan: &NormalizedRelationalPlan, time: &TimeSemantics) -> bool {
    let terms = plan_predicates(plan);
    let equality = terms.iter().any(|predicate| {
        matches!(
            predicate,
            NormalizedExpression::Binary { operator, left, right }
            if operator == "=" && expression_references_column(left, &time.column)
                    && literal_matches(right, &time.start_inclusive)
        )
    });
    if equality {
        return chrono::NaiveDate::parse_from_str(&time.start_inclusive, "%Y-%m-%d")
            .ok()
            .zip(chrono::NaiveDate::parse_from_str(&time.end_exclusive, "%Y-%m-%d").ok())
            .is_some_and(|(start, end)| end.signed_duration_since(start).num_days() == 1);
    }
    let lower = terms.iter().any(|predicate| {
        matches!(
            predicate,
            NormalizedExpression::Binary { operator, left, right }
            if operator == ">=" && expression_references_column(left, &time.column)
                    && literal_matches(right, &time.start_inclusive)
        )
    });
    let upper = terms.iter().any(|predicate| {
        matches!(
            predicate,
            NormalizedExpression::Binary { operator, left, right }
            if operator == "<" && expression_references_column(left, &time.column)
                    && literal_matches(right, &time.end_exclusive)
        )
    });
    lower && upper
}

fn plan_proves_timezone(plan: &NormalizedRelationalPlan, time: &TimeSemantics) -> bool {
    let column = normalize_identifier(time.column.rsplit('.').next().unwrap_or(&time.column));
    if matches!(column.as_str(), "date" | "day" | "dt")
        || column
            .rsplit('_')
            .next()
            .is_some_and(|suffix| matches!(suffix, "date" | "day" | "dt"))
    {
        return true;
    }
    let expressions = plan
        .projections
        .iter()
        .map(|value| &value.expression)
        .chain(plan.filters.iter());
    expressions
        .into_iter()
        .any(|expression| expression_proves_timezone(expression, time))
}

fn expression_proves_timezone(expression: &NormalizedExpression, time: &TimeSemantics) -> bool {
    match expression {
        NormalizedExpression::AtTimeZone {
            timestamp,
            timezone,
        } => {
            expression_references_column(timestamp, &time.column)
                && expression_contains_literal(timezone, &time.timezone)
        }
        NormalizedExpression::Function { arguments, .. } => {
            ([
                "convert_tz",
                "timezone",
                "from_utc_timestamp",
                "date_format",
            ]
            .iter()
            .any(|function| {
                expression_contains_function(expression, function)
                    && expression_references_column(expression, &time.column)
                    && expression_contains_literal(expression, &time.timezone)
            })) || arguments
                .iter()
                .any(|value| expression_proves_timezone(value, time))
        }
        NormalizedExpression::Binary { left, right, .. } => {
            expression_proves_timezone(left, time) || expression_proves_timezone(right, time)
        }
        NormalizedExpression::Unary { expression, .. }
        | NormalizedExpression::IsNull { expression, .. }
        | NormalizedExpression::Cast { expression, .. } => {
            expression_proves_timezone(expression, time)
        }
        NormalizedExpression::InList {
            expression, values, ..
        } => {
            expression_proves_timezone(expression, time)
                || values
                    .iter()
                    .any(|value| expression_proves_timezone(value, time))
        }
        NormalizedExpression::Between {
            expression,
            low,
            high,
            ..
        } => {
            expression_proves_timezone(expression, time)
                || expression_proves_timezone(low, time)
                || expression_proves_timezone(high, time)
        }
        NormalizedExpression::Case {
            operand,
            branches,
            else_expression,
        } => {
            operand
                .as_deref()
                .is_some_and(|value| expression_proves_timezone(value, time))
                || branches.iter().any(|(condition, result)| {
                    expression_proves_timezone(condition, time)
                        || expression_proves_timezone(result, time)
                })
                || else_expression
                    .as_deref()
                    .is_some_and(|value| expression_proves_timezone(value, time))
        }
        NormalizedExpression::Column { .. }
        | NormalizedExpression::Literal { .. }
        | NormalizedExpression::Unsupported { .. } => false,
    }
}

fn plan_proves_comparison(plan: &NormalizedRelationalPlan, comparison: &ComparisonSpec) -> bool {
    let expressions = plan
        .projections
        .iter()
        .map(|value| &value.expression)
        .chain(plan.filters.iter())
        .collect::<Vec<_>>();
    let temporal_windows = match (
        comparison.baseline_window.as_ref(),
        comparison.treatment_window.as_ref(),
    ) {
        (Some(baseline), Some(treatment)) => {
            comparison_windows_are_aligned(baseline, treatment)
                && comparison_window_is_bound(plan, baseline)
                && comparison_window_is_bound(plan, treatment)
        }
        (None, None) => false,
        _ => return false,
    };
    let cohorts = temporal_windows
        || expressions.iter().any(|expression| {
            expression_contains_literal(expression, &comparison.baseline)
                && expression_contains_literal(expression, &comparison.treatment)
        });
    let method = normalize_identifier(&comparison.method);
    let method_proved = method.is_empty()
        || (method == "difference"
            && expressions.iter().any(|expression| {
                expression_contains_operator(expression, "-")
                    || expression_contains_function(expression, "lag")
            }))
        || (matches!(method.as_str(), "ratio" | "percent")
            && expressions
                .iter()
                .any(|expression| expression_contains_operator(expression, "/")))
        || (temporal_windows
            && matches!(
                method.as_str(),
                "mom"
                    | "yoy"
                    | "wow"
                    | "dod"
                    | "periodoverperiod"
                    | "period_over_period"
                    | "period-over-period"
                    | "periodcomparison"
                    | "period_comparison"
            )
            && expressions.iter().any(|expression| {
                expression_contains_operator(expression, "-")
                    || expression_contains_operator(expression, "/")
                    || expression_contains_function(expression, "lag")
            }));
    cohorts && method_proved
}

fn comparison_window_is_bound(plan: &NormalizedRelationalPlan, window: &ComparisonWindow) -> bool {
    let expressions = plan
        .projections
        .iter()
        .map(|projection| &projection.expression)
        .chain(plan.filters.iter())
        .collect::<Vec<_>>();
    let proves_boundary = |operator: &str, expected: &str| {
        expressions.iter().any(|expression| {
            expression_proves_time_boundary(expression, &window.column, operator, expected)
        })
    };
    proves_boundary(">=", &window.start_inclusive)
        && proves_boundary("<", &window.end_exclusive)
        && (!window.timezone.trim().is_empty())
}

fn expression_proves_time_boundary(
    expression: &NormalizedExpression,
    column: &str,
    operator: &str,
    expected: &str,
) -> bool {
    if matches!(
        expression,
        NormalizedExpression::Binary {
            operator: actual_operator,
            left,
            right,
        } if actual_operator == operator
            && column_matches(left, column)
            && literal_matches(right, expected)
    ) {
        return true;
    }
    match expression {
        NormalizedExpression::Function { arguments, .. } => arguments
            .iter()
            .any(|argument| expression_proves_time_boundary(argument, column, operator, expected)),
        NormalizedExpression::Binary { left, right, .. } => {
            expression_proves_time_boundary(left, column, operator, expected)
                || expression_proves_time_boundary(right, column, operator, expected)
        }
        NormalizedExpression::Unary { expression, .. }
        | NormalizedExpression::IsNull { expression, .. }
        | NormalizedExpression::Cast { expression, .. } => {
            expression_proves_time_boundary(expression, column, operator, expected)
        }
        NormalizedExpression::InList {
            expression, values, ..
        } => {
            expression_proves_time_boundary(expression, column, operator, expected)
                || values
                    .iter()
                    .any(|value| expression_proves_time_boundary(value, column, operator, expected))
        }
        NormalizedExpression::Between {
            expression,
            low,
            high,
            ..
        } => [expression.as_ref(), low.as_ref(), high.as_ref()]
            .into_iter()
            .any(|value| expression_proves_time_boundary(value, column, operator, expected)),
        NormalizedExpression::AtTimeZone {
            timestamp,
            timezone,
        } => {
            expression_proves_time_boundary(timestamp, column, operator, expected)
                || expression_proves_time_boundary(timezone, column, operator, expected)
        }
        NormalizedExpression::Case {
            operand,
            branches,
            else_expression,
        } => {
            operand.as_deref().is_some_and(|value| {
                expression_proves_time_boundary(value, column, operator, expected)
            }) || branches.iter().any(|(condition, result)| {
                expression_proves_time_boundary(condition, column, operator, expected)
                    || expression_proves_time_boundary(result, column, operator, expected)
            }) || else_expression.as_deref().is_some_and(|value| {
                expression_proves_time_boundary(value, column, operator, expected)
            })
        }
        NormalizedExpression::Column { .. }
        | NormalizedExpression::Literal { .. }
        | NormalizedExpression::Unsupported { .. } => false,
    }
}

fn parse_comparison_boundary(value: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .or_else(|| {
            Some(
                chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .ok()?
                    .and_hms_opt(0, 0, 0)?
                    .and_utc()
                    .fixed_offset(),
            )
        })
}

fn comparison_windows_are_aligned(
    baseline: &ComparisonWindow,
    treatment: &ComparisonWindow,
) -> bool {
    if baseline.timezone.trim().is_empty()
        || baseline.timezone != treatment.timezone
        || baseline.business_calendar != treatment.business_calendar
    {
        return false;
    }
    let (Some(baseline_start), Some(baseline_end), Some(treatment_start), Some(treatment_end)) = (
        parse_comparison_boundary(&baseline.start_inclusive),
        parse_comparison_boundary(&baseline.end_exclusive),
        parse_comparison_boundary(&treatment.start_inclusive),
        parse_comparison_boundary(&treatment.end_exclusive),
    ) else {
        return false;
    };
    let baseline_duration = baseline_end - baseline_start;
    let treatment_duration = treatment_end - treatment_start;
    baseline_duration > chrono::Duration::zero()
        && baseline_duration == treatment_duration
        && (baseline_end <= treatment_start || treatment_end <= baseline_start)
}

fn plan_proves_null_policy(plan: &NormalizedRelationalPlan, policy: &NullPolicy) -> ProofStatus {
    let expressions = plan
        .projections
        .iter()
        .map(|value| &value.expression)
        .chain(plan.filters.iter())
        .collect::<Vec<_>>();
    match policy {
        NullPolicy::Ignore => {
            if expressions.iter().any(|expression| {
                matches!(
                    expression,
                    NormalizedExpression::IsNull { negated: true, .. }
                ) || expression_contains_function(expression, "count")
                    || expression_contains_function(expression, "sum")
                    || expression_contains_function(expression, "avg")
            }) {
                ProofStatus::Proved
            } else {
                ProofStatus::Disproved
            }
        }
        NullPolicy::Zero => {
            if expressions.iter().any(|expression| {
                ["coalesce", "ifnull", "nvl"]
                    .iter()
                    .any(|name| expression_contains_function(expression, name))
            }) {
                ProofStatus::Proved
            } else {
                ProofStatus::Disproved
            }
        }
        NullPolicy::SeparateBucket => {
            if expressions.iter().any(|expression| {
                expression_contains_function(expression, "coalesce")
                    && expression_contains_literal(expression, "unknown")
            }) {
                ProofStatus::Proved
            } else {
                ProofStatus::Unknown
            }
        }
        NullPolicy::Fail => ProofStatus::Unknown,
    }
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
    pub normalized_plan: Option<NormalizedRelationalPlan>,
    pub proof_obligations: Vec<ProofObligation>,
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
        _selected_columns: &[String],
        _group_by: &[String],
        joins: &[JoinContract],
        metric_contracts: &[MetricContract],
        sql: &str,
    ) -> SemanticVerification {
        let normalized_plan = parse_normalized_relational_plan(sql).ok();
        let safety = match normalized_plan.as_ref() {
            Some(plan) if plan.unsupported.is_empty() => pass(
                "safe",
                "read-only SQL is inside the provable relational subset",
            ),
            Some(plan) => fail(
                "unsupported_for_semantic_proof",
                &format!(
                    "SQL uses unsupported relational semantics: {}",
                    plan.unsupported.join(", ")
                ),
            ),
            None => fail(
                "unsafe_statement",
                "candidate is not one read-only SELECT/CTE query",
            ),
        };
        let required: BTreeSet<_> = ir.dimensions.iter().map(|d| d.column.as_str()).collect();
        let dimension_is_selected = |dimension: &&str| {
            normalized_plan
                .as_ref()
                .is_some_and(|plan| projection_proves_column(plan, dimension))
        };
        let schema_binding =
            if normalized_plan.is_some() && required.iter().all(dimension_is_selected) {
                pass("schema_bound", "all requested dimensions are selected")
            } else {
                fail("missing_dimension", "a requested dimension is not selected")
            };
        let grain_consistency = match ir.grain {
            Grain::Row => pass("grain_row", "row grain does not require grouping"),
            _ if normalized_plan.as_ref().is_some_and(|plan| {
                required
                    .iter()
                    .all(|dimension| group_proves_column(plan, dimension))
            }) =>
            {
                pass("grain_bound", "grouping matches requested dimensions")
            }
            _ => fail("grain_mismatch", "GROUP BY does not match requested grain"),
        };
        let sql_join_count = normalized_plan.as_ref().map_or(0, |plan| {
            plan.nodes
                .iter()
                .filter(|node| matches!(node, RelationalPlanNode::Join { .. }))
                .count()
        });
        let fanout = joins.iter().any(|join| {
            join.fanout_risk
                || (join.cardinality == JoinCardinality::ManyToMany
                    && join.dedup_strategy.as_deref().is_none_or(str::is_empty))
        });
        let join_cardinality = if sql_join_count > joins.len() {
            fail(
                "join_contract_missing",
                "at least one SQL join has no bound cardinality contract",
            )
        } else if fanout {
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
                normalized_plan
                    .as_ref()
                    .is_none_or(|plan| !plan_proves_filter(plan, filter))
            })
            .count();
        let filter_completeness = if missing_mandatory > 0 {
            fail(
                "filter_unproven",
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
                normalized_plan.as_ref().is_some_and(|plan| {
                    projection_contains_expression(
                        plan,
                        &metric_ir_to_normalized(&contract.expression),
                    )
                })
            }) {
            pass(
                "metric_equivalent",
                "selected metric expressions match their contracts",
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
            CheckResult {
                status: CheckStatus::NotChecked,
                code: "metric_contract_unbound".into(),
                message: "a versioned metric contract is required for semantic proof".into(),
            }
        };
        let denominator_equivalence =
            verify_denominator(ir, &bound_contracts, normalized_plan.as_ref());
        let population_equivalence = if ir.population.subject.trim().is_empty() {
            fail("population_missing", "population subject is empty")
        } else if normalized_plan
            .as_ref()
            .is_none_or(|plan| !plan_proves_population_subject(plan, &ir.population.subject))
        {
            fail(
                "population_subject_unproven",
                "the requested population is not represented by the SQL candidate",
            )
        } else if ir.population.dedup_key.as_deref().is_some_and(|key| {
            normalized_plan
                .as_ref()
                .is_none_or(|plan| !plan_proves_dedup(plan, key))
        }) {
            fail(
                "population_dedup_unproven",
                "the population deduplication policy is not proven by DISTINCT semantics",
            )
        } else if ir.population.exclude_test_users
            && normalized_plan
                .as_ref()
                .is_none_or(|plan| !plan_proves_exclusion(plan, "test"))
        {
            fail(
                "test_population_exclusion_unproven",
                "test-user exclusion is required but is not proven in SQL",
            )
        } else if ir.population.exclude_internal_users
            && normalized_plan
                .as_ref()
                .is_none_or(|plan| !plan_proves_exclusion(plan, "internal"))
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
                let required = parse_filter_expression(rule);
                normalized_plan.as_ref().is_none_or(|plan| {
                    required.as_ref().is_none_or(|required| {
                        !plan_predicates(plan)
                            .iter()
                            .any(|actual| normalized_expression_equivalent(actual, required))
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
            if normalized_plan.as_ref().is_some_and(|plan| {
                plan_proves_time(plan, time) && plan_proves_timezone(plan, time)
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
        let comparison_consistency = verify_comparison(ir, normalized_plan.as_ref());
        let null_semantics = verify_null_semantics(ir, normalized_plan.as_ref());
        let ordering_consistency = verify_ordering(ir, normalized_plan.as_ref());
        let limit_consistency = verify_limit(ir, normalized_plan.as_ref());
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
        let proof_obligations = [
            ("read_only_supported_subset", &safety),
            ("schema_binding", &schema_binding),
            ("metric_equivalence", &metric_equivalence),
            ("denominator_equivalence", &denominator_equivalence),
            ("population_equivalence", &population_equivalence),
            ("grain_consistency", &grain_consistency),
            ("time_and_timezone", &time_consistency),
            ("comparison_period", &comparison_consistency),
            ("join_cardinality", &join_cardinality),
            ("mandatory_filters", &filter_completeness),
            ("null_semantics", &null_semantics),
            ("ordering", &ordering_consistency),
            ("limit", &limit_consistency),
            ("security_scope", &policy_compliance),
        ]
        .into_iter()
        .map(|(name, check)| ProofObligation {
            name: name.into(),
            status: match check.status {
                CheckStatus::Pass => ProofStatus::Proved,
                CheckStatus::Fail => ProofStatus::Disproved,
                CheckStatus::Warn | CheckStatus::NotChecked => ProofStatus::Unknown,
            },
            evidence: vec![check.code.clone(), check.message.clone()],
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
            || filter_completeness.status != CheckStatus::Pass
            || grain_consistency.status == CheckStatus::Fail
            || metric_equivalence.status != CheckStatus::Pass
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
            normalized_plan,
            proof_obligations,
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

fn verify_denominator(
    ir: &AnalyticIntentIR,
    contracts: &[&MetricContract],
    plan: Option<&NormalizedRelationalPlan>,
) -> CheckResult {
    let mut expected = Vec::<NormalizedExpression>::new();
    if let Some(denominator) = ir.denominator.as_ref() {
        let Some(expression) = parse_normalized_expression(&denominator.expression) else {
            return CheckResult {
                status: CheckStatus::NotChecked,
                code: "denominator_contract_unsupported".into(),
                message: "the denominator contract is outside the provable expression subset"
                    .into(),
            };
        };
        expected.push(expression);
    }
    expected.extend(
        contracts
            .iter()
            .filter_map(|contract| contract.denominator.as_ref())
            .map(metric_ir_to_normalized),
    );
    if expected.is_empty() {
        return pass(
            "denominator_not_required",
            "the intent and metric contracts do not require a denominator",
        );
    }
    if plan.is_some_and(|plan| {
        expected
            .iter()
            .all(|denominator| projection_contains_expression(plan, denominator))
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

fn verify_comparison(
    ir: &AnalyticIntentIR,
    plan: Option<&NormalizedRelationalPlan>,
) -> CheckResult {
    let Some(comparison) = ir.comparison.as_ref() else {
        return pass(
            "comparison_not_required",
            "the intent does not require a comparison",
        );
    };
    if plan.is_some_and(|plan| plan_proves_comparison(plan, comparison)) {
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

fn verify_null_semantics(
    ir: &AnalyticIntentIR,
    plan: Option<&NormalizedRelationalPlan>,
) -> CheckResult {
    let Some(plan) = plan else {
        return fail("null_semantics_unproven", "SQL shape could not be parsed");
    };
    match plan_proves_null_policy(plan, &ir.null_policy) {
        ProofStatus::Proved => pass(
            "null_semantics_proved",
            "NULL handling is structurally represented in the relational plan",
        ),
        ProofStatus::Unknown => CheckResult {
            status: CheckStatus::NotChecked,
            code: "null_semantics_unknown".into(),
            message: "NULL handling requires result evidence or an explicit supported expression"
                .into(),
        },
        ProofStatus::Disproved => fail(
            "null_policy_mismatch",
            "the SQL candidate does not implement the requested NULL policy",
        ),
    }
}

fn verify_ordering(ir: &AnalyticIntentIR, plan: Option<&NormalizedRelationalPlan>) -> CheckResult {
    if ir.ordering.is_empty() {
        return pass(
            "ordering_not_required",
            "the intent does not require ordering",
        );
    }
    let Some(plan) = plan else {
        return fail("ordering_unproven", "SQL shape could not be parsed");
    };
    if ir.ordering.len() != plan.order_by.len() {
        return fail(
            "ordering_mismatch",
            "ORDER BY expression count differs from the canonical intent",
        );
    }
    if ir
        .ordering
        .iter()
        .zip(&plan.order_by)
        .all(|(expected, actual)| {
            let expected_expression = parse_normalized_expression(&expected.expression)
                .unwrap_or_else(|| NormalizedExpression::Column {
                    relation: None,
                    name: normalize_identifier(&expected.expression),
                });
            (normalized_expression_equivalent(&expected_expression, &actual.0)
                || actual
                    .0
                    .column_name()
                    .is_some_and(|column| column == normalize_identifier(&expected.expression)))
                && expected.descending != actual.1
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

fn verify_limit(ir: &AnalyticIntentIR, plan: Option<&NormalizedRelationalPlan>) -> CheckResult {
    let Some(expected) = ir.limit else {
        return pass(
            "limit_not_required",
            "the intent does not require a row limit",
        );
    };
    let Some(actual) = plan.and_then(|plan| plan.limit) else {
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

fn pass(code: &str, message: &str) -> CheckResult {
    CheckResult {
        status: CheckStatus::Pass,
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
    fn verifier_does_not_release_an_unbound_metric_contract() {
        let result = SemanticVerifier::default().verify(
            &ir(),
            &["business_date".into(), "COUNT(*)".into()],
            &["business_date".into()],
            &[],
            "SELECT business_date, COUNT(*) FROM orders WHERE (created_at AT TIME ZONE 'Asia/Shanghai') >= DATE '2026-01-01' AND (created_at AT TIME ZONE 'Asia/Shanghai') < DATE '2026-01-02' GROUP BY business_date",
        );
        assert_eq!(result.metric_equivalence.status, CheckStatus::NotChecked);
        assert_eq!(result.metric_equivalence.code, "metric_contract_unbound");
        assert_eq!(
            result.release_decision,
            QueryReleaseDecision::NeedsClarification
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
            baseline_window: None,
            treatment_window: None,
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
    fn temporal_comparison_requires_equal_aligned_windows_and_all_sql_boundaries() {
        let mut input = ir();
        input.time = None;
        input.objective = AnalyticObjective::Comparison;
        input.comparison = Some(ComparisonSpec {
            baseline: "previous period".into(),
            treatment: "current period".into(),
            method: "period_over_period".into(),
            baseline_window: Some(ComparisonWindow {
                column: "business_date".into(),
                start_inclusive: "2026-01-01".into(),
                end_exclusive: "2026-01-08".into(),
                timezone: "UTC".into(),
                business_calendar: None,
            }),
            treatment_window: Some(ComparisonWindow {
                column: "business_date".into(),
                start_inclusive: "2026-01-08".into(),
                end_exclusive: "2026-01-15".into(),
                timezone: "UTC".into(),
                business_calendar: None,
            }),
        });
        let valid_sql = "SELECT
            SUM(CASE WHEN business_date >= DATE '2026-01-08' AND business_date < DATE '2026-01-15' THEN revenue ELSE 0 END)
            - SUM(CASE WHEN business_date >= DATE '2026-01-01' AND business_date < DATE '2026-01-08' THEN revenue ELSE 0 END) AS delta
            FROM orders";
        let valid = SemanticVerifier::default().verify(&input, &[], &[], &[], valid_sql);
        assert_eq!(valid.comparison_consistency.status, CheckStatus::Pass);

        let missing_boundary = SemanticVerifier::default().verify(
            &input,
            &[],
            &[],
            &[],
            "SELECT SUM(CASE WHEN business_date >= DATE '2026-01-08' AND business_date < DATE '2026-01-15' THEN revenue ELSE 0 END) - SUM(revenue) FROM orders",
        );
        assert_eq!(
            missing_boundary.comparison_consistency.status,
            CheckStatus::Fail
        );

        input
            .comparison
            .as_mut()
            .and_then(|comparison| comparison.treatment_window.as_mut())
            .expect("treatment window")
            .end_exclusive = "2026-01-16".into();
        let unequal = SemanticVerifier::default().verify(&input, &[], &[], &[], valid_sql);
        assert_eq!(unequal.comparison_consistency.status, CheckStatus::Fail);
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
        let proves = |sql: &str, time: &TimeSemantics| {
            let plan = compile_normalized_relational_plan(sql).unwrap();
            plan_proves_time(&plan, time)
        };
        assert!(proves(
            "SELECT COUNT(*) FROM orders WHERE business_date = DATE '2026-08-15'",
            &one_day
        ));
        assert!(proves(
            "SELECT COUNT(*) FROM orders WHERE business_date >= DATE '2026-08-15' AND business_date < DATE '2026-08-16'",
            &one_day
        ));
        assert!(!proves(
            "SELECT COUNT(*) FROM orders WHERE business_date >= DATE '2026-08-15' AND business_date <= DATE '2026-08-16'",
            &one_day
        ));

        let multi_day = TimeSemantics {
            end_exclusive: "2026-08-22".into(),
            ..one_day
        };
        assert!(!proves(
            "SELECT COUNT(*) FROM orders WHERE business_date = DATE '2026-08-15'",
            &multi_day
        ));
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
    fn adversarial_relational_proof_rejects_business_semantic_drift() {
        let mut input = ir();
        input.dimensions.clear();
        input.grain = Grain::Row;
        input.population = PopulationDefinition {
            subject: "query_rows".into(),
            dedup_key: None,
            exclude_test_users: false,
            exclude_internal_users: false,
            valid_record_rule: None,
        };
        input.time = None;
        input.null_policy = NullPolicy::Ignore;
        input.filters = vec![SemanticFilter::Equals {
            field: "tenant_id".into(),
            value: "tenant-a".into(),
        }];
        input.metrics = vec![MetricRef {
            id: "roi".into(),
            version: Some(7),
            display_name: "ROI".into(),
        }];
        input.denominator = Some(DenominatorSpec {
            expression: "SUM(cost)".into(),
            population: input.population.clone(),
        });
        let contract = MetricContract {
            id: "roi".into(),
            version: 7,
            names: vec!["ROI".into()],
            expression: MetricExpressionIR::Ratio {
                numerator: Box::new(MetricExpressionIR::Aggregate {
                    function: "SUM".into(),
                    expression: Box::new(MetricExpressionIR::Column("revenue".into())),
                    distinct: false,
                }),
                denominator: Box::new(MetricExpressionIR::Aggregate {
                    function: "SUM".into(),
                    expression: Box::new(MetricExpressionIR::Column("cost".into())),
                    distinct: false,
                }),
            },
            denominator: Some(MetricExpressionIR::Aggregate {
                function: "SUM".into(),
                expression: Box::new(MetricExpressionIR::Column("cost".into())),
                distinct: false,
            }),
            population: input.population.clone(),
            default_grain: Grain::Row,
            allowed_grains: vec![Grain::Row],
            time_column: "business_date".into(),
            timezone: "Asia/Shanghai".into(),
            mandatory_filters: input.filters.clone(),
            join_contracts: vec![],
            invariants: vec![],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: Some("finance".into()),
            evidence_refs: vec!["metric-contract:roi:7".into()],
        };
        let verify = |sql: &str| {
            SemanticVerifier::default().verify_with_metric_contracts(
                &input,
                &[],
                &[],
                &[],
                std::slice::from_ref(&contract),
                sql,
            )
        };

        let correct = verify(
            "SELECT SUM(revenue) / SUM(cost) AS roi FROM app_daily WHERE tenant_id = 'tenant-a'",
        );
        assert_eq!(correct.metric_equivalence.status, CheckStatus::Pass);
        assert_eq!(correct.denominator_equivalence.status, CheckStatus::Pass);
        assert_eq!(correct.filter_completeness.status, CheckStatus::Pass);
        assert!(correct
            .proof_obligations
            .iter()
            .all(|obligation| obligation.status != ProofStatus::Disproved));

        let wrong_denominator = verify(
            "SELECT SUM(revenue) / SUM(active_users) AS roi FROM app_daily WHERE tenant_id = 'tenant-a'",
        );
        assert_eq!(
            wrong_denominator.metric_equivalence.status,
            CheckStatus::Fail
        );
        assert_eq!(
            wrong_denominator.denominator_equivalence.status,
            CheckStatus::Fail
        );
        assert_ne!(
            wrong_denominator.release_decision,
            QueryReleaseDecision::Release
        );

        let comment_only_rls = verify(
            "SELECT SUM(revenue) / SUM(cost) AS roi FROM app_daily /* tenant_id = 'tenant-a' */",
        );
        assert_eq!(
            comment_only_rls.filter_completeness.status,
            CheckStatus::Fail
        );
        assert_ne!(
            comment_only_rls.release_decision,
            QueryReleaseDecision::Release
        );

        let join_without_contract = verify(
            "SELECT SUM(a.revenue) / SUM(a.cost) AS roi FROM app_daily a JOIN users u ON a.user_id = u.id WHERE a.tenant_id = 'tenant-a'",
        );
        assert_eq!(
            join_without_contract.join_cardinality.status,
            CheckStatus::Fail
        );
        assert_eq!(
            join_without_contract.release_decision,
            QueryReleaseDecision::Reject
        );
    }

    #[test]
    fn unsupported_udf_and_correlated_subquery_fail_closed() {
        assert!(matches!(
            compile_normalized_relational_plan("SELECT tenant_magic(revenue) FROM app_daily"),
            Err(RelationalPlanError::UnsupportedForSemanticProof(_))
        ));
        assert!(matches!(
            compile_normalized_relational_plan(
                "SELECT user_id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.user_id)"
            ),
            Err(RelationalPlanError::UnsupportedForSemanticProof(_))
        ));
    }

    #[test]
    fn adversarial_population_time_comparison_null_and_contract_drift_fail_closed() {
        let mut population = ir();
        population.objective = AnalyticObjective::Lookup;
        population.metrics.clear();
        population.dimensions.clear();
        population.grain = Grain::Row;
        population.time = None;
        population.population = PopulationDefinition {
            subject: "order".into(),
            dedup_key: Some("order_id".into()),
            exclude_test_users: false,
            exclude_internal_users: false,
            valid_record_rule: None,
        };
        let wrong_population = SemanticVerifier::default().verify(
            &population,
            &[],
            &[],
            &[],
            "SELECT user_id FROM users",
        );
        assert_eq!(
            wrong_population.population_equivalence.status,
            CheckStatus::Fail
        );
        assert_ne!(
            wrong_population.release_decision,
            QueryReleaseDecision::Release
        );

        let duplicate_input = AnalyticIntentIR {
            population: PopulationDefinition {
                subject: "user".into(),
                dedup_key: Some("user_id".into()),
                exclude_test_users: false,
                exclude_internal_users: false,
                valid_record_rule: None,
            },
            ..population.clone()
        };
        let duplicate_population = SemanticVerifier::default().verify(
            &duplicate_input,
            &[],
            &[],
            &[],
            "SELECT user_id FROM users",
        );
        assert_eq!(
            duplicate_population.population_equivalence.status,
            CheckStatus::Fail
        );

        let mut timezone = ir();
        timezone.objective = AnalyticObjective::Lookup;
        timezone.metrics.clear();
        timezone.dimensions.clear();
        timezone.grain = Grain::Row;
        timezone.population = PopulationDefinition {
            subject: "order".into(),
            dedup_key: None,
            exclude_test_users: false,
            exclude_internal_users: false,
            valid_record_rule: None,
        };
        let wrong_timezone = SemanticVerifier::default().verify(
            &timezone,
            &[],
            &[],
            &[],
            "SELECT order_id FROM orders WHERE (created_at AT TIME ZONE 'UTC') >= DATE '2026-01-01' AND (created_at AT TIME ZONE 'UTC') < DATE '2026-01-02'",
        );
        assert_eq!(wrong_timezone.time_consistency.status, CheckStatus::Fail);

        let mut comparison = population.clone();
        comparison.objective = AnalyticObjective::Comparison;
        comparison.time = Some(TimeSemantics {
            column: "business_date".into(),
            timezone: "UTC".into(),
            start_inclusive: "2026-01-01".into(),
            end_exclusive: "2026-01-02".into(),
            business_calendar: None,
            as_of: None,
        });
        comparison.comparison = Some(ComparisonSpec {
            baseline: "control".into(),
            treatment: "treatment".into(),
            method: "difference".into(),
            baseline_window: None,
            treatment_window: None,
        });
        let wrong_comparison = SemanticVerifier::default().verify(
            &comparison,
            &[],
            &[],
            &[],
            "SELECT SUM(CASE WHEN cohort = 'old' THEN revenue ELSE 0 END) - SUM(CASE WHEN cohort = 'new' THEN revenue ELSE 0 END) FROM orders WHERE business_date = DATE '2026-01-01'",
        );
        assert_eq!(
            wrong_comparison.comparison_consistency.status,
            CheckStatus::Fail
        );

        let mut zero_null = population.clone();
        zero_null.null_policy = NullPolicy::Zero;
        let wrong_null = SemanticVerifier::default().verify(
            &zero_null,
            &[],
            &[],
            &[],
            "SELECT revenue FROM orders",
        );
        assert_eq!(wrong_null.null_semantics.status, CheckStatus::Fail);

        let mut versioned = population;
        versioned.objective = AnalyticObjective::Aggregate;
        versioned.metrics = vec![MetricRef {
            id: "revenue".into(),
            version: Some(2),
            display_name: "Revenue".into(),
        }];
        let stale_contract = MetricContract {
            id: "revenue".into(),
            version: 1,
            names: vec!["Revenue".into()],
            expression: MetricExpressionIR::Aggregate {
                function: "SUM".into(),
                expression: Box::new(MetricExpressionIR::Column("revenue".into())),
                distinct: false,
            },
            denominator: None,
            population: versioned.population.clone(),
            default_grain: Grain::Row,
            allowed_grains: vec![Grain::Row],
            time_column: "business_date".into(),
            timezone: "UTC".into(),
            mandatory_filters: vec![],
            join_contracts: vec![],
            invariants: vec![],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: Some("finance".into()),
            evidence_refs: vec!["metric-contract:revenue:1".into()],
        };
        let stale = SemanticVerifier::default().verify_with_metric_contracts(
            &versioned,
            &[],
            &[],
            &[],
            &[stale_contract],
            "SELECT SUM(revenue) FROM orders",
        );
        assert_eq!(stale.metric_equivalence.status, CheckStatus::NotChecked);
        assert_ne!(stale.release_decision, QueryReleaseDecision::Release);
    }

    #[test]
    fn qualified_column_lineage_rejects_same_name_metric_and_repair_drift() {
        let mut input = ir();
        input.dimensions.clear();
        input.grain = Grain::Row;
        input.time = None;
        input.population = PopulationDefinition {
            subject: "query_rows".into(),
            dedup_key: None,
            exclude_test_users: false,
            exclude_internal_users: false,
            valid_record_rule: None,
        };
        input.metrics = vec![MetricRef {
            id: "gross_amount".into(),
            version: Some(3),
            display_name: "Gross amount".into(),
        }];
        let contract = MetricContract {
            id: "gross_amount".into(),
            version: 3,
            names: vec!["Gross amount".into()],
            expression: MetricExpressionIR::Aggregate {
                function: "SUM".into(),
                expression: Box::new(MetricExpressionIR::Column("orders.amount".into())),
                distinct: false,
            },
            denominator: None,
            population: input.population.clone(),
            default_grain: Grain::Row,
            allowed_grains: vec![Grain::Row],
            time_column: "business_date".into(),
            timezone: "UTC".into(),
            mandatory_filters: vec![],
            join_contracts: vec!["orders-refunds".into()],
            invariants: vec![],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: Some("finance".into()),
            evidence_refs: vec!["metric-contract:gross_amount:3".into()],
        };
        let join = JoinContract {
            id: "orders-refunds".into(),
            left_table: "orders".into(),
            right_table: "refunds".into(),
            left_keys: vec!["order_id".into()],
            right_keys: vec!["order_id".into()],
            cardinality: JoinCardinality::OneToOne,
            temporal_condition: None,
            nullable: false,
            dedup_strategy: None,
            allowed_grains: vec![Grain::Row],
            fanout_risk: false,
        };
        let verify = |sql: &str| {
            SemanticVerifier::default().verify_with_metric_contracts(
                &input,
                &[],
                &[],
                std::slice::from_ref(&join),
                std::slice::from_ref(&contract),
                sql,
            )
        };
        let original =
            verify("SELECT SUM(o.amount) FROM orders o JOIN refunds r ON o.order_id = r.order_id");
        assert_eq!(original.metric_equivalence.status, CheckStatus::Pass);

        let drifted_repair =
            verify("SELECT SUM(r.amount) FROM orders o JOIN refunds r ON o.order_id = r.order_id");
        assert_eq!(drifted_repair.metric_equivalence.status, CheckStatus::Fail);
        assert_ne!(
            drifted_repair.release_decision,
            QueryReleaseDecision::Release
        );

        let ambiguous =
            verify("SELECT SUM(amount) FROM orders o JOIN refunds r ON o.order_id = r.order_id");
        assert_eq!(ambiguous.safety.status, CheckStatus::Fail);
        assert_eq!(ambiguous.release_decision, QueryReleaseDecision::Reject);
    }

    #[test]
    fn metric_contract_rejects_thousand_fold_unit_conversion_error() {
        let mut input = ir();
        input.dimensions.clear();
        input.grain = Grain::Row;
        input.time = None;
        input.population = PopulationDefinition {
            subject: "query_rows".into(),
            dedup_key: None,
            exclude_test_users: false,
            exclude_internal_users: false,
            valid_record_rule: None,
        };
        input.metrics = vec![MetricRef {
            id: "revenue_base_units".into(),
            version: Some(4),
            display_name: "Revenue".into(),
        }];
        let contract = MetricContract {
            id: "revenue_base_units".into(),
            version: 4,
            names: vec!["Revenue".into()],
            expression: MetricExpressionIR::Ratio {
                numerator: Box::new(MetricExpressionIR::Aggregate {
                    function: "SUM".into(),
                    expression: Box::new(MetricExpressionIR::Column("revenue_milliunits".into())),
                    distinct: false,
                }),
                denominator: Box::new(MetricExpressionIR::Literal("1000".into())),
            },
            denominator: Some(MetricExpressionIR::Literal("1000".into())),
            population: input.population.clone(),
            default_grain: Grain::Row,
            allowed_grains: vec![Grain::Row],
            time_column: "business_date".into(),
            timezone: "UTC".into(),
            mandatory_filters: vec![],
            join_contracts: vec![],
            invariants: vec![],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: Some("finance".into()),
            evidence_refs: vec!["metric-contract:revenue_base_units:4".into()],
        };
        let correct = SemanticVerifier::default().verify_with_metric_contracts(
            &input,
            &[],
            &[],
            &[],
            std::slice::from_ref(&contract),
            "SELECT SUM(revenue_milliunits) / 1000 FROM payments",
        );
        assert_eq!(correct.metric_equivalence.status, CheckStatus::Pass);

        let thousand_fold_error = SemanticVerifier::default().verify_with_metric_contracts(
            &input,
            &[],
            &[],
            &[],
            std::slice::from_ref(&contract),
            "SELECT SUM(revenue_milliunits) FROM payments",
        );
        assert_eq!(
            thousand_fold_error.metric_equivalence.status,
            CheckStatus::Fail
        );
        assert_ne!(
            thousand_fold_error.release_decision,
            QueryReleaseDecision::Release
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
