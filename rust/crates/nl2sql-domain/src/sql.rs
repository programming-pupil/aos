//! Lightweight SQL/result helpers shared by NL2SQL route implementations.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Cheap regex pass that pulls top-level table identifiers out of a SQL
/// statement for use in coreference resolution and masking. This is best-effort:
/// failure to extract only reduces downstream context quality.
pub fn extract_top_level_tables(sql: &str) -> Vec<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)\b(?:FROM|JOIN)\s+((?:"[^"]+"|`[^`]+`|[A-Za-z_][A-Za-z0-9_$]*)(?:\s*\.\s*(?:"[^"]+"|`[^`]+`|[A-Za-z_][A-Za-z0-9_$]*))*)"#,
        )
        .expect("static regex")
    });
    let mut out: Vec<String> = re
        .captures_iter(sql)
        .filter_map(|c| c.get(1).map(|m| normalize_table_ref(m.as_str())))
        .filter(|name| !name.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Prefer physical-looking qualified names when schema hydration has a strict
/// table budget. Input order is retained within each priority class so the
/// knowledge retriever remains the authority for relevance.
pub fn prioritize_schema_discovery_tables(
    table_names: impl IntoIterator<Item = String>,
    max_tables: usize,
) -> Vec<String> {
    let mut qualified = Vec::new();
    let mut unqualified = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for table in table_names {
        let normalized = normalize_table_ref(&table);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        if table
            .split('.')
            .filter(|part| !part.trim().is_empty())
            .count()
            >= 2
        {
            qualified.push(table);
        } else {
            unqualified.push(table);
        }
    }
    qualified.extend(unqualified);
    qualified.truncate(max_tables);
    qualified
}

fn normalize_table_ref(raw: &str) -> String {
    raw.trim()
        .trim_end_matches(';')
        .trim_end_matches(',')
        .split('.')
        .filter_map(|part| {
            let cleaned = part
                .trim()
                .trim_matches('`')
                .trim_matches('"')
                .trim_matches('[')
                .trim_matches(']')
                .trim();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned.to_ascii_lowercase())
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Build a compact JSON summary of stored query results for explanation prompts.
pub fn build_data_summary(
    columns_json: &Option<String>,
    rows_json: &Option<String>,
    row_count: u32,
) -> String {
    let cols: Vec<String> = columns_json
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let rows: Vec<serde_json::Value> = rows_json
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let preview_rows = if rows.len() > 10 {
        rows[..10].to_vec()
    } else {
        rows
    };

    let summary = serde_json::json!({
        "total_rows": row_count,
        "preview_rows": preview_rows,
        "column_count": cols.len(),
    });

    serde_json::to_string_pretty(&summary).unwrap_or_else(|_| "{}".to_string())
}

fn json_value_to_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            let normalized = s.trim().trim_matches('%').replace(',', "");
            if normalized.is_empty() {
                None
            } else {
                normalized.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

fn display_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::String(s) => s.chars().take(80).collect(),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => value.to_string(),
        _ => serde_json::to_string(value)
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect(),
    }
}

/// Build a compact JSON summary of in-memory rows for explanation prompts.
pub fn build_data_summary_for_rows(rows: &[serde_json::Value]) -> String {
    if rows.is_empty() {
        return r#"{"total_rows": 0, "preview_rows": [], "column_count": 0}"#.to_string();
    }

    let preview_rows = if rows.len() > 20 {
        rows[..20].to_vec()
    } else {
        rows.to_vec()
    };

    let columns: Vec<String> = rows
        .first()
        .and_then(serde_json::Value::as_object)
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();
    let column_count = columns.len();

    let mut numeric_stats = Vec::new();
    let mut categorical_samples = Vec::new();

    for column in columns.iter().take(40) {
        let mut count = 0usize;
        let mut nulls = 0usize;
        let mut nums = Vec::new();
        let mut frequencies: BTreeMap<String, usize> = BTreeMap::new();

        for row in rows {
            let value = row
                .as_object()
                .and_then(|obj| obj.get(column))
                .unwrap_or(&serde_json::Value::Null);
            if value.is_null() {
                nulls += 1;
                continue;
            }
            count += 1;
            if let Some(num) = json_value_to_f64(value) {
                if num.is_finite() {
                    nums.push(num);
                    continue;
                }
            }
            let key = display_value(value);
            if !key.is_empty() {
                *frequencies.entry(key).or_insert(0) += 1;
            }
        }

        if nums.len() >= count.saturating_div(2).max(1) {
            nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let sum: f64 = nums.iter().sum();
            let min = *nums.first().unwrap_or(&0.0);
            let max = *nums.last().unwrap_or(&0.0);
            let avg = if nums.is_empty() {
                0.0
            } else {
                sum / nums.len() as f64
            };
            let median = if nums.is_empty() {
                0.0
            } else {
                nums[nums.len() / 2]
            };
            numeric_stats.push(serde_json::json!({
                "column": column,
                "count": nums.len(),
                "nulls": nulls,
                "min": min,
                "max": max,
                "avg": avg,
                "median": median,
                "sum": sum
            }));
        } else if !frequencies.is_empty() {
            let mut top_values: Vec<(String, usize)> = frequencies.into_iter().collect();
            top_values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            categorical_samples.push(serde_json::json!({
                "column": column,
                "count": count,
                "nulls": nulls,
                "top_values": top_values.into_iter().take(8).map(|(value, count)| {
                    serde_json::json!({"value": value, "count": count})
                }).collect::<Vec<_>>()
            }));
        }
    }

    let summary = serde_json::json!({
        "total_rows": rows.len(),
        "preview_rows": preview_rows,
        "column_count": column_count,
        "numeric_stats": numeric_stats,
        "categorical_samples": categorical_samples,
        "summary_scope": "Statistics are computed from the available result snapshot. If the SQL was paginated, preview rows may be only the current page."
    });

    serde_json::to_string_pretty(&summary).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_top_level_tables_keeps_qualified_trino_names() {
        let sql = r#"
WITH recent AS (
  SELECT *
  FROM iceberg.mps_prod.business_order
)
SELECT *
FROM recent r
JOIN `hive`.`ods`.`order_item` oi ON oi.order_id = r.order_id
JOIN "iceberg"."dim"."user_profile" u ON u.user_id = r.user_id
"#;

        let tables = extract_top_level_tables(sql);

        assert!(tables.contains(&"iceberg.mps_prod.business_order".to_string()));
        assert!(tables.contains(&"hive.ods.order_item".to_string()));
        assert!(tables.contains(&"iceberg.dim.user_profile".to_string()));
    }

    #[test]
    fn schema_discovery_prioritizes_qualified_tables_and_preserves_order() {
        let tables = prioritize_schema_discovery_tables(
            vec![
                "first_cte".to_string(),
                "mps_prod.orders".to_string(),
                "other_cte".to_string(),
                "ads.metrics".to_string(),
                "MPS_PROD.ORDERS".to_string(),
            ],
            3,
        );

        assert_eq!(
            tables,
            vec![
                "mps_prod.orders".to_string(),
                "ads.metrics".to_string(),
                "first_cte".to_string(),
            ]
        );
    }
}
