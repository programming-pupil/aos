//! In-memory step-result merge strategies — hash join, outer join, union, cross product.
//!
//! Kept in `nl2sql-domain` so the multi-step agent orchestrator can reuse and
//! test merge semantics without pulling in SQL engines, Axum, or route handlers.

#![allow(clippy::collapsible_if, clippy::needless_borrow)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeInput {
    pub input_name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlExecutionAttempt {
    pub attempt: usize,
    pub status: String,
    pub sql: String,
    pub execution_ms: u64,
    pub error: Option<String>,
    pub retry_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_strategy: Option<String>,
    #[serde(default)]
    pub scope_changed: bool,
    #[serde(default)]
    pub diagnostic_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub step_id: usize,
    pub output_name: String,
    pub sql: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub row_count: usize,
    pub execution_ms: u64,
    pub error: Option<String>,
    pub datasource_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_attempts: Vec<SqlExecutionAttempt>,
    #[serde(default)]
    pub diagnostic_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_note: Option<String>,
}

/// Pure in-memory hash join. Takes two step results and join keys.
/// If `is_left_join` is true, keeps left rows with no match (fills right cols with null).
pub fn hash_join(
    inputs: &[MergeInput],
    intermediate: &std::collections::HashMap<String, StepResult>,
    on: &[String],
    is_left_join: bool,
    max_cross_ds_rows: usize,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    if inputs.len() != 2 {
        return Err(anyhow::anyhow!(
            "hash join requires exactly 2 inputs, got {}",
            inputs.len()
        ));
    }

    let left_name = &inputs[0].input_name;
    let right_name = &inputs.get(1).map(|i| &i.input_name).unwrap_or(left_name);

    let left = intermediate.get(left_name).ok_or_else(|| {
        anyhow::anyhow!("input '{}' not found in intermediate results", left_name)
    })?;
    let right = intermediate.get(right_name.as_str());

    // Build right hash index on join keys, excluding NULL keys entirely.
    // In SQL, NULL = NULL is NULL (not TRUE), so NULL join keys never match.
    let right_map: std::collections::HashMap<String, Vec<&serde_json::Value>> =
        if let Some(right) = right {
            let mut map: std::collections::HashMap<String, Vec<&serde_json::Value>> =
                std::collections::HashMap::new();
            for row in &right.rows {
                let key = join_key_values(row, on)?;
                let key_str = join_key_str(on, row)?;
                if !key_str.contains("__null__") {
                    map.entry(key).or_default().push(row);
                }
            }
            map
        } else {
            std::collections::HashMap::new()
        };

    let mut output_columns: Vec<String> = left.columns.clone();
    if let Some(right) = right {
        for col in &right.columns {
            if !output_columns.contains(col) {
                output_columns.push(col.clone());
            }
        }
    }

    let mut output_rows: Vec<serde_json::Value> = Vec::new();
    let cap = max_cross_ds_rows;

    for left_row in &left.rows {
        // Skip rows with NULL on any join key — NULL never matches in SQL.
        if join_key_has_null(left_row, on) {
            if is_left_join {
                let merged = fill_null_columns(
                    &output_columns,
                    left_row,
                    right.map(|r| &r.columns).map(|v| &**v),
                );
                output_rows.push(serde_json::Value::Object(merged));
            }
            continue;
        }

        let key = join_key_values(left_row, on)?;
        let matches = right_map.get(&key);

        if let Some(right_matches) = matches {
            for right_row in right_matches {
                let merged = merge_rows_fn(&output_columns, left_row, right_row);
                output_rows.push(serde_json::Value::Object(merged));
            }
        } else if is_left_join {
            let merged = fill_null_columns(
                &output_columns,
                left_row,
                right.map(|r| &r.columns).map(|v| &**v),
            );
            output_rows.push(serde_json::Value::Object(merged));
        }

        if output_rows.len() >= cap {
            output_rows.truncate(cap);
            return Ok((output_columns, output_rows));
        }
    }

    Ok((output_columns, output_rows))
}

/// Concatenate rows from multiple inputs with matching columns.
pub fn union_all(
    inputs: &[MergeInput],
    intermediate: &std::collections::HashMap<String, StepResult>,
    max_cross_ds_rows: usize,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    let mut all_rows: Vec<serde_json::Value> = Vec::new();
    let mut columns: Vec<String> = Vec::new();
    let cap = max_cross_ds_rows;

    for input in inputs {
        let result = intermediate
            .get(&input.input_name)
            .ok_or_else(|| anyhow::anyhow!("input '{}' not found", input.input_name))?;

        if columns.is_empty() {
            columns = result.columns.clone();
        }

        for row in &result.rows {
            if all_rows.len() >= cap {
                all_rows.truncate(cap);
                return Ok((columns, all_rows));
            }
            all_rows.push(row.clone());
        }
    }

    Ok((columns, all_rows))
}

/// Union distinct: concatenate rows from multiple inputs and deduplicate.
pub fn union_distinct(
    inputs: &[MergeInput],
    intermediate: &std::collections::HashMap<String, StepResult>,
    max_cross_ds_rows: usize,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    let mut all_rows: Vec<serde_json::Value> = Vec::new();
    let mut columns: Vec<String> = Vec::new();
    let cap = max_cross_ds_rows;

    for input in inputs {
        let result = intermediate
            .get(&input.input_name)
            .ok_or_else(|| anyhow::anyhow!("input '{}' not found", input.input_name))?;

        if columns.is_empty() {
            columns = result.columns.clone();
        }

        for row in &result.rows {
            if all_rows.len() >= cap {
                all_rows.truncate(cap);
                return Ok((columns, all_rows));
            }
            if !all_rows.contains(row) {
                all_rows.push(row.clone());
            }
        }
    }

    Ok((columns, all_rows))
}

/// Right join: hash-join two inputs, keeping all right rows (with NULL-filled left columns for unmatched).
pub fn right_join(
    inputs: &[MergeInput],
    intermediate: &std::collections::HashMap<String, StepResult>,
    on: &[String],
    max_cross_ds_rows: usize,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    if inputs.len() != 2 {
        return Err(anyhow::anyhow!(
            "right join requires exactly 2 inputs, got {}",
            inputs.len()
        ));
    }

    let left_name = &inputs[0].input_name;
    let right_name = &inputs.get(1).map(|i| &i.input_name).unwrap_or(left_name);
    let left = intermediate.get(left_name).ok_or_else(|| {
        anyhow::anyhow!("input '{}' not found in intermediate results", left_name)
    })?;
    let right = match intermediate.get(right_name.as_str()) {
        Some(r) => r,
        None => {
            return Err(anyhow::anyhow!(
                "right input '{}' not found in intermediate results",
                right_name
            ));
        }
    };

    // Build left hash index (mirrors hash_join but with roles swapped)
    let left_map: std::collections::HashMap<String, Vec<&serde_json::Value>> = {
        let mut map: std::collections::HashMap<String, Vec<&serde_json::Value>> =
            std::collections::HashMap::new();
        for row in &left.rows {
            let key = join_key_values(row, on)?;
            let key_str = join_key_str(on, row)?;
            if !key_str.contains("__null__") {
                map.entry(key).or_default().push(row);
            }
        }
        map
    };

    // Output columns: right + left (only non-overlapping)
    let mut output_columns: Vec<String> = right.columns.clone();
    for col in &left.columns {
        if !output_columns.contains(col) {
            output_columns.push(col.clone());
        }
    }

    let mut output_rows: Vec<serde_json::Value> = Vec::new();
    let cap = max_cross_ds_rows;

    for right_row in &right.rows {
        let key = join_key_values(right_row, on)?;
        let key_str = join_key_str(on, right_row)?;
        let has_null = key_str.contains("__null__");

        if has_null {
            // NULL join key: emit right row with NULL-filled left columns
            let merged = fill_null_columns(&output_columns, right_row, Some(&left.columns));
            output_rows.push(serde_json::Value::Object(merged));
        } else if let Some(left_matches) = left_map.get(&key) {
            for left_row in left_matches {
                let merged = merge_rows_fn(&output_columns, left_row, right_row);
                output_rows.push(serde_json::Value::Object(merged));
            }
        } else {
            // Right row with no left match: NULL-filled left columns
            let merged = fill_null_columns(&output_columns, right_row, Some(&left.columns));
            output_rows.push(serde_json::Value::Object(merged));
        }

        if output_rows.len() >= cap {
            output_rows.truncate(cap);
            return Ok((output_columns, output_rows));
        }
    }

    Ok((output_columns, output_rows))
}

/// Full outer join: keep all rows from both inputs.
pub fn full_outer_join(
    inputs: &[MergeInput],
    intermediate: &std::collections::HashMap<String, StepResult>,
    on: &[String],
    max_cross_ds_rows: usize,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    if inputs.len() != 2 {
        return Err(anyhow::anyhow!(
            "full outer join requires exactly 2 inputs, got {}",
            inputs.len()
        ));
    }

    let left_name = &inputs[0].input_name;
    let right_name = &inputs.get(1).map(|i| &i.input_name).unwrap_or(left_name);
    let left = intermediate.get(left_name).ok_or_else(|| {
        anyhow::anyhow!("input '{}' not found in intermediate results", left_name)
    })?;
    let right = match intermediate.get(right_name.as_str()) {
        Some(r) => r,
        None => {
            return Err(anyhow::anyhow!(
                "right input '{}' not found in intermediate results",
                right_name
            ));
        }
    };

    // Build right hash index
    let right_map: std::collections::HashMap<String, Vec<&serde_json::Value>> = {
        let mut map: std::collections::HashMap<String, Vec<&serde_json::Value>> =
            std::collections::HashMap::new();
        for row in &right.rows {
            let key = join_key_values(row, on)?;
            let key_str = join_key_str(on, row)?;
            if !key_str.contains("__null__") {
                map.entry(key).or_default().push(row);
            }
        }
        map
    };

    // Output columns
    let mut output_columns: Vec<String> = left.columns.clone();
    for col in &right.columns {
        if !output_columns.contains(col) {
            output_columns.push(col.clone());
        }
    }

    let mut output_rows: Vec<serde_json::Value> = Vec::new();
    let cap = max_cross_ds_rows;

    // Track which right keys found a match so we can emit unmatched right rows
    let mut right_matched_keys: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Left rows
    for left_row in &left.rows {
        let key = join_key_values(left_row, on)?;
        let key_str = join_key_str(on, left_row)?;
        let has_null = key_str.contains("__null__");

        if has_null {
            let merged = fill_null_columns(&output_columns, left_row, Some(&right.columns));
            output_rows.push(serde_json::Value::Object(merged));
        } else if let Some(right_matches) = right_map.get(&key) {
            right_matched_keys.insert(key.clone());
            for right_row in right_matches {
                let merged = merge_rows_fn(&output_columns, left_row, right_row);
                output_rows.push(serde_json::Value::Object(merged));
            }
        } else {
            let merged = fill_null_columns(&output_columns, left_row, Some(&right.columns));
            output_rows.push(serde_json::Value::Object(merged));
        }

        if output_rows.len() >= cap {
            output_rows.truncate(cap);
            return Ok((output_columns, output_rows));
        }
    }

    // Right rows with no left match (key not in right_matched_keys)
    for right_row in &right.rows {
        let key = join_key_values(right_row, on)?;
        let key_str = join_key_str(on, right_row)?;
        let has_null = key_str.contains("__null__");

        if has_null || !right_matched_keys.contains(&key) {
            let merged = fill_null_columns(&output_columns, right_row, Some(&left.columns));
            output_rows.push(serde_json::Value::Object(merged));
        }

        if output_rows.len() >= cap {
            output_rows.truncate(cap);
            return Ok((output_columns, output_rows));
        }
    }

    Ok((output_columns, output_rows))
}

/// Cross join: Cartesian product of all inputs, capped to prevent row explosion.
pub fn cross_join(
    inputs: &[MergeInput],
    intermediate: &std::collections::HashMap<String, StepResult>,
    max_cross_ds_rows: usize,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    if inputs.is_empty() {
        return Ok((vec![], vec![]));
    }

    // Gather all inputs' results
    let inputs_data: Vec<_> = inputs
        .iter()
        .map(|input| {
            intermediate.get(&input.input_name).ok_or_else(|| {
                anyhow::anyhow!(
                    "input '{}' not found in intermediate results",
                    input.input_name
                )
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Collect all columns (union of all inputs)
    let mut output_columns: Vec<String> = Vec::new();
    for result in &inputs_data {
        for col in &result.columns {
            if !output_columns.contains(col) {
                output_columns.push(col.clone());
            }
        }
    }

    // Start with all rows from the first input
    let mut output_rows: Vec<serde_json::Value> = inputs_data
        .first()
        .map(|r| {
            r.rows
                .iter()
                .filter_map(|row| {
                    let obj = row.as_object()?;
                    let mut m = serde_json::Map::new();
                    for col in &output_columns {
                        if let Some(v) = obj.get(col) {
                            m.insert(col.clone(), v.clone());
                        }
                    }
                    Some(serde_json::Value::Object(m))
                })
                .collect()
        })
        .unwrap_or_default();

    let cap = max_cross_ds_rows;

    // Iteratively cross-join remaining inputs
    for result in inputs_data.iter().skip(1) {
        if output_rows.is_empty() {
            break;
        }
        let mut new_rows: Vec<serde_json::Value> = Vec::new();

        for left_row in &output_rows {
            let left_map = match left_row {
                serde_json::Value::Object(m) => m,
                _ => continue,
            };

            for right_row in &result.rows {
                let right_map = match right_row {
                    serde_json::Value::Object(m) => m,
                    _ => continue,
                };

                let mut merged = left_map.clone();
                for col in &result.columns {
                    if !merged.contains_key(col) {
                        merged.insert(
                            col.clone(),
                            right_map
                                .get(col)
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        );
                    }
                }

                new_rows.push(serde_json::Value::Object(merged));

                if new_rows.len() >= cap {
                    new_rows.truncate(cap);
                    return Ok((output_columns, new_rows));
                }
            }
        }

        output_rows = new_rows;
        if output_rows.is_empty() {
            break;
        }
    }

    Ok((output_columns, output_rows))
}

/// Extract join key values from a row as a string key.
pub fn join_key_values(row: &serde_json::Value, keys: &[String]) -> anyhow::Result<String> {
    let obj = row
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("join key row must be an object"))?;
    let mut parts = Vec::with_capacity(keys.len());
    for k in keys {
        let v = obj
            .get(k)
            .ok_or_else(|| anyhow::anyhow!("join key '{}' not found in row", k))?;
        parts.push(json_value_key(v));
    }
    Ok(parts.join("\x00"))
}

/// Check whether any join key column in the row is NULL.
pub fn join_key_has_null(row: &serde_json::Value, keys: &[String]) -> bool {
    let Some(obj) = row.as_object() else {
        return false;
    };
    for k in keys {
        if let Some(v) = obj.get(k) {
            if matches!(v, serde_json::Value::Null) {
                return true;
            }
        }
    }
    false
}

/// Build a join key string for NULL-checking without repeating work.
pub fn join_key_str(keys: &[String], row: &serde_json::Value) -> anyhow::Result<String> {
    join_key_values(row, keys)
}

/// Convert a JSON value to a comparable string key for hashing.
/// Uses a type prefix so that "1" (number) and "1" (string) produce different keys,
/// and NULL values are distinguishable from the string "NULL".
pub fn json_value_key(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "__null__".to_string(),
        serde_json::Value::Bool(b) => format!("__bool__{}", b),
        serde_json::Value::Number(n) => format!("__num__{}", n),
        serde_json::Value::String(s) => format!("__str__{}", s),
        _ => format!("__other__{}", serde_json::to_string(v).unwrap_or_default()),
    }
}

/// Merge two rows into a single object with all columns.
pub fn merge_rows_fn(
    output_columns: &[String],
    left: &serde_json::Value,
    right: &serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    let left_obj = left.as_object().cloned().unwrap_or_default();
    let right_obj = right.as_object().cloned().unwrap_or_default();
    let mut merged = serde_json::Map::new();
    for col in output_columns {
        let val = left_obj
            .get(col)
            .cloned()
            .or_else(|| right_obj.get(col).cloned())
            .unwrap_or(serde_json::Value::Null);
        merged.insert(col.clone(), val);
    }
    merged
}

/// Fill missing columns with null values.
pub fn fill_null_columns(
    _output_columns: &[String],
    row: &serde_json::Value,
    right_columns: Option<&[String]>,
) -> serde_json::Map<String, serde_json::Value> {
    let row_obj = row.as_object().cloned().unwrap_or_default();
    let mut merged = row_obj;
    if let Some(right_cols) = right_columns {
        for col in right_cols {
            if !merged.contains_key(col) {
                merged.insert(col.clone(), serde_json::Value::Null);
            }
        }
    }
    merged
}
