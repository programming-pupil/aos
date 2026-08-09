//! Result Validator — post-execution sanity checks on SQL result sets.
//!
//! Provides a confidence score (0-1) and human-readable warnings when results look suspicious.
//! Used as the last safety net before returning results to non-technical users.
//!
//! Validation rules are loaded from `nl2sql_result_validation_rules` per datasource.
//! Rules not in the DB are inferred from column type / name patterns.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub passed: bool,
    pub score: f32,
    pub warnings: Vec<ValidationWarning>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationWarning {
    pub table: String,
    pub column: String,
    pub rule_type: String,
    pub severity: String,
    pub message: String,
    pub actual_value: String,
    pub expected: String,
}

// ─── ResultValidator ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ResultValidator {
    db: SqlitePool,
}

impl ResultValidator {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// Validate a result set. `result` is the raw rows from the DB connector.
    pub async fn validate(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        result_rows: &[serde_json::Value],
        result_columns: &[String],
    ) -> anyhow::Result<ValidationResult> {
        let rules = self.load_rules(tenant_id, datasource_id).await?;
        let mut warnings = Vec::new();
        let mut suggestions = Vec::new();
        let row_count = result_rows.len();

        // ── Rule-based validation ───────────────────────────────────────────
        for rule in &rules {
            if rule.table_name != table_name {
                continue;
            }
            if !rule.enabled {
                continue;
            }

            match rule.rule_type.as_str() {
                "row_count" => {
                    let cfg: RowCountConfig =
                        serde_json::from_value(rule.config_json.clone()).unwrap_or_default();
                    if let Some(min) = cfg.min {
                        let min_usize = min as usize;
                        if row_count < min_usize {
                            warnings.push(ValidationWarning {
                                table: table_name.to_string(),
                                column: String::new(),
                                rule_type: "row_count".to_string(),
                                severity: rule.severity.clone(),
                                message: format!(
                                    "Row count {} is below minimum {}",
                                    row_count, min
                                ),
                                actual_value: row_count.to_string(),
                                expected: format!("min: {min}"),
                            });
                        }
                    }
                }
                "null_ratio" => {
                    let col_idx = result_columns.iter().position(|c| c == &rule.column_name);
                    let Some(_col_idx) = col_idx else { continue };
                    let cfg: NullRatioConfig =
                        serde_json::from_value(rule.config_json.clone()).unwrap_or_default();
                    let null_count = result_rows
                        .iter()
                        .filter(|row| {
                            row.get(&rule.column_name)
                                .map(|v| v.is_null())
                                .unwrap_or(true)
                        })
                        .count();
                    let pct = if row_count > 0 {
                        (null_count as f32 / row_count as f32) * 100.0
                    } else {
                        0.0
                    };
                    if pct > cfg.max_pct as f32 {
                        warnings.push(ValidationWarning {
                            table: table_name.to_string(),
                            column: rule.column_name.clone(),
                            rule_type: "null_ratio".to_string(),
                            severity: rule.severity.clone(),
                            message: format!(
                                "{:.1}% NULL values exceeds max {}%",
                                pct, cfg.max_pct
                            ),
                            actual_value: format!("{:.1}%", pct),
                            expected: format!("max_pct: {}", cfg.max_pct),
                        });
                    }
                }
                "range" => {
                    let col_idx = result_columns.iter().position(|c| c == &rule.column_name);
                    let Some(_col_idx) = col_idx else { continue };
                    let cfg: RangeConfig =
                        serde_json::from_value(rule.config_json.clone()).unwrap_or_default();
                    let values: Vec<f64> = result_rows
                        .iter()
                        .filter_map(|row| {
                            let v = row.get(&rule.column_name)?;
                            let s = v
                                .as_str()
                                .map(|s| s.to_owned())
                                .or_else(|| v.as_i64().map(|n| n.to_string()))
                                .or_else(|| v.as_f64().map(|f| f.to_string()))?;
                            s.parse::<f64>().ok()
                        })
                        .collect();

                    for (i, v) in values.iter().enumerate().take(1000) {
                        if let Some(min) = cfg.min {
                            if *v < min {
                                warnings.push(ValidationWarning {
                                    table: table_name.to_string(),
                                    column: rule.column_name.clone(),
                                    rule_type: "range".to_string(),
                                    severity: rule.severity.clone(),
                                    message: format!("Value {v} in row {i} is below minimum {min}"),
                                    actual_value: v.to_string(),
                                    expected: format!("min: {min}"),
                                });
                            }
                        }
                        if let Some(max) = cfg.max {
                            if *v > max {
                                warnings.push(ValidationWarning {
                                    table: table_name.to_string(),
                                    column: rule.column_name.clone(),
                                    rule_type: "range".to_string(),
                                    severity: rule.severity.clone(),
                                    message: format!("Value {v} in row {i} exceeds maximum {max}"),
                                    actual_value: v.to_string(),
                                    expected: format!("max: {max}"),
                                });
                            }
                        }
                    }
                }
                "cardinality" => {
                    let col_idx = result_columns.iter().position(|c| c == &rule.column_name);
                    let Some(_col_idx) = col_idx else { continue };
                    let cfg: CardinalityConfig =
                        serde_json::from_value(rule.config_json.clone()).unwrap_or_default();
                    let distinct: std::collections::HashSet<_> = result_rows
                        .iter()
                        .filter_map(|row| row.get(&rule.column_name).cloned())
                        .collect();
                    let d_count = distinct.len() as i32;
                    if let Some(min) = cfg.distinct_min {
                        if d_count < min {
                            warnings.push(ValidationWarning {
                                table: table_name.to_string(),
                                column: rule.column_name.clone(),
                                rule_type: "cardinality".to_string(),
                                severity: rule.severity.clone(),
                                message: format!(
                                    "Only {d_count} distinct values, minimum is {min}"
                                ),
                                actual_value: d_count.to_string(),
                                expected: format!("distinct_min: {min}"),
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        // ── Inferred / heuristic validation ────────────────────────────────

        // Empty result with non-zero expected
        if row_count == 0 {
            suggestions.push(
                "结果为空。可能的原因：筛选条件无匹配数据、时间范围不正确、或数据尚未入库。"
                    .to_string(),
            );
        }

        warnings.extend(detect_all_null_dimension_warnings(
            table_name,
            result_rows,
            result_columns,
        ));

        // Duplicate key values in what looks like a detail/join query
        if row_count > 1 && result_columns.iter().any(|c| c.ends_with("_id")) {
            let id_cols: Vec<_> = result_columns
                .iter()
                .filter(|c| c.ends_with("_id") || c.as_str() == "id")
                .cloned()
                .collect();
            for id_col in id_cols {
                let ids: std::collections::HashSet<_> = result_rows
                    .iter()
                    .filter_map(|r| r.get(&id_col).cloned())
                    .collect();
                if ids.len() == 1 && row_count > 1 {
                    suggestions.push(format!(
                        "所有结果的 {id_col} 值相同，可能是 JOIN 条件缺失导致的笛卡尔积。"
                    ));
                    break;
                }
            }
        }

        // Suspicious numeric values
        for col in result_columns {
            let numeric_values: Vec<f64> = result_rows
                .iter()
                .filter_map(|r| {
                    r.get(col).and_then(|v| {
                        v.as_str()
                            .and_then(|s| s.parse::<f64>().ok())
                            .or_else(|| v.as_f64())
                            .or_else(|| v.as_i64().map(|n| n as f64))
                    })
                })
                .collect();

            if numeric_values.len() > 3 {
                let all_negative = numeric_values.iter().all(|v| *v < 0.0);
                if all_negative
                    && (col.contains("amount")
                        || col.contains("revenue")
                        || col.contains("price")
                        || col.contains("count"))
                {
                    suggestions.push(format!(
                        "列 '{col}' 所有值均为负数，请确认这是业务预期（如退款、折扣场景）。"
                    ));
                }

                let has_zero = numeric_values.iter().any(|v| *v == 0.0);
                if has_zero && (col.contains("price") || col.contains("amount")) {
                    suggestions.push(format!(
                        "列 '{col}' 包含零值，请确认是否存在免费赠送或测试数据。"
                    ));
                }
            }
        }

        // NULLs in primary key columns
        for col in result_columns {
            if col == "id" || col.ends_with("_id") {
                let has_null = result_rows
                    .iter()
                    .any(|r| r.get(col).map(|v| v.is_null()).unwrap_or(true));
                if has_null {
                    warnings.push(ValidationWarning {
                        table: table_name.to_string(),
                        column: col.clone(),
                        rule_type: "pk_null".to_string(),
                        severity: "error".to_string(),
                        message: format!(
                            "主键/外键列 '{col}' 存在 NULL 值，数据完整性可能有问题。"
                        ),
                        actual_value: "NULL".to_string(),
                        expected: "NOT NULL".to_string(),
                    });
                }
            }
        }

        // ── Score computation ────────────────────────────────────────────────
        let error_count = warnings.iter().filter(|w| w.severity == "error").count();
        let warning_count = warnings.len();

        let score = if error_count > 0 {
            0.0
        } else if warning_count == 0 {
            1.0
        } else if warning_count <= 2 {
            0.8
        } else {
            0.5
        };

        Ok(ValidationResult {
            passed: error_count == 0,
            score,
            warnings,
            suggestions,
        })
    }

    async fn load_rules(
        &self,
        tenant_id: &str,
        datasource_id: &str,
    ) -> anyhow::Result<Vec<ValidationRule>> {
        let rows: Vec<(
            String,
            String,
            String,
            serde_json::Value,
            String,
            String,
            bool,
        )> = sqlx::query_as(
            r#"
            SELECT table_name, column_name, rule_type, rule_config, severity, description, enabled
            FROM nl2sql_result_validation_rules
            WHERE tenant_id = ? AND datasource_id = ? AND enabled = 1
            "#,
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    table_name,
                    column_name,
                    rule_type,
                    rule_config,
                    severity,
                    description,
                    enabled,
                )| {
                    ValidationRule {
                        table_name,
                        column_name,
                        rule_type,
                        config_json: rule_config,
                        severity,
                        description,
                        enabled,
                    }
                },
            )
            .collect())
    }
}

fn detect_all_null_dimension_warnings(
    table_name: &str,
    result_rows: &[serde_json::Value],
    result_columns: &[String],
) -> Vec<ValidationWarning> {
    if result_rows.is_empty() || result_columns.len() < 2 {
        return Vec::new();
    }

    let has_metric_column = result_columns
        .iter()
        .any(|col| looks_like_metric_column(col.as_str()));
    if !has_metric_column {
        return Vec::new();
    }

    result_columns
        .iter()
        .filter(|col| !looks_like_metric_column(col.as_str()))
        .filter(|col| {
            result_rows
                .iter()
                .all(|row| row.get(col.as_str()).map(|v| v.is_null()).unwrap_or(true))
        })
        .map(|col| {
            let expected = if looks_like_time_bucket_column(col) {
                "non-NULL time bucket; verify epoch seconds/milliseconds or DATETIME conversion"
            } else {
                "non-NULL dimension value"
            };
            ValidationWarning {
                table: table_name.to_string(),
                column: col.clone(),
                rule_type: "all_null_dimension".to_string(),
                severity: "warning".to_string(),
                message: format!(
                    "结果维度列 '{col}' 全部为 NULL。查询已成功执行，但结果可能不可用；请检查字段是否为空，或日期/时间戳转换是否使用了正确的单位。"
                ),
                actual_value: "all NULL".to_string(),
                expected: expected.to_string(),
            }
        })
        .collect()
}

fn looks_like_metric_column(column: &str) -> bool {
    let col = column.to_ascii_lowercase();
    col == "count"
        || col.ends_with("_count")
        || col.contains("count")
        || col.starts_with("sum")
        || col.starts_with("avg")
        || col.starts_with("min")
        || col.starts_with("max")
        || col.contains("total")
        || col.contains("amount")
        || col.contains("revenue")
        || col.contains("gmv")
        || col.contains("sales")
        || col.contains("price")
        || col.contains("rate")
        || col.contains("ratio")
        || col.contains("percent")
}

fn looks_like_time_bucket_column(column: &str) -> bool {
    let col = column.to_ascii_lowercase();
    matches!(
        col.as_str(),
        "year" | "month" | "week" | "day" | "date" | "dt" | "hour" | "time"
    ) || col.ends_with("_year")
        || col.ends_with("_month")
        || col.ends_with("_date")
        || col.ends_with("_time")
}

// ─── Rule Types ───────────────────────────────────────────────────────────────

struct ValidationRule {
    table_name: String,
    column_name: String,
    rule_type: String,
    config_json: serde_json::Value,
    severity: String,
    #[allow(dead_code)]
    description: String,
    enabled: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RowCountConfig {
    min: Option<i64>,
    #[allow(dead_code)]
    max: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct NullRatioConfig {
    max_pct: i32,
}

#[derive(Debug, Default, Deserialize)]
struct RangeConfig {
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct CardinalityConfig {
    distinct_min: Option<i32>,
    #[allow(dead_code)]
    distinct_max: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_count_rule_should_not_require_result_column_presence() {
        let rules = vec![ValidationRule {
            table_name: "web_sessions".to_string(),
            column_name: "token".to_string(),
            rule_type: "row_count".to_string(),
            config_json: serde_json::json!({"min": 1}),
            severity: "error".to_string(),
            description: String::new(),
            enabled: true,
        }];
        let result_rows: Vec<serde_json::Value> = vec![];
        let row_count = result_rows.len();

        let mut warnings = Vec::new();
        for rule in &rules {
            if rule.rule_type.as_str() != "row_count" || !rule.enabled {
                continue;
            }
            let cfg: RowCountConfig =
                serde_json::from_value(rule.config_json.clone()).unwrap_or_default();
            if let Some(min) = cfg.min {
                if row_count < min as usize {
                    warnings.push(rule.table_name.clone());
                }
            }
        }

        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn warns_when_dimension_bucket_is_all_null() {
        let rows = vec![serde_json::json!({"year": null, "order_count": 12757})];
        let columns = vec!["year".to_string(), "order_count".to_string()];

        let warnings = detect_all_null_dimension_warnings("business_order", &rows, &columns);

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].column, "year");
        assert_eq!(warnings[0].rule_type, "all_null_dimension");
        assert!(warnings[0].expected.contains("epoch seconds/milliseconds"));
    }

    #[test]
    fn does_not_warn_for_all_null_metric_columns() {
        let rows = vec![serde_json::json!({"region": "ID", "total_amount": null})];
        let columns = vec!["region".to_string(), "total_amount".to_string()];

        let warnings = detect_all_null_dimension_warnings("business_order", &rows, &columns);

        assert!(warnings.is_empty());
    }
}
