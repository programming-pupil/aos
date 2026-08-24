//! Safe execution of the AOS MongoDB SQL subset.
//!
//! AOS keeps SQL as the user-visible NL2SQL language for every datasource.
//! MongoDB queries are parsed into an AST and translated to an aggregation
//! pipeline. Unsupported constructs fail closed instead of being approximated.

use std::str::FromStr;
use std::time::Duration;

use chrono::{Datelike, TimeZone, Utc};
use mongodb::bson::{doc, Bson, Document};
use nl2sql_domain::datasource_config::{build_mongodb_uri, MongoConfig};
use sqlparser::ast::{
    BinaryOperator, DuplicateTreatment, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArguments, GroupByExpr, LimitClause, ObjectNamePart, OrderByKind, Query, Select,
    SelectItem, SetExpr, Statement, TableFactor, UnaryOperator, Value,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

const DEFAULT_MAX_ROWS: usize = 1_000;

#[derive(Debug, Clone)]
pub(crate) struct MongoQueryPlan {
    pub collection: String,
    pub pipeline: Vec<Document>,
    pub count_pipeline: Vec<Document>,
    pub columns: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct MongoQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub total_rows: i64,
}

struct TranslationContext<'a> {
    collection: &'a str,
    table_alias: Option<&'a str>,
    query_timestamp: mongodb::bson::DateTime,
    query_date: mongodb::bson::DateTime,
    query_date_string: String,
}

#[derive(Debug)]
struct AggregateProjection {
    alias: String,
    accumulator_field: String,
    accumulator: Bson,
    final_expression: Bson,
}

fn invalid(message: impl Into<String>) -> String {
    format!("MongoDB SQL subset: {}", message.into())
}

fn literal_to_bson(value: &Value) -> Result<Bson, String> {
    match value {
        Value::Number(raw, _) => raw
            .parse::<i64>()
            .map(Bson::Int64)
            .or_else(|_| raw.parse::<f64>().map(Bson::Double))
            .map_err(|_| invalid(format!("invalid numeric literal `{raw}`"))),
        Value::SingleQuotedString(value)
        | Value::DoubleQuotedString(value)
        | Value::EscapedStringLiteral(value)
        | Value::UnicodeStringLiteral(value)
        | Value::NationalStringLiteral(value) => Ok(Bson::String(value.clone())),
        Value::Boolean(value) => Ok(Bson::Boolean(*value)),
        Value::Null => Ok(Bson::Null),
        _ => Err(invalid("unsupported literal type")),
    }
}

fn field_name(expr: &Expr, ctx: &TranslationContext<'_>) -> Result<String, String> {
    let parts = match expr {
        Expr::Identifier(ident) => vec![ident.value.clone()],
        Expr::CompoundIdentifier(idents) => {
            idents.iter().map(|ident| ident.value.clone()).collect()
        }
        Expr::Nested(inner) => return field_name(inner, ctx),
        _ => return Err(invalid(format!("expected a field reference, got `{expr}`"))),
    };
    let mut start = 0usize;
    if parts.len() > 1
        && (ctx
            .table_alias
            .is_some_and(|alias| parts[0].eq_ignore_ascii_case(alias))
            || parts[0].eq_ignore_ascii_case(ctx.collection))
    {
        start = 1;
    }
    let name = parts[start..].join(".");
    if name.is_empty() || name.starts_with('$') || name.contains('\0') {
        return Err(invalid("invalid MongoDB field name"));
    }
    Ok(name)
}

fn function_args(function: &Function) -> Result<Vec<&FunctionArgExpr>, String> {
    if function.filter.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
    {
        return Err(invalid(format!(
            "function modifiers are not supported for `{}`",
            function.name
        )));
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err(invalid(format!(
            "invalid arguments for `{}`",
            function.name
        )));
    };
    if !args.clauses.is_empty() {
        return Err(invalid(format!(
            "argument clauses are not supported for `{}`",
            function.name
        )));
    }
    args.args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(expr) => Ok(expr),
            FunctionArg::Named { .. } | FunctionArg::ExprNamed { .. } => {
                Err(invalid("named function arguments are not supported"))
            }
        })
        .collect()
}

fn expression_to_bson(expr: &Expr, ctx: &TranslationContext<'_>) -> Result<Bson, String> {
    match expr {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Nested(_) => {
            if let Expr::Nested(inner) = expr {
                return expression_to_bson(inner, ctx);
            }
            Ok(Bson::String(format!("${}", field_name(expr, ctx)?)))
        }
        Expr::Value(value) => literal_to_bson(value),
        Expr::TypedString(value) => literal_to_bson(&value.value),
        Expr::UnaryOp { op, expr } => {
            let value = expression_to_bson(expr, ctx)?;
            match op {
                UnaryOperator::Plus => Ok(value),
                UnaryOperator::Minus => Ok(Bson::Document(doc! { "$multiply": [-1, value] })),
                UnaryOperator::Not => Ok(Bson::Document(doc! { "$not": [value] })),
                _ => Err(invalid(format!("unsupported unary operator `{op}`"))),
            }
        }
        Expr::BinaryOp { left, op, right } => {
            let left = expression_to_bson(left, ctx)?;
            let right = expression_to_bson(right, ctx)?;
            let mongo_op = match op {
                BinaryOperator::Plus => "$add",
                BinaryOperator::Minus => "$subtract",
                BinaryOperator::Multiply => "$multiply",
                BinaryOperator::Divide => "$divide",
                BinaryOperator::Modulo => "$mod",
                BinaryOperator::Eq => "$eq",
                BinaryOperator::NotEq => "$ne",
                BinaryOperator::Gt => "$gt",
                BinaryOperator::GtEq => "$gte",
                BinaryOperator::Lt => "$lt",
                BinaryOperator::LtEq => "$lte",
                BinaryOperator::And => "$and",
                BinaryOperator::Or => "$or",
                _ => return Err(invalid(format!("unsupported binary operator `{op}`"))),
            };
            Ok(Bson::Document(doc! { mongo_op: [left, right] }))
        }
        Expr::Cast {
            expr, data_type, ..
        } => cast_expression_to_bson(expr, data_type, ctx),
        Expr::Function(function) => scalar_function_to_bson(function, ctx),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let mut branches = Vec::with_capacity(conditions.len());
            for branch in conditions {
                let case = if let Some(operand) = operand {
                    Bson::Document(doc! {
                        "$eq": [expression_to_bson(operand, ctx)?, expression_to_bson(&branch.condition, ctx)?]
                    })
                } else {
                    expression_to_bson(&branch.condition, ctx)?
                };
                branches.push(Bson::Document(doc! {
                    "case": case,
                    "then": expression_to_bson(&branch.result, ctx)?,
                }));
            }
            Ok(Bson::Document(doc! {
                "$switch": {
                    "branches": branches,
                    "default": match else_result {
                        Some(expr) => expression_to_bson(expr, ctx)?,
                        None => Bson::Null,
                    }
                }
            }))
        }
        _ => Err(invalid(format!("unsupported expression `{expr}`"))),
    }
}

fn cast_expression_to_bson(
    expr: &Expr,
    data_type: &sqlparser::ast::DataType,
    ctx: &TranslationContext<'_>,
) -> Result<Bson, String> {
    let target = data_type.to_string().to_ascii_uppercase();
    if (target.starts_with("VARCHAR")
        || target.starts_with("CHAR")
        || matches!(target.as_str(), "STRING" | "TEXT"))
        && expr.to_string().eq_ignore_ascii_case("CURRENT_DATE")
    {
        return Ok(Bson::String(ctx.query_date_string.clone()));
    }
    cast_value_to_bson(expression_to_bson(expr, ctx)?, data_type)
}

fn cast_value_to_bson(input: Bson, data_type: &sqlparser::ast::DataType) -> Result<Bson, String> {
    let target = data_type.to_string().to_ascii_uppercase();
    let mongo_type = if target.starts_with("VARCHAR")
        || target.starts_with("CHAR")
        || matches!(target.as_str(), "STRING" | "TEXT")
    {
        "string"
    } else if target == "DATE" || target.starts_with("DATETIME") || target.starts_with("TIMESTAMP")
    {
        "date"
    } else if target.starts_with("INT")
        || target.starts_with("INTEGER")
        || target.starts_with("BIGINT")
        || target.starts_with("SMALLINT")
        || target.starts_with("TINYINT")
    {
        "long"
    } else if target.starts_with("DOUBLE")
        || target.starts_with("FLOAT")
        || target.starts_with("REAL")
        || target.starts_with("DECIMAL")
        || target.starts_with("NUMERIC")
    {
        "double"
    } else if matches!(target.as_str(), "BOOL" | "BOOLEAN") {
        "bool"
    } else {
        return Err(invalid(format!(
            "unsupported CAST target type `{data_type}`"
        )));
    };
    Ok(Bson::Document(doc! {
        "$convert": {
            "input": input,
            "to": mongo_type,
        }
    }))
}

fn scalar_function_to_bson(
    function: &Function,
    ctx: &TranslationContext<'_>,
) -> Result<Bson, String> {
    let name = function.name.to_string().to_ascii_uppercase();
    if matches!(name.as_str(), "CURRENT_TIMESTAMP" | "NOW") {
        match &function.args {
            FunctionArguments::None => {}
            FunctionArguments::List(args) if args.args.is_empty() && args.clauses.is_empty() => {}
            _ => return Err(invalid(format!("`{name}` does not accept arguments"))),
        }
        return Ok(Bson::DateTime(ctx.query_timestamp));
    }
    if name == "CURRENT_DATE" {
        match &function.args {
            FunctionArguments::None => {}
            FunctionArguments::List(args) if args.args.is_empty() && args.clauses.is_empty() => {}
            _ => return Err(invalid("`CURRENT_DATE` does not accept arguments")),
        }
        return Ok(Bson::DateTime(ctx.query_date));
    }
    let args = function_args(function)?;
    let expr_args = args
        .iter()
        .map(|arg| match arg {
            FunctionArgExpr::Expr(expr) => expression_to_bson(expr, ctx),
            _ => Err(invalid(format!("wildcard is invalid for `{name}`"))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    scalar_function_from_args(&name, expr_args)
}

fn scalar_function_from_args(name: &str, expr_args: Vec<Bson>) -> Result<Bson, String> {
    match name {
        "LOWER" if expr_args.len() == 1 => {
            Ok(Bson::Document(doc! { "$toLower": expr_args[0].clone() }))
        }
        "UPPER" if expr_args.len() == 1 => {
            Ok(Bson::Document(doc! { "$toUpper": expr_args[0].clone() }))
        }
        "ABS" if expr_args.len() == 1 => Ok(Bson::Document(doc! { "$abs": expr_args[0].clone() })),
        "ROUND" if (1..=2).contains(&expr_args.len()) => {
            Ok(Bson::Document(doc! { "$round": expr_args }))
        }
        "COALESCE" | "IFNULL" if expr_args.len() >= 2 => {
            let mut iter = expr_args.into_iter().rev();
            let mut result = iter.next().expect("checked non-empty");
            for value in iter {
                result = Bson::Document(doc! { "$ifNull": [value, result] });
            }
            Ok(result)
        }
        "DATE" if expr_args.len() == 1 => Ok(Bson::Document(doc! {
            "$dateToString": { "format": "%Y-%m-%d", "date": expr_args[0].clone() }
        })),
        "YEAR" if expr_args.len() == 1 => {
            Ok(Bson::Document(doc! { "$year": expr_args[0].clone() }))
        }
        "MONTH" if expr_args.len() == 1 => {
            Ok(Bson::Document(doc! { "$month": expr_args[0].clone() }))
        }
        "DAY" | "DAYOFMONTH" if expr_args.len() == 1 => {
            Ok(Bson::Document(doc! { "$dayOfMonth": expr_args[0].clone() }))
        }
        "OBJECT_ID" | "OBJECTID" if expr_args.len() == 1 => {
            let Bson::String(value) = &expr_args[0] else {
                return Err(invalid("OBJECT_ID requires a string literal"));
            };
            mongodb::bson::oid::ObjectId::from_str(value)
                .map(Bson::ObjectId)
                .map_err(|error| invalid(format!("invalid ObjectId: {error}")))
        }
        "ISO_DATE" | "ISODATE" if expr_args.len() == 1 => {
            let Bson::String(value) = &expr_args[0] else {
                return Err(invalid("ISO_DATE requires an RFC 3339 string literal"));
            };
            mongodb::bson::DateTime::parse_rfc3339_str(value)
                .map(Bson::DateTime)
                .map_err(|error| invalid(format!("invalid RFC 3339 date: {error}")))
        }
        _ => Err(invalid(format!("unsupported scalar function `{name}`"))),
    }
}

fn comparison_document(field: String, operator: &str, value: Bson) -> Document {
    if operator == "$eq" {
        doc! { field: value }
    } else {
        doc! { field: { operator: value } }
    }
}

fn invert_comparison(operator: &BinaryOperator) -> Option<&'static str> {
    match operator {
        BinaryOperator::Eq => Some("$eq"),
        BinaryOperator::NotEq => Some("$ne"),
        BinaryOperator::Gt => Some("$lt"),
        BinaryOperator::GtEq => Some("$lte"),
        BinaryOperator::Lt => Some("$gt"),
        BinaryOperator::LtEq => Some("$gte"),
        _ => None,
    }
}

fn direct_comparison_operator(operator: &BinaryOperator) -> Option<&'static str> {
    match operator {
        BinaryOperator::Eq => Some("$eq"),
        BinaryOperator::NotEq => Some("$ne"),
        BinaryOperator::Gt => Some("$gt"),
        BinaryOperator::GtEq => Some("$gte"),
        BinaryOperator::Lt => Some("$lt"),
        BinaryOperator::LtEq => Some("$lte"),
        _ => None,
    }
}

fn like_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    let mut literal = String::new();
    for ch in pattern.chars() {
        match ch {
            '%' | '_' => {
                out.push_str(&regex::escape(&literal));
                literal.clear();
                out.push_str(if ch == '%' { ".*" } else { "." });
            }
            other => literal.push(other),
        }
    }
    out.push_str(&regex::escape(&literal));
    out.push('$');
    out
}

fn filter_to_document(expr: &Expr, ctx: &TranslationContext<'_>) -> Result<Document, String> {
    match expr {
        Expr::Nested(inner) => filter_to_document(inner, ctx),
        Expr::BinaryOp { left, op, right }
            if matches!(op, BinaryOperator::And | BinaryOperator::Or) =>
        {
            let mongo_op = if matches!(op, BinaryOperator::And) {
                "$and"
            } else {
                "$or"
            };
            Ok(doc! { mongo_op: [
                Bson::Document(filter_to_document(left, ctx)?),
                Bson::Document(filter_to_document(right, ctx)?),
            ] })
        }
        Expr::BinaryOp { left, op, right } => {
            if let (Ok(field), Expr::Value(value)) = (field_name(left, ctx), right.as_ref()) {
                if let Some(operator) = direct_comparison_operator(op) {
                    return Ok(comparison_document(
                        field,
                        operator,
                        literal_to_bson(value)?,
                    ));
                }
            }
            if let (Expr::Value(value), Ok(field)) = (left.as_ref(), field_name(right, ctx)) {
                if let Some(operator) = invert_comparison(op) {
                    return Ok(comparison_document(
                        field,
                        operator,
                        literal_to_bson(value)?,
                    ));
                }
            }
            Ok(doc! { "$expr": expression_to_bson(expr, ctx)? })
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let field = field_name(expr, ctx)?;
            let values = list
                .iter()
                .map(|value| expression_to_bson(value, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            let op = if *negated { "$nin" } else { "$in" };
            Ok(doc! { field: { op: values } })
        }
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => {
            let field = field_name(expr, ctx)?;
            let low = expression_to_bson(low, ctx)?;
            let high = expression_to_bson(high, ctx)?;
            if *negated {
                Ok(doc! { "$or": [
                    Bson::Document(doc! { field.clone(): { "$lt": low } }),
                    Bson::Document(doc! { field: { "$gt": high } }),
                ] })
            } else {
                Ok(doc! { field: { "$gte": low, "$lte": high } })
            }
        }
        Expr::IsNull(expr) => Ok(doc! { field_name(expr, ctx)?: Bson::Null }),
        Expr::IsNotNull(expr) => Ok(doc! { field_name(expr, ctx)?: { "$ne": Bson::Null } }),
        Expr::Like {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        }
        | Expr::ILike {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        } => {
            if *any || escape_char.is_some() {
                return Err(invalid("LIKE ANY and LIKE ESCAPE are not supported"));
            }
            let field = field_name(expr, ctx)?;
            let Expr::Value(pattern) = pattern.as_ref() else {
                return Err(invalid("LIKE pattern must be a string literal"));
            };
            let Value::SingleQuotedString(pattern) = &pattern.value else {
                return Err(invalid("LIKE pattern must be a string literal"));
            };
            let regex = Bson::RegularExpression(mongodb::bson::Regex {
                pattern: like_regex(pattern),
                options: "i".to_string(),
            });
            if *negated {
                Ok(doc! { field: { "$not": regex } })
            } else {
                Ok(doc! { field: regex })
            }
        }
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => Ok(doc! { "$nor": [Bson::Document(filter_to_document(expr, ctx)?)] }),
        _ => Ok(doc! { "$expr": expression_to_bson(expr, ctx)? }),
    }
}

fn is_aggregate_function(function: &Function) -> bool {
    matches!(
        function.name.to_string().to_ascii_uppercase().as_str(),
        "COUNT" | "SUM" | "AVG" | "MIN" | "MAX"
    )
}

fn contains_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::Function(function) => {
            is_aggregate_function(function)
                || function_args(function).is_ok_and(|args| {
                    args.into_iter().any(|arg| match arg {
                        FunctionArgExpr::Expr(expr) => contains_aggregate(expr),
                        _ => false,
                    })
                })
        }
        Expr::BinaryOp { left, right, .. } => contains_aggregate(left) || contains_aggregate(right),
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::Cast { expr, .. }
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr) => contains_aggregate(expr),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            operand.as_deref().is_some_and(contains_aggregate)
                || conditions.iter().any(|branch| {
                    contains_aggregate(&branch.condition) || contains_aggregate(&branch.result)
                })
                || else_result.as_deref().is_some_and(contains_aggregate)
        }
        _ => false,
    }
}

fn aggregate_projection(
    function: &Function,
    alias: String,
    ctx: &TranslationContext<'_>,
) -> Result<AggregateProjection, String> {
    let name = function.name.to_string().to_ascii_uppercase();
    if !is_aggregate_function(function) {
        return Err(invalid(format!("unsupported aggregate function `{name}`")));
    }
    let FunctionArguments::List(arguments) = &function.args else {
        return Err(invalid(format!("invalid aggregate `{name}`")));
    };
    if arguments.args.len() != 1 || !arguments.clauses.is_empty() {
        return Err(invalid(format!("`{name}` requires exactly one argument")));
    }
    let argument = match &arguments.args[0] {
        FunctionArg::Unnamed(argument) => argument,
        FunctionArg::Named { .. } | FunctionArg::ExprNamed { .. } => {
            return Err(invalid("named aggregate arguments are unsupported"))
        }
    };
    let distinct = matches!(
        arguments.duplicate_treatment,
        Some(DuplicateTreatment::Distinct)
    );
    if distinct && name != "COUNT" {
        return Err(invalid(
            "DISTINCT is currently supported only by COUNT(DISTINCT field)",
        ));
    }
    let accumulator_field = if distinct {
        format!("__distinct_{alias}")
    } else {
        alias.clone()
    };
    let argument_value = match argument {
        FunctionArgExpr::Expr(expr) => expression_to_bson(expr, ctx)?,
        FunctionArgExpr::Wildcard | FunctionArgExpr::QualifiedWildcard(_) if name == "COUNT" => {
            Bson::Int32(1)
        }
        _ => return Err(invalid(format!("wildcard is only valid in COUNT"))),
    };
    let accumulator = if distinct {
        Bson::Document(doc! { "$addToSet": argument_value })
    } else {
        match name.as_str() {
            "COUNT" => match argument {
                FunctionArgExpr::Wildcard | FunctionArgExpr::QualifiedWildcard(_) => {
                    Bson::Document(doc! { "$sum": 1 })
                }
                _ => Bson::Document(doc! {
                    "$sum": { "$cond": [{ "$ne": [argument_value, Bson::Null] }, 1, 0] }
                }),
            },
            "SUM" => Bson::Document(doc! { "$sum": argument_value }),
            "AVG" => Bson::Document(doc! { "$avg": argument_value }),
            "MIN" => Bson::Document(doc! { "$min": argument_value }),
            "MAX" => Bson::Document(doc! { "$max": argument_value }),
            _ => unreachable!(),
        }
    };
    let final_expression = if distinct {
        Bson::Document(doc! { "$size": format!("${accumulator_field}") })
    } else {
        Bson::String(format!("${accumulator_field}"))
    };
    Ok(AggregateProjection {
        alias,
        accumulator_field,
        accumulator,
        final_expression,
    })
}

fn grouped_expression_to_bson(
    expr: &Expr,
    ctx: &TranslationContext<'_>,
    group_exprs: &[Expr],
    group: &mut Document,
    aggregate_index: &mut usize,
) -> Result<Bson, String> {
    if let Some(group_index) = group_exprs.iter().position(|group_expr| group_expr == expr) {
        return Ok(Bson::String(format!("$_id.g{group_index}")));
    }
    match expr {
        Expr::Function(function) if is_aggregate_function(function) => {
            let field = format!("__agg_{}", *aggregate_index);
            *aggregate_index += 1;
            let aggregate = aggregate_projection(function, field, ctx)?;
            group.insert(aggregate.accumulator_field, aggregate.accumulator);
            Ok(aggregate.final_expression)
        }
        Expr::Function(function) => {
            let name = function.name.to_string().to_ascii_uppercase();
            let args = function_args(function)?
                .into_iter()
                .map(|arg| match arg {
                    FunctionArgExpr::Expr(expr) => {
                        grouped_expression_to_bson(expr, ctx, group_exprs, group, aggregate_index)
                    }
                    _ => Err(invalid(format!("wildcard is invalid for `{name}`"))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            scalar_function_from_args(&name, args)
        }
        Expr::Value(value) => literal_to_bson(value),
        Expr::TypedString(value) => literal_to_bson(&value.value),
        Expr::Nested(inner) => {
            grouped_expression_to_bson(inner, ctx, group_exprs, group, aggregate_index)
        }
        Expr::Cast {
            expr: inner,
            data_type,
            ..
        } => cast_value_to_bson(
            grouped_expression_to_bson(inner, ctx, group_exprs, group, aggregate_index)?,
            data_type,
        ),
        Expr::UnaryOp { op, expr } => {
            let value = grouped_expression_to_bson(expr, ctx, group_exprs, group, aggregate_index)?;
            match op {
                UnaryOperator::Plus => Ok(value),
                UnaryOperator::Minus => Ok(Bson::Document(doc! { "$multiply": [-1, value] })),
                UnaryOperator::Not => Ok(Bson::Document(doc! { "$not": [value] })),
                _ => Err(invalid(format!("unsupported unary operator `{op}`"))),
            }
        }
        Expr::BinaryOp { left, op, right } => {
            let left = grouped_expression_to_bson(left, ctx, group_exprs, group, aggregate_index)?;
            let right =
                grouped_expression_to_bson(right, ctx, group_exprs, group, aggregate_index)?;
            let mongo_op = match op {
                BinaryOperator::Plus => "$add",
                BinaryOperator::Minus => "$subtract",
                BinaryOperator::Multiply => "$multiply",
                BinaryOperator::Divide => "$divide",
                BinaryOperator::Modulo => "$mod",
                BinaryOperator::Eq => "$eq",
                BinaryOperator::NotEq => "$ne",
                BinaryOperator::Gt => "$gt",
                BinaryOperator::GtEq => "$gte",
                BinaryOperator::Lt => "$lt",
                BinaryOperator::LtEq => "$lte",
                BinaryOperator::And => "$and",
                BinaryOperator::Or => "$or",
                _ => return Err(invalid(format!("unsupported binary operator `{op}`"))),
            };
            Ok(Bson::Document(doc! { mongo_op: [left, right] }))
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let grouped_operand = operand
                .as_deref()
                .map(|value| {
                    grouped_expression_to_bson(value, ctx, group_exprs, group, aggregate_index)
                })
                .transpose()?;
            let mut branches = Vec::with_capacity(conditions.len());
            for branch in conditions {
                let condition = grouped_expression_to_bson(
                    &branch.condition,
                    ctx,
                    group_exprs,
                    group,
                    aggregate_index,
                )?;
                let case = grouped_operand
                    .as_ref()
                    .map_or(condition.clone(), |operand| {
                        Bson::Document(doc! { "$eq": [operand.clone(), condition] })
                    });
                branches.push(Bson::Document(doc! {
                    "case": case,
                    "then": grouped_expression_to_bson(
                        &branch.result,
                        ctx,
                        group_exprs,
                        group,
                        aggregate_index,
                    )?,
                }));
            }
            Ok(Bson::Document(doc! {
                "$switch": {
                    "branches": branches,
                    "default": match else_result {
                        Some(value) => grouped_expression_to_bson(
                            value,
                            ctx,
                            group_exprs,
                            group,
                            aggregate_index,
                        )?,
                        None => Bson::Null,
                    }
                }
            }))
        }
        _ => Err(invalid(format!(
            "grouped expression `{expr}` must be a GROUP BY expression or use supported aggregates"
        ))),
    }
}

fn select_item_parts(item: &SelectItem) -> Result<(&Expr, Option<String>), String> {
    match item {
        SelectItem::UnnamedExpr(expr) => Ok((expr, None)),
        SelectItem::ExprWithAlias { expr, alias } => Ok((expr, Some(alias.value.clone()))),
        _ => Err(invalid("wildcards cannot be used in grouped queries")),
    }
}

fn default_column_name(expr: &Expr, ctx: &TranslationContext<'_>) -> String {
    field_name(expr, ctx)
        .ok()
        .and_then(|name| name.rsplit('.').next().map(str::to_string))
        .unwrap_or_else(|| expr.to_string())
}

fn parse_nonnegative_integer(expr: &Expr, label: &str) -> Result<usize, String> {
    let Expr::Value(value) = expr else {
        return Err(invalid(format!("{label} must be a non-negative integer")));
    };
    let Value::Number(raw, _) = &value.value else {
        return Err(invalid(format!("{label} must be a non-negative integer")));
    };
    raw.parse::<usize>()
        .map_err(|_| invalid(format!("{label} must be a non-negative integer")))
}

fn table_and_alias(select: &Select) -> Result<(String, Option<String>), String> {
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(invalid(
            "exactly one collection is required; JOIN is not supported",
        ));
    }
    match &select.from[0].relation {
        TableFactor::Table {
            name, alias, args, ..
        } if args.is_none() => {
            if name.0.len() != 1 {
                return Err(invalid(
                    "database-qualified collection names are not supported",
                ));
            }
            let collection = name
                .0
                .last()
                .and_then(ObjectNamePart::as_ident)
                .map(|ident| ident.value.clone())
                .ok_or_else(|| invalid("collection name is required"))?;
            Ok((
                collection,
                alias.as_ref().map(|alias| alias.name.value.clone()),
            ))
        }
        _ => Err(invalid(
            "derived tables and table functions are not supported",
        )),
    }
}

fn projection_alias_map(
    projection: &[SelectItem],
    ctx: &TranslationContext<'_>,
) -> Vec<(String, Expr)> {
    projection
        .iter()
        .filter_map(|item| match item {
            SelectItem::ExprWithAlias { expr, alias } => Some((alias.value.clone(), expr.clone())),
            SelectItem::UnnamedExpr(expr) => Some((default_column_name(expr, ctx), expr.clone())),
            _ => None,
        })
        .collect()
}

fn order_document(
    query: &Query,
    aliases: &[(String, Expr)],
    ctx: &TranslationContext<'_>,
    grouped: bool,
) -> Result<Option<Document>, String> {
    let Some(order_by) = &query.order_by else {
        return Ok(None);
    };
    if order_by.interpolate.is_some() {
        return Err(invalid("ORDER BY INTERPOLATE is not supported"));
    }
    let mut sort = Document::new();
    let OrderByKind::Expressions(expressions) = &order_by.kind else {
        return Err(invalid("ORDER BY ALL is not supported"));
    };
    for order in expressions {
        let field = match &order.expr {
            Expr::Identifier(ident)
                if aliases
                    .iter()
                    .any(|(alias, _)| alias.eq_ignore_ascii_case(&ident.value)) =>
            {
                if grouped {
                    ident.value.clone()
                } else {
                    let (_, source) = aliases
                        .iter()
                        .find(|(alias, _)| alias.eq_ignore_ascii_case(&ident.value))
                        .expect("alias existence checked");
                    field_name(source, ctx)?
                }
            }
            expr if grouped => aliases
                .iter()
                .find(|(_, source)| {
                    source == expr
                        || matches!(
                            (field_name(source, ctx), field_name(expr, ctx)),
                            (Ok(left), Ok(right)) if left.eq_ignore_ascii_case(&right)
                        )
                })
                .map(|(alias, _)| alias.clone())
                .ok_or_else(|| {
                    invalid(format!(
                        "grouped ORDER BY expression `{expr}` is not projected"
                    ))
                })?,
            Expr::Identifier(ident) => {
                if let Some((_, source)) = aliases
                    .iter()
                    .find(|(alias, _)| alias.eq_ignore_ascii_case(&ident.value))
                {
                    field_name(source, ctx)?
                } else {
                    field_name(&order.expr, ctx)?
                }
            }
            Expr::CompoundIdentifier(_) => field_name(&order.expr, ctx)?,
            _ => {
                return Err(invalid(
                    "ORDER BY supports field names and selected aliases only",
                ))
            }
        };
        sort.insert(
            field,
            if order.options.asc == Some(false) {
                -1
            } else {
                1
            },
        );
    }
    Ok((!sort.is_empty()).then_some(sort))
}

fn build_plan_from_query(query: &Query, max_rows: usize) -> Result<MongoQueryPlan, String> {
    let has_limit_by = query.limit_clause.as_ref().is_some_and(|clause| {
        matches!(
            clause,
            LimitClause::LimitOffset { limit_by, .. } if !limit_by.is_empty()
        )
    });
    if query.with.is_some()
        || has_limit_by
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
    {
        return Err(invalid(
            "CTE, FETCH, LIMIT BY and locking clauses are not supported",
        ));
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(invalid(
            "UNION, INTERSECT and nested query bodies are not supported",
        ));
    };
    if select.distinct.is_some()
        || select.top.is_some()
        || select.into.is_some()
        || select.prewhere.is_some()
        || !select.lateral_views.is_empty()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
    {
        return Err(invalid(
            "DISTINCT, TOP, INTO, PREWHERE, WINDOW and QUALIFY are not supported",
        ));
    }
    let (collection, table_alias) = table_and_alias(select)?;
    let now = Utc::now();
    let start_of_day = Utc
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()
        .ok_or_else(|| invalid("failed to calculate the current UTC day"))?;
    let ctx = TranslationContext {
        collection: &collection,
        table_alias: table_alias.as_deref(),
        query_timestamp: mongodb::bson::DateTime::from_millis(now.timestamp_millis()),
        query_date: mongodb::bson::DateTime::from_millis(start_of_day.timestamp_millis()),
        query_date_string: start_of_day.format("%Y-%m-%d").to_string(),
    };
    let group_exprs = match &select.group_by {
        GroupByExpr::Expressions(expressions, modifiers) if modifiers.is_empty() => expressions,
        GroupByExpr::Expressions(_, _) | GroupByExpr::All(_) => {
            return Err(invalid(
                "GROUP BY modifiers and GROUP BY ALL are not supported",
            ));
        }
    };
    let has_aggregates = select.projection.iter().any(|item| match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
            contains_aggregate(expr)
        }
        _ => false,
    });
    let grouped = has_aggregates || !group_exprs.is_empty();
    let aliases = projection_alias_map(&select.projection, &ctx);
    let mut pipeline = Vec::new();
    if let Some(selection) = &select.selection {
        pipeline.push(doc! { "$match": filter_to_document(selection, &ctx)? });
    }

    let mut columns = Vec::new();
    if grouped {
        let mut group_id = Document::new();
        for (index, expr) in group_exprs.iter().enumerate() {
            group_id.insert(format!("g{index}"), expression_to_bson(expr, &ctx)?);
        }
        let mut group = Document::new();
        group.insert(
            "_id",
            if group_id.is_empty() {
                Bson::Null
            } else {
                Bson::Document(group_id)
            },
        );
        let mut project = doc! { "_id": 0 };
        let mut aggregate_index = 0usize;
        for item in &select.projection {
            let (expr, explicit_alias) = select_item_parts(item)?;
            if contains_aggregate(expr) && explicit_alias.is_none() {
                return Err(invalid("aggregate expressions must have an alias"));
            }
            let alias = explicit_alias.unwrap_or_else(|| default_column_name(expr, &ctx));
            if alias.starts_with('$') || alias.contains('.') {
                return Err(invalid(format!("invalid result alias `{alias}`")));
            }
            // Scalar expressions derived entirely from grouped fields are valid
            // SQL (for example COALESCE(status, 'unknown')). The recursive
            // translator still rejects any raw field that is not in GROUP BY,
            // so this broadens compatibility without weakening correctness.
            let result_expression = grouped_expression_to_bson(
                expr,
                &ctx,
                group_exprs,
                &mut group,
                &mut aggregate_index,
            )?;
            project.insert(alias.clone(), result_expression);
            columns.push(alias);
        }
        pipeline.push(doc! { "$group": group });
        pipeline.push(doc! { "$project": project });
        if let Some(having) = &select.having {
            let post_group_ctx = TranslationContext {
                collection: "",
                table_alias: None,
                query_timestamp: ctx.query_timestamp,
                query_date: ctx.query_date,
                query_date_string: ctx.query_date_string.clone(),
            };
            pipeline.push(doc! { "$match": filter_to_document(having, &post_group_ctx)? });
        }
        if let Some(sort) = order_document(query, &aliases, &ctx, true)? {
            pipeline.push(doc! { "$sort": sort });
        }
    } else {
        if select.having.is_some() {
            return Err(invalid("HAVING requires GROUP BY or an aggregate"));
        }
        if let Some(sort) = order_document(query, &aliases, &ctx, false)? {
            pipeline.push(doc! { "$sort": sort });
        }
        let has_wildcard = select.projection.iter().any(|item| {
            matches!(
                item,
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)
            )
        });
        if has_wildcard && select.projection.len() != 1 {
            return Err(invalid(
                "wildcard cannot be mixed with explicit projected fields",
            ));
        }
        if !has_wildcard {
            let mut project = doc! { "_id": 0 };
            for item in &select.projection {
                let (expr, explicit_alias) = select_item_parts(item)?;
                if matches!(expr, Expr::Function(function) if is_aggregate_function(function)) {
                    return Err(invalid("aggregate expressions require grouped translation"));
                }
                let alias = explicit_alias.unwrap_or_else(|| default_column_name(expr, &ctx));
                if alias.starts_with('$') || alias.contains('.') {
                    return Err(invalid(format!("invalid result alias `{alias}`")));
                }
                project.insert(alias.clone(), expression_to_bson(expr, &ctx)?);
                columns.push(alias);
            }
            pipeline.push(doc! { "$project": project });
        }
    }

    let count_pipeline = pipeline.clone();
    let (offset, requested_limit) = match query.limit_clause.as_ref() {
        Some(LimitClause::LimitOffset { limit, offset, .. }) => (
            offset
                .as_ref()
                .map(|offset| parse_nonnegative_integer(&offset.value, "OFFSET"))
                .transpose()?
                .unwrap_or(0),
            limit
                .as_ref()
                .map(|limit| parse_nonnegative_integer(limit, "LIMIT"))
                .transpose()?
                .unwrap_or(max_rows),
        ),
        Some(LimitClause::OffsetCommaLimit { offset, limit }) => (
            parse_nonnegative_integer(offset, "OFFSET")?,
            parse_nonnegative_integer(limit, "LIMIT")?,
        ),
        None => (0, max_rows),
    };
    let effective_limit = requested_limit.min(max_rows.max(1));
    if offset > 0 {
        pipeline.push(doc! { "$skip": i64::try_from(offset).unwrap_or(i64::MAX) });
    }
    pipeline.push(doc! { "$limit": i64::try_from(effective_limit).unwrap_or(i64::MAX) });

    Ok(MongoQueryPlan {
        collection,
        pipeline,
        count_pipeline,
        columns,
    })
}

pub(crate) fn translate_sql(sql: &str, max_rows: usize) -> Result<MongoQueryPlan, String> {
    let statements = Parser::parse_sql(&GenericDialect {}, sql)
        .map_err(|error| invalid(format!("SQL parse failed: {error}")))?;
    if statements.len() != 1 {
        return Err(invalid("exactly one SELECT statement is required"));
    }
    let Statement::Query(query) = &statements[0] else {
        return Err(invalid("only SELECT statements are allowed"));
    };
    build_plan_from_query(query, max_rows.max(1).min(DEFAULT_MAX_ROWS))
}

fn bson_to_json(value: Bson) -> serde_json::Value {
    match value {
        Bson::Double(value) => serde_json::json!(value),
        Bson::String(value) => serde_json::Value::String(value),
        Bson::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(bson_to_json).collect())
        }
        Bson::Document(document) => serde_json::Value::Object(
            document
                .into_iter()
                .map(|(key, value)| (key, bson_to_json(value)))
                .collect(),
        ),
        Bson::Boolean(value) => serde_json::json!(value),
        Bson::Null | Bson::Undefined => serde_json::Value::Null,
        Bson::Int32(value) => serde_json::json!(value),
        Bson::Int64(value) => serde_json::json!(value),
        Bson::ObjectId(value) => serde_json::Value::String(value.to_hex()),
        Bson::DateTime(value) => serde_json::Value::String(
            value
                .try_to_rfc3339_string()
                .unwrap_or_else(|_| value.timestamp_millis().to_string()),
        ),
        Bson::Timestamp(value) => serde_json::json!({
            "time": value.time,
            "increment": value.increment,
        }),
        Bson::Decimal128(value) => serde_json::Value::String(value.to_string()),
        Bson::Binary(value) => serde_json::Value::String(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            value.bytes,
        )),
        Bson::RegularExpression(value) => serde_json::Value::String(value.pattern),
        Bson::JavaScriptCode(value) | Bson::Symbol(value) => serde_json::Value::String(value),
        Bson::JavaScriptCodeWithScope(value) => serde_json::Value::String(value.code),
        Bson::DbPointer(value) => serde_json::Value::String(format!("{value:?}")),
        Bson::MaxKey => serde_json::Value::String("MaxKey".to_string()),
        Bson::MinKey => serde_json::Value::String("MinKey".to_string()),
    }
}

pub(crate) async fn execute(
    config: &MongoConfig,
    sql: &str,
    max_rows: usize,
    timeout: Duration,
) -> Result<MongoQueryResult, String> {
    if config.database.trim().is_empty() {
        return Err(invalid("database is required"));
    }
    let plan = translate_sql(sql, max_rows)?;
    let uri = build_mongodb_uri(config)?;
    let mut options = mongodb::options::ClientOptions::parse(&uri)
        .await
        .map_err(|error| format!("MongoDB connection configuration failed: {error}"))?;
    options.server_selection_timeout = Some(timeout);
    options.connect_timeout = Some(timeout);
    let client = mongodb::Client::with_options(options)
        .map_err(|error| format!("MongoDB client creation failed: {error}"))?;
    let collection = client
        .database(config.database.trim())
        .collection::<Document>(&plan.collection);

    let query_future = async {
        let mut count_pipeline = plan.count_pipeline.clone();
        count_pipeline.push(doc! { "$count": "count" });
        let mut count_cursor = collection
            .aggregate(count_pipeline)
            .await
            .map_err(|error| format!("MongoDB count query failed: {error}"))?;
        let total_rows = if count_cursor
            .advance()
            .await
            .map_err(|error| format!("MongoDB count cursor failed: {error}"))?
        {
            let count_document = count_cursor
                .deserialize_current()
                .map_err(|error| format!("MongoDB count decode failed: {error}"))?;
            count_document
                .get_i64("count")
                .or_else(|_| count_document.get_i32("count").map(i64::from))
                .unwrap_or(0)
        } else {
            0
        };

        let mut cursor = collection
            .aggregate(plan.pipeline.clone())
            .await
            .map_err(|error| format!("MongoDB query failed: {error}"))?;
        let mut rows = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|error| format!("MongoDB query cursor failed: {error}"))?
        {
            let document = cursor
                .deserialize_current()
                .map_err(|error| format!("MongoDB result decode failed: {error}"))?;
            rows.push(bson_to_json(Bson::Document(document)));
        }
        let columns = if plan.columns.is_empty() {
            rows.first()
                .and_then(serde_json::Value::as_object)
                .map(|row| row.keys().cloned().collect())
                .unwrap_or_default()
        } else {
            plan.columns.clone()
        };
        Ok::<_, String>(MongoQueryResult {
            columns,
            rows,
            total_rows,
        })
    };

    tokio::time::timeout(timeout, query_future)
        .await
        .map_err(|_| {
            format!(
                "MongoDB query timed out after {} seconds",
                timeout.as_secs()
            )
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_filtered_projection_and_pagination() {
        let plan = translate_sql(
            "SELECT name, profile.city AS city FROM users WHERE age >= 18 AND status IN ('active', 'trial') ORDER BY name DESC LIMIT 20 OFFSET 5",
            100,
        )
        .expect("valid plan");
        assert_eq!(plan.collection, "users");
        assert_eq!(plan.columns, vec!["name", "city"]);
        assert_eq!(plan.pipeline.last(), Some(&doc! { "$limit": 20_i64 }));
        assert_eq!(
            plan.pipeline[plan.pipeline.len() - 2],
            doc! { "$skip": 5_i64 }
        );
    }

    #[test]
    fn translates_grouped_aggregates_and_having() {
        let plan = translate_sql(
            "SELECT region, COUNT(*) AS orders, SUM(amount) AS revenue, SUM(amount) / COUNT(*) AS avg_order_value FROM orders WHERE paid = true GROUP BY region HAVING orders > 2 ORDER BY revenue DESC LIMIT 10",
            100,
        )
        .expect("valid grouped plan");
        assert_eq!(
            plan.columns,
            vec!["region", "orders", "revenue", "avg_order_value"]
        );
        assert!(plan
            .pipeline
            .iter()
            .any(|stage| stage.contains_key("$group")));
        assert!(plan
            .pipeline
            .iter()
            .any(|stage| stage.contains_key("$match")));
    }

    #[test]
    fn rejects_join_and_mutation() {
        assert!(translate_sql(
            "SELECT * FROM users u JOIN orders o ON u.id = o.user_id",
            100
        )
        .unwrap_err()
        .contains("JOIN"));
        assert!(translate_sql("DELETE FROM users", 100)
            .unwrap_err()
            .contains("only SELECT"));
    }

    #[test]
    fn caps_user_limit_and_escapes_like_pattern() {
        let plan = translate_sql(
            "SELECT name FROM users WHERE name LIKE 'a.%_x' LIMIT 9999",
            50,
        )
        .expect("valid plan");
        assert_eq!(plan.pipeline.last(), Some(&doc! { "$limit": 50_i64 }));
        assert_eq!(like_regex("a.%_x"), r"^a\..*.x$");
        assert_eq!(like_regex("a.%_.x"), r"^a\..*.\.x$");
    }

    #[test]
    fn translates_current_date_and_current_timestamp_without_arguments() {
        let date_plan = translate_sql(
            "SELECT COUNT(*) AS total FROM orders WHERE created_at >= CURRENT_DATE",
            100,
        )
        .expect("CURRENT_DATE should translate");
        let date_pipeline = format!("{:?}", date_plan.pipeline);
        assert!(date_pipeline.contains("DateTime"));
        assert!(!date_pipeline.contains("$$NOW"));
        assert_eq!(date_plan.count_pipeline[0], date_plan.pipeline[0]);

        let timestamp_plan = translate_sql(
            "SELECT id FROM orders WHERE created_at <= CURRENT_TIMESTAMP LIMIT 10",
            100,
        )
        .expect("CURRENT_TIMESTAMP should translate");
        let timestamp_pipeline = format!("{:?}", timestamp_plan.pipeline);
        assert!(timestamp_pipeline.contains("DateTime"));
        assert!(!timestamp_pipeline.contains("$$NOW"));
    }

    #[test]
    fn translates_casts_and_rejects_unsupported_targets() {
        let plan = translate_sql(
            "SELECT COUNT(*) AS total FROM kpi WHERE business_date = CAST(CURRENT_DATE AS STRING)",
            100,
        )
        .expect("date string cast should translate");
        let pipeline = format!("{:?}", plan.pipeline);
        assert!(pipeline.contains(&Utc::now().format("%Y-%m-%d").to_string()));
        assert!(!pipeline.contains("$$NOW"));

        let converted = translate_sql(
            "SELECT CAST(score AS INTEGER) AS score_int FROM kpi LIMIT 10",
            100,
        )
        .expect("integer cast should translate");
        assert!(format!("{:?}", converted.pipeline).contains("$convert"));

        let error = translate_sql("SELECT CAST(score AS BINARY) FROM kpi", 100)
            .expect_err("unsupported cast must fail closed");
        assert!(error.contains("unsupported CAST target"));
    }

    #[test]
    fn grouped_order_by_source_expression_maps_to_projection_alias() {
        let plan = translate_sql(
            "SELECT executor_device_id AS device_id FROM task_offer GROUP BY executor_device_id ORDER BY executor_device_id LIMIT 100",
            100,
        )
        .expect("grouped source order should map to selected alias");
        assert!(plan
            .pipeline
            .iter()
            .any(|stage| stage == &doc! { "$sort": { "device_id": 1 } }));

        let qualified = translate_sql(
            "SELECT t.executor_device_id AS device_id FROM task_offer t GROUP BY t.executor_device_id ORDER BY t.executor_device_id DESC LIMIT 100",
            100,
        )
        .expect("qualified grouped source order should map to selected alias");
        assert!(qualified
            .pipeline
            .iter()
            .any(|stage| stage == &doc! { "$sort": { "device_id": -1 } }));
    }

    #[test]
    fn grouped_scalar_expression_can_derive_from_grouped_field() {
        let plan = translate_sql(
            "SELECT business_date, COALESCE(target_status, 'unknown') AS target_status, COUNT(*) AS kpi_count FROM daily_kpi_bucket GROUP BY business_date, target_status ORDER BY business_date, target_status",
            100,
        )
        .expect("COALESCE of a grouped field should translate");

        assert_eq!(
            plan.columns,
            vec!["business_date", "target_status", "kpi_count"]
        );
        let rendered = format!("{:?}", plan.pipeline);
        assert!(rendered.contains("$ifNull"));
        assert!(plan
            .pipeline
            .iter()
            .any(|stage| stage == &doc! { "$sort": { "business_date": 1, "target_status": 1 } }));
    }

    #[test]
    fn translates_common_nl2sql_shapes_without_dialect_leaks() {
        let queries = [
            "SELECT DATE(created_at) AS day, COUNT(id) AS orders FROM orders WHERE created_at BETWEEN ISO_DATE('2026-08-01T00:00:00Z') AND ISO_DATE('2026-08-08T00:00:00Z') GROUP BY DATE(created_at) ORDER BY day",
            "SELECT status, COUNT(DISTINCT user_id) AS users, AVG(amount) AS avg_amount FROM orders WHERE status IN ('paid', 'refunded') GROUP BY status HAVING users > 0 ORDER BY users DESC",
            "SELECT CASE WHEN enabled = true THEN 'enabled' ELSE 'disabled' END AS state, COUNT(*) AS total FROM devices GROUP BY enabled ORDER BY state",
            "SELECT CAST(score AS INTEGER) AS score_bucket, COUNT(*) AS total FROM metrics GROUP BY score ORDER BY score_bucket",
            "SELECT profile.city AS city, MIN(created_at) AS first_seen, MAX(created_at) AS last_seen FROM users u WHERE deleted_at IS NULL GROUP BY profile.city ORDER BY city LIMIT 50",
        ];

        for sql in queries {
            translate_sql(sql, 100)
                .unwrap_or_else(|error| panic!("failed to translate `{sql}`: {error}"));
        }
    }
}
