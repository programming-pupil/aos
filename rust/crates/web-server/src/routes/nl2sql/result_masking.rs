//! Sensitive-column detection and result-row masking.
//!
//! Extracted from `mod.rs` to isolate the data-protection rules from query-handling code.
//! Masking is now fully configuration-driven:
//!
//! * **Per-datasource patterns** — loaded from `data_sources.sensitive_columns` JSON column.
//! * **Tenant masking rules** — loaded from `nl2sql_column_masking_rules` in execution paths.
//!
//! No hardcoded always-mask column list is enforced in code.

use super::ColumnInfo;

/// Legacy helper kept for API compatibility in sibling modules.
/// Built-in hardcoded sensitive pattern matching was removed in favor of
/// DB-managed masking rules, so this now always returns false.
pub(crate) fn is_sensitive_column(_name: &str) -> bool {
    false
}

/// Replace `value` with a fixed placeholder when the column is sensitive.
///
/// * Empty strings stay empty (no information leak; saves the placeholder).
/// * Strings become `"***MASKED***"`.
/// * `null` stays `null`.
/// * Non-string values become `null` so numeric IDs in sensitive columns are not exposed.
pub(crate) fn mask_sensitive_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) if s.is_empty() => serde_json::Value::String(s.clone()),
        serde_json::Value::String(_) => serde_json::Value::String("***MASKED***".to_string()),
        serde_json::Value::Null => serde_json::Value::Null,
        _ => serde_json::Value::Null,
    }
}

/// Apply masking to a single row keyed by `columns`, returning the new row and the column
/// metadata updated with `masked: true` flags for the masked columns.
#[allow(dead_code)]
pub(crate) fn apply_row_masking(
    row: serde_json::Map<String, serde_json::Value>,
    columns: &[ColumnInfo],
) -> (serde_json::Map<String, serde_json::Value>, Vec<ColumnInfo>) {
    let mut masked_row = serde_json::Map::new();
    let mut updated_columns: Vec<ColumnInfo> = Vec::with_capacity(columns.len());

    for col in columns {
        let value = row.get(&col.name);
        let (final_value, is_masked) = if is_datasource_sensitive(&col.name, &[]) {
            (
                value
                    .map(mask_sensitive_value)
                    .unwrap_or(serde_json::Value::Null),
                true,
            )
        } else {
            (value.cloned().unwrap_or(serde_json::Value::Null), false)
        };
        masked_row.insert(col.name.clone(), final_value);
        updated_columns.push(ColumnInfo {
            name: col.name.clone(),
            type_: col.type_.clone(),
            masked: Some(is_masked),
        });
    }

    (masked_row, updated_columns)
}

/// True if `col_name` matches any datasource-specific sensitive pattern.
pub(crate) fn is_datasource_sensitive(col_name: &str, patterns: &[String]) -> bool {
    let col_lower = col_name.to_lowercase();
    for pattern in patterns {
        let p_lower = pattern.to_lowercase();
        if p_lower.is_empty() {
            continue;
        }
        if p_lower == col_lower || col_lower.contains(&p_lower) {
            return true;
        }
    }
    false
}

/// Loads the per-datasource sensitive-column pattern list from `data_sources.sensitive_columns`.
/// Returns an empty Vec on miss or error — masking degrades gracefully to built-in patterns only.
#[allow(dead_code)]
pub(crate) async fn load_datasource_sensitive_columns(
    db: &sqlx::SqlitePool,
    datasource_id: &str,
) -> Vec<String> {
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT sensitive_columns FROM data_sources WHERE id = ?")
            .bind(datasource_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

    match row {
        Some((serde_json::Value::Array(arr),)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// Mask sensitive columns in every row of `rows` in place. Used right before sending result
/// rows back to the API caller.
#[allow(dead_code)]
pub(crate) fn apply_datasource_masking(rows: &mut [serde_json::Value], patterns: &[String]) {
    for row in rows.iter_mut() {
        mask_nested_value(row, "", patterns);
    }
}

fn mask_nested_value(value: &mut serde_json::Value, prefix: &str, patterns: &[String]) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if is_datasource_sensitive(&path, patterns)
                    || is_datasource_sensitive(key, patterns)
                {
                    *child = mask_sensitive_value(child);
                } else {
                    mask_nested_value(child, &path, patterns);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                mask_nested_value(child, prefix, patterns);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datasource_masking_covers_nested_mongodb_fields() {
        let mut rows = vec![serde_json::json!({
            "name": "Ada",
            "profile": {
                "ssn": "123-45-6789",
                "contacts": [{ "email": "ada@example.com", "kind": "work" }]
            }
        })];

        apply_datasource_masking(&mut rows, &["profile.ssn".to_string(), "email".to_string()]);

        assert_eq!(rows[0]["name"], "Ada");
        assert_eq!(rows[0]["profile"]["ssn"], "***MASKED***");
        assert_eq!(rows[0]["profile"]["contacts"][0]["email"], "***MASKED***");
        assert_eq!(rows[0]["profile"]["contacts"][0]["kind"], "work");
    }
}
