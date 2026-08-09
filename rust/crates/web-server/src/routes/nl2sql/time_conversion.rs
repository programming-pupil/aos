//! SQL time-conversion normalization for generated NL2SQL statements.
//!
//! The LLM sometimes treats numeric timestamp columns as Unix seconds even when
//! the source data stores milliseconds. MySQL returns NULL for out-of-range
//! `FROM_UNIXTIME(13_digit_ms)` values, so the query "succeeds" with a NULL date
//! bucket. This module applies a narrow, schema-aware rewrite to generated SQL.

use regex::{Captures, Regex};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimeConversionRewrite {
    pub column: String,
    pub type_name: String,
    pub strategy: String,
}

#[derive(Debug, Clone)]
struct ColumnMeta {
    table: String,
    column: String,
    type_name: String,
}

#[derive(Debug, Default)]
struct SchemaIndex {
    by_table_column: HashMap<(String, String), ColumnMeta>,
    by_column: HashMap<String, Option<ColumnMeta>>,
}

pub(crate) fn normalize_generated_time_conversions(
    sql: &str,
    schema: &Value,
    db_type: &str,
) -> (String, Vec<TimeConversionRewrite>) {
    if !matches!(db_type.to_ascii_lowercase().as_str(), "mysql" | "tidb") {
        return (sql.to_string(), Vec::new());
    }

    let index = build_schema_index(schema);
    if index.by_table_column.is_empty() {
        return (sql.to_string(), Vec::new());
    }

    let aliases = extract_aliases(sql);
    let mut rewrites = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let rewritten = from_unixtime_column_regex()
        .replace_all(sql, |caps: &Captures<'_>| {
            let Some(expr_match) = caps.get(1) else {
                return caps[0].to_string();
            };
            let expr = expr_match.as_str().trim();
            let Some((qualifier, column)) = split_column_expr(expr) else {
                return caps[0].to_string();
            };
            let Some(meta) = resolve_column_meta(&index, &aliases, qualifier.as_deref(), &column)
            else {
                return caps[0].to_string();
            };

            if is_datetime_type(&meta.type_name) {
                if seen.insert((meta.table.clone(), meta.column.clone())) {
                    rewrites.push(TimeConversionRewrite {
                        column: format!("{}.{}", meta.table, meta.column),
                        type_name: meta.type_name.clone(),
                        strategy: "datetime_passthrough".to_string(),
                    });
                }
                return expr.to_string();
            }

            if is_numeric_type(&meta.type_name) {
                if seen.insert((meta.table.clone(), meta.column.clone())) {
                    rewrites.push(TimeConversionRewrite {
                        column: format!("{}.{}", meta.table, meta.column),
                        type_name: meta.type_name.clone(),
                        strategy: "adaptive_epoch_seconds_millis_micros".to_string(),
                    });
                }
                return format!(
                    "FROM_UNIXTIME(CASE WHEN ABS({expr}) >= 1000000000000000 THEN {expr} / 1000000 WHEN ABS({expr}) >= 100000000000 THEN {expr} / 1000 ELSE {expr} END)"
                );
            }

            caps[0].to_string()
        })
        .to_string();

    (rewritten, rewrites)
}

fn from_unixtime_column_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(
            r"(?i)FROM_UNIXTIME\s*\(\s*(`?[A-Za-z_][A-Za-z0-9_]*`?(?:\s*\.\s*`?[A-Za-z_][A-Za-z0-9_]*`?)?)\s*\)",
        )
        .expect("valid FROM_UNIXTIME column regex")
    })
}

fn alias_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:FROM|JOIN)\s+((?:`?[A-Za-z_][A-Za-z0-9_]*`?\.)?`?[A-Za-z_][A-Za-z0-9_]*`?)(?:\s+(?:AS\s+)?`?([A-Za-z_][A-Za-z0-9_]*)`?)?",
        )
        .expect("valid FROM/JOIN alias regex")
    })
}

fn build_schema_index(schema: &Value) -> SchemaIndex {
    let mut index = SchemaIndex::default();
    let tables = schema
        .as_array()
        .or_else(|| schema.get("tables").and_then(Value::as_array));

    let Some(tables) = tables else {
        return index;
    };

    for table in tables {
        let Some(table_name) = table.get("table_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(columns) = table.get("columns").and_then(Value::as_array) else {
            continue;
        };
        for column in columns {
            let Some(column_name) = column.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(type_name) = column
                .get("type")
                .or_else(|| column.get("column_type"))
                .or_else(|| column.get("data_type"))
                .and_then(Value::as_str)
            else {
                continue;
            };

            let meta = ColumnMeta {
                table: table_name.to_string(),
                column: column_name.to_string(),
                type_name: type_name.to_string(),
            };
            index.by_table_column.insert(
                (
                    normalize_identifier(table_name),
                    normalize_identifier(column_name),
                ),
                meta.clone(),
            );
            index
                .by_column
                .entry(normalize_identifier(column_name))
                .and_modify(|slot| *slot = None)
                .or_insert(Some(meta));
        }
    }

    index
}

fn extract_aliases(sql: &str) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for caps in alias_regex().captures_iter(sql) {
        let Some(table_match) = caps.get(1) else {
            continue;
        };
        let table = last_path_segment(table_match.as_str());
        if table.is_empty() {
            continue;
        }
        aliases.insert(normalize_identifier(&table), normalize_identifier(&table));

        if let Some(alias_match) = caps.get(2) {
            let alias = normalize_identifier(alias_match.as_str());
            if !alias.is_empty() && !is_sql_keyword(&alias) {
                aliases.insert(alias, normalize_identifier(&table));
            }
        }
    }
    aliases
}

fn split_column_expr(expr: &str) -> Option<(Option<String>, String)> {
    let parts: Vec<String> = expr
        .split('.')
        .map(normalize_identifier)
        .filter(|s| !s.is_empty())
        .collect();
    match parts.as_slice() {
        [column] => Some((None, column.clone())),
        [qualifier, column] => Some((Some(qualifier.clone()), column.clone())),
        _ => None,
    }
}

fn resolve_column_meta(
    index: &SchemaIndex,
    aliases: &HashMap<String, String>,
    qualifier: Option<&str>,
    column: &str,
) -> Option<ColumnMeta> {
    if let Some(q) = qualifier {
        let table = aliases.get(q).map(String::as_str).unwrap_or(q);
        if let Some(meta) = index
            .by_table_column
            .get(&(normalize_identifier(table), normalize_identifier(column)))
        {
            return Some(meta.clone());
        }
    }

    index
        .by_column
        .get(&normalize_identifier(column))
        .and_then(Clone::clone)
}

fn normalize_identifier(input: &str) -> String {
    input.trim().trim_matches('`').trim().to_ascii_lowercase()
}

fn last_path_segment(input: &str) -> String {
    input
        .split('.')
        .next_back()
        .map(normalize_identifier)
        .unwrap_or_default()
}

fn is_sql_keyword(input: &str) -> bool {
    matches!(
        input,
        "where"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "outer"
            | "on"
            | "group"
            | "order"
            | "limit"
            | "having"
            | "union"
    )
}

fn is_datetime_type(type_name: &str) -> bool {
    let t = type_name.to_ascii_lowercase();
    t.contains("datetime")
        || t.contains("timestamp")
        || t == "date"
        || t.starts_with("date(")
        || t == "time"
        || t.starts_with("time(")
}

fn is_numeric_type(type_name: &str) -> bool {
    let t = type_name.to_ascii_lowercase();
    t.contains("int")
        || t.contains("decimal")
        || t.contains("numeric")
        || t.contains("double")
        || t.contains("float")
        || t.contains("real")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rewrites_numeric_timestamp_to_adaptive_epoch() {
        let schema = json!([
            {
                "table_name": "business_order",
                "columns": [
                    {"name": "create_timestamp", "type": "bigint(20)"},
                    {"name": "id", "type": "bigint(20)"}
                ]
            }
        ]);
        let sql = "SELECT DATE_FORMAT(FROM_UNIXTIME(o.create_timestamp), '%Y') AS year FROM business_order o GROUP BY DATE_FORMAT(FROM_UNIXTIME(o.create_timestamp), '%Y')";

        let (rewritten, rewrites) = normalize_generated_time_conversions(sql, &schema, "mysql");

        assert!(rewritten.contains("CASE WHEN ABS(o.create_timestamp) >= 1000000000000000"));
        assert!(!rewritten.contains("FROM_UNIXTIME(o.create_timestamp)"));
        assert_eq!(rewrites.len(), 1);
        assert_eq!(rewrites[0].strategy, "adaptive_epoch_seconds_millis_micros");
    }

    #[test]
    fn removes_from_unixtime_for_datetime_columns() {
        let schema = json!([
            {
                "table_name": "orders",
                "columns": [{"name": "created_at", "type": "datetime"}]
            }
        ]);
        let sql = "SELECT DATE_FORMAT(FROM_UNIXTIME(created_at), '%Y') AS year FROM orders";

        let (rewritten, rewrites) = normalize_generated_time_conversions(sql, &schema, "tidb");

        assert_eq!(
            rewritten,
            "SELECT DATE_FORMAT(created_at, '%Y') AS year FROM orders"
        );
        assert_eq!(rewrites[0].strategy, "datetime_passthrough");
    }

    #[test]
    fn does_not_rewrite_unknown_or_non_mysql_sql() {
        let schema = json!([
            {
                "table_name": "orders",
                "columns": [{"name": "created_at", "type": "varchar(32)"}]
            }
        ]);
        let sql = "SELECT FROM_UNIXTIME(created_at) FROM orders";

        let (rewritten, rewrites) = normalize_generated_time_conversions(sql, &schema, "mysql");
        assert_eq!(rewritten, sql);
        assert!(rewrites.is_empty());

        let (rewritten, rewrites) = normalize_generated_time_conversions(sql, &schema, "postgres");
        assert_eq!(rewritten, sql);
        assert!(rewrites.is_empty());
    }
}
