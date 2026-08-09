//! MySQL / PostgreSQL row-cell decoders that preserve type fidelity for the API layer.
//!
//! Extracted from `mod.rs` so result-marshalling lives in one place. Decoders are exhaustive
//! over the type names the respective sqlx driver reports; unknown types fall back to a string
//! cast. Numeric precision is preserved by stringifying `DECIMAL` / `NUMERIC` — `serde_json`'s
//! default `Number` would silently round values like `0.79329000` on the UI side.
//!
//! The simplified [`decode_pg_cell`] is kept as a separate entry point because some callers
//! cannot or should not invoke the type-aware [`decode_postgres_cell`] (e.g. early helper code
//! that only handles a handful of common types).

use sqlx::{Column, Row, TypeInfo};

/// Best-effort PostgreSQL decoder: tries i64 → i32 → f64 → String. Used by older call sites
/// that only need the four most common scalar types.
pub fn decode_pg_cell(row: &sqlx::postgres::PgRow, i: usize) -> serde_json::Value {
    if let Ok(v) = row.try_get::<Option<i64>, _>(i) {
        return v
            .map(|n| serde_json::json!(n))
            .unwrap_or(serde_json::Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(i) {
        return v
            .map(|n| serde_json::json!(n))
            .unwrap_or(serde_json::Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(i) {
        return v
            .map(|n| serde_json::json!(n))
            .unwrap_or(serde_json::Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<String>, _>(i) {
        return v
            .map(|s| serde_json::json!(s))
            .unwrap_or(serde_json::Value::Null);
    }
    serde_json::Value::Null
}

/// Type-aware MySQL cell decoder. Switches on `column.type_info().name()` and decodes into
/// the appropriate Rust type before round-tripping through serde_json. Numeric columns are
/// preserved with full precision; binary blobs are hex-encoded as `0x...` strings.
#[allow(dead_code)]
pub fn decode_mysql_cell(row: &sqlx::mysql::MySqlRow, i: usize) -> serde_json::Value {
    let col = &row.columns()[i];
    let ty = col.type_info().name();

    match ty {
        // Integer-ish
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => row
            .try_get::<Option<i64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
        | "BIGINT UNSIGNED" | "BIT" => row
            .try_get::<Option<u64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "FLOAT" | "DOUBLE" => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        // DECIMAL / NUMERIC — keep full precision by rendering as a string.
        "DECIMAL" | "NUMERIC" | "NEWDECIMAL" => row
            .try_get::<Option<sqlx::types::BigDecimal>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "DATETIME" | "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or_else(|| {
                row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(i)
                    .ok()
                    .flatten()
                    .map(|v| serde_json::json!(v.to_rfc3339()))
                    .unwrap_or(serde_json::Value::Null)
            }),
        "BOOLEAN" => row
            .try_get::<Option<bool>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "JSON" => row
            .try_get::<Option<serde_json::Value>, _>(i)
            .ok()
            .flatten()
            .unwrap_or(serde_json::Value::Null),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" | "GEOMETRY" => {
            row.try_get::<Option<Vec<u8>>, _>(i)
                .ok()
                .flatten()
                .map(|v| serde_json::json!(format!("0x{}", hex::encode(v))))
                .unwrap_or(serde_json::Value::Null)
        }
        // VARCHAR, CHAR, TEXT, ENUM, SET, UUID, and anything else — treat as string.
        _ => row
            .try_get::<Option<String>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
    }
}

/// Type-aware PostgreSQL cell decoder. Same precision-preserving strategy as
/// [`decode_mysql_cell`] for `NUMERIC` / `DECIMAL`; UUIDs are stringified; JSON columns are
/// passed through; bytea blobs are hex-encoded.
#[allow(dead_code)]
pub fn decode_postgres_cell(row: &sqlx::postgres::PgRow, i: usize) -> serde_json::Value {
    let col = &row.columns()[i];
    let ty = col.type_info().name();

    match ty {
        "INT2" | "INT4" | "INT8" => row
            .try_get::<Option<i64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "FLOAT4" | "FLOAT8" => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "NUMERIC" => row
            .try_get::<Option<sqlx::types::BigDecimal>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "BOOL" => row
            .try_get::<Option<bool>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_rfc3339()))
            .unwrap_or(serde_json::Value::Null),
        "UUID" => row
            .try_get::<Option<uuid::Uuid>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "JSON" | "JSONB" => row
            .try_get::<Option<serde_json::Value>, _>(i)
            .ok()
            .flatten()
            .unwrap_or(serde_json::Value::Null),
        "BYTEA" => row
            .try_get::<Option<Vec<u8>>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(format!("0x{}", hex::encode(v))))
            .unwrap_or(serde_json::Value::Null),
        _ => row
            .try_get::<Option<String>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
    }
}
