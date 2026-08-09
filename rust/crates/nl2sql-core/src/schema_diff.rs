//! Structural schema diff.
//!
//! The stored `schema_info` JSON on `data_sources` can re-order its fields
//! (because MySQL's JSON driver or a different discovery pass may output
//! tables in a different order). A naive `serde_json::Value` equality check
//! would fire on every refresh, causing needless LLM work.
//!
//! This module normalises the two sides — strips whitespace, sorts tables by
//! name and columns by name, discards non-semantic fields — then computes
//! a set-based diff so we only re-index the tables that **actually** changed.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// A single column's comparable shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct NormColumn {
    name: String,
    /// Canonical-cased SQL type (e.g. `VARCHAR(255)`).
    r#type: String,
    /// `NULL` / `NOT NULL`. Missing → "NULL" (SQL default).
    nullable: String,
    /// Primary-key flag as reported by the driver; defaults to false.
    primary_key: bool,
}

/// A single table's comparable shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NormTable {
    columns: Vec<NormColumn>,
}

/// Result of a structural schema diff.
#[derive(Debug, Default, Clone)]
pub struct SchemaDiff {
    /// Tables that are new in the new schema (not present in the old one).
    pub added: Vec<String>,
    /// Tables that exist in both schemas but whose column set changed.
    /// The caller should re-index these.
    pub changed: Vec<String>,
    /// Tables that exist in the old schema but not in the new one.
    pub removed: Vec<String>,
}

impl SchemaDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Compute the set of tables that differ between `old` and `new`.
///
/// Both inputs are expected to be `schema_info`-shaped JSON arrays:
/// `[{"table_name": "...", "columns": [{"name": "...", "type": "...", ...}, ...]}]`.
/// Missing / malformed inputs are treated as empty.
///
/// **Manual tables are invisible to this diff.** A table entry whose
/// `is_manual == true` in the *old* side is intentionally excluded from
/// the normalisation pass, so the scheduler never reports a user-created
/// table as "removed" simply because it doesn't exist in the live source
/// database. Manual tables are preserved separately by the caller, which
/// must merge them back into the saved `schema_info` after running this
/// diff.
pub fn diff_schemas(old: &Value, new: &Value) -> SchemaDiff {
    let old_map = normalise(old);
    let new_map = normalise(new);

    let mut out = SchemaDiff::default();

    // Any table in new that is missing from old → added.
    for (name, new_t) in &new_map {
        match old_map.get(name) {
            Some(old_t) if old_t == new_t => {}
            Some(_) => out.changed.push(name.clone()),
            None => out.added.push(name.clone()),
        }
    }

    // Any table in old that is absent from new → removed.
    let new_names: HashSet<&String> = new_map.keys().collect();
    for name in old_map.keys() {
        if !new_names.contains(name) {
            out.removed.push(name.clone());
        }
    }

    out.added.sort();
    out.changed.sort();
    out.removed.sort();
    out
}

/// Extract the subset of `schema_info` entries that are user-managed
/// ("manual") tables. Callers merge these back into the live-discovered
/// schema so the scheduler never drops them.
pub fn extract_manual_tables(schema: &Value) -> Vec<Value> {
    let arr = match schema {
        Value::Object(o) => o.get("tables").and_then(|t| t.as_array()),
        Value::Array(a) => Some(a),
        _ => None,
    };
    arr.map(|arr| {
        arr.iter()
            .filter(|t| {
                t.get("is_manual")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    })
    .unwrap_or_default()
}

/// Normalises `schema_info` (which may be either `{"tables": [...]}` or a flat
/// `[...]` array) into a `Vec<Value>` of table entries. Returns an empty vec if
/// the value is neither.
pub fn extract_schema_tables(schema: &Value) -> Vec<Value> {
    let arr = match schema {
        Value::Object(o) => o.get("tables").and_then(|t| t.as_array()),
        Value::Array(a) => Some(a),
        _ => None,
    };
    arr.map(|a| a.iter().cloned().collect()).unwrap_or_default()
}

fn normalise(v: &Value) -> BTreeMap<String, NormTable> {
    let arr: &Vec<Value> = match v {
        Value::Object(o) => match o.get("tables").and_then(|t| t.as_array()) {
            Some(a) => a,
            None => return BTreeMap::new(),
        },
        Value::Array(a) => a,
        _ => return BTreeMap::new(),
    };

    let mut out = BTreeMap::new();
    for table in arr {
        // Manual tables are user-managed; they never exist in the live
        // source database and must not appear in the diff, or the
        // scheduler will classify them as "removed" and purge their
        // semantics on the next cycle.
        if table
            .get("is_manual")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let Some(name) = table.get("table_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let cols_arr = table
            .get("columns")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut cols: Vec<NormColumn> = cols_arr
            .iter()
            .filter_map(|c| {
                let col_name = c.get("name").and_then(|v| v.as_str())?.to_string();
                let col_type = c
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_uppercase();
                let nullable = match c.get("nullable") {
                    Some(Value::Bool(true)) => "NULL".to_string(),
                    Some(Value::Bool(false)) => "NOT NULL".to_string(),
                    Some(Value::String(s)) => s.trim().to_uppercase(),
                    _ => "NULL".to_string(),
                };
                let primary_key = c
                    .get("primary_key")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(NormColumn {
                    name: col_name,
                    r#type: col_type,
                    nullable,
                    primary_key,
                })
            })
            .collect();
        cols.sort_by(|a, b| a.name.cmp(&b.name));
        out.insert(name.to_string(), NormTable { columns: cols });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_schemas_produce_empty_diff() {
        let a = json!([
            {"table_name": "t1", "columns": [{"name": "c1", "type": "INT"}]}
        ]);
        let b = a.clone();
        let d = diff_schemas(&a, &b);
        assert!(d.is_empty());
    }

    #[test]
    fn reordered_tables_are_equal() {
        let a = json!([
            {"table_name": "t1", "columns": []},
            {"table_name": "t2", "columns": []},
        ]);
        let b = json!([
            {"table_name": "t2", "columns": []},
            {"table_name": "t1", "columns": []},
        ]);
        assert!(diff_schemas(&a, &b).is_empty());
    }

    #[test]
    fn reordered_columns_are_equal() {
        let a = json!([
            {"table_name": "t1", "columns": [
                {"name": "a", "type": "INT"},
                {"name": "b", "type": "VARCHAR(32)"}
            ]}
        ]);
        let b = json!([
            {"table_name": "t1", "columns": [
                {"name": "b", "type": "varchar(32)"},
                {"name": "a", "type": "int"}
            ]}
        ]);
        assert!(diff_schemas(&a, &b).is_empty());
    }

    #[test]
    fn added_table_is_reported_as_added() {
        let a = json!([{"table_name": "t1", "columns": []}]);
        let b = json!([
            {"table_name": "t1", "columns": []},
            {"table_name": "t2", "columns": []},
        ]);
        let d = diff_schemas(&a, &b);
        assert_eq!(d.added, vec!["t2"]);
    }

    #[test]
    fn removed_table_is_reported_as_removed() {
        let a = json!([
            {"table_name": "t1", "columns": []},
            {"table_name": "t2", "columns": []},
        ]);
        let b = json!([{"table_name": "t1", "columns": []}]);
        let d = diff_schemas(&a, &b);
        assert_eq!(d.removed, vec!["t2"]);
        assert!(d.changed.is_empty());
        assert!(d.added.is_empty());
    }

    #[test]
    fn column_change_marks_table_changed() {
        let a = json!([
            {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]}
        ]);
        let b = json!([
            {"table_name": "t1", "columns": [{"name": "a", "type": "BIGINT"}]}
        ]);
        let d = diff_schemas(&a, &b);
        assert_eq!(d.changed, vec!["t1"]);
    }

    #[test]
    fn manual_tables_are_never_reported_as_changed_or_removed() {
        // `manual_tbl` exists only in the stored side (user-added) and is
        // absent from the freshly-discovered schema. It must NOT surface
        // as "removed" — otherwise the scheduler would nuke its semantics.
        let stored = json!([
            {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
            {"table_name": "manual_tbl", "is_manual": true, "columns": [
                {"name": "x", "type": "TEXT"}
            ]},
        ]);
        let live = json!([
            {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
        ]);
        let d = diff_schemas(&stored, &live);
        assert!(
            d.removed.is_empty(),
            "manual table incorrectly reported as removed"
        );
        assert!(
            d.changed.is_empty(),
            "manual table incorrectly triggered a change"
        );
        assert!(
            d.added.is_empty(),
            "manual table incorrectly triggered an add"
        );
    }

    #[test]
    fn extract_manual_tables_returns_only_manual_entries() {
        let schema = json!([
            {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
            {"table_name": "m1", "is_manual": true, "columns": [
                {"name": "x", "type": "TEXT"}
            ]},
        ]);
        let manual = extract_manual_tables(&schema);
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0]["table_name"], "m1");
    }

    #[test]
    fn normalise_handles_object_format() {
        // Object format: {"tables": [...], "foreign_keys": [...]}
        let schema = json!({
            "tables": [
                {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
                {"table_name": "t2", "columns": [{"name": "b", "type": "VARCHAR(32)"}]},
            ],
            "foreign_keys": [
                {"source_table": "t1", "source_column": "id", "target_table": "t2", "target_column": "t1_id"}
            ]
        });
        let result = normalise(&schema);
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("t1"));
        assert!(result.contains_key("t2"));
    }

    #[test]
    fn normalise_object_format_skips_manual_tables() {
        let schema = json!({
            "tables": [
                {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
                {"table_name": "manual_tbl", "is_manual": true, "columns": [{"name": "x", "type": "TEXT"}]},
            ],
            "foreign_keys": []
        });
        let result = normalise(&schema);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("t1"));
        assert!(!result.contains_key("manual_tbl"));
    }

    #[test]
    fn extract_manual_tables_handles_object_format() {
        let schema = json!({
            "tables": [
                {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
                {"table_name": "m1", "is_manual": true, "columns": [{"name": "x", "type": "TEXT"}]},
            ],
            "foreign_keys": []
        });
        let manual = extract_manual_tables(&schema);
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0]["table_name"], "m1");
    }

    #[test]
    fn diff_schemas_object_format_detects_changes() {
        let old = json!({
            "tables": [
                {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
            ],
            "foreign_keys": []
        });
        let new = json!({
            "tables": [
                {"table_name": "t1", "columns": [{"name": "a", "type": "INT"}]},
                {"table_name": "t2", "columns": []},
            ],
            "foreign_keys": []
        });
        let d = diff_schemas(&old, &new);
        assert_eq!(d.added, vec!["t2"]);
        assert!(d.removed.is_empty());
        assert!(d.changed.is_empty());
    }

    #[test]
    fn diff_schemas_object_format_detects_removal() {
        let old = json!({
            "tables": [
                {"table_name": "t1", "columns": []},
                {"table_name": "t2", "columns": []},
            ],
            "foreign_keys": []
        });
        let new = json!({
            "tables": [
                {"table_name": "t1", "columns": []},
            ],
            "foreign_keys": []
        });
        let d = diff_schemas(&old, &new);
        assert_eq!(d.removed, vec!["t2"]);
        assert!(d.changed.is_empty());
        assert!(d.added.is_empty());
    }
}
