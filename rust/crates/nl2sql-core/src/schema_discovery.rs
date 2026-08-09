//! Live schema discovery from source databases.
//!
//! One authoritative implementation that both the interactive
//! `POST /data-sources/:id/discover` endpoint and the periodic
//! scheduler delegate to. Previously these two paths had drifted:
//! the scheduler had `LIMIT 1000`, no retry, and the Trino path
//! silently replaced a failed `DESCRIBE` with an empty dataset —
//! losing every column of a flaky table.
//!
//! Contract:
//!   * Returns the full live schema as a `Vec<serde_json::Value>` shaped
//!     like `[{table_name, columns: [{name, type, nullable}]}]`.
//!   * Foreign key relationships are also discovered and returned as
//!     `foreign_keys: [{source_table, source_column, target_table, target_column}]`
//!     to enable the NL2SQL prompt builder to generate correct JOINs.
//!   * Per-table metadata fetches are retried with exponential backoff
//!     before being skipped; skipped tables are reported so the caller
//!     can warn operators rather than silently dropping them.
//!   * Honours `NL2SQL_MAX_TABLES_PER_DATASOURCE` (default 100_000) as
//!     a cap to protect the system from pathological schemas. Trino/Presto
//!     uses `SHOW TABLES IN catalog.schema` with a smaller default via
//!     `NL2SQL_TRINO_MAX_TABLES_PER_DATASOURCE` because Hive Metastore-backed
//!     schemas can be extremely large. When the cap is hit we log a warning
//!     with the database name.
//!   * Manual tables are **not** handled here; the caller merges them
//!     back into `schema_info` via [`schema_diff::extract_manual_tables`].

use std::collections::BTreeMap;
use std::time::Duration;

use crate::datasource_config::{
    build_mongodb_uri, build_mysql_url_parts, build_postgres_url_parts, normalize_trino_schemas,
    MongoConfig,
};
use mongodb::bson::{doc, Bson, Document};
use serde_json::{json, Value};

/// Foreign key relationship between two columns.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ForeignKey {
    pub source_table: String,
    pub source_column: String,
    pub target_table: String,
    pub target_column: String,
}

/// Outcome of a live schema discovery pass. `tables` is the list of
/// discovered tables (manual tables excluded — the caller merges them
/// back in). `skipped` records per-table failures that survived the
/// retry budget, so the UI can surface them without the whole call
/// failing. `foreign_keys` lists all FK relationships found.
#[derive(Debug, Default, Clone)]
pub struct DiscoveryOutcome {
    pub tables: Vec<Value>,
    pub skipped: Vec<(String, String)>,
    pub cap_hit: bool,
    /// Foreign key relationships across all discovered tables.
    pub foreign_keys: Vec<ForeignKey>,
}

impl DiscoveryOutcome {
    #[must_use]
    pub fn total_columns(&self) -> usize {
        self.tables
            .iter()
            .map(|t| {
                t.get("columns")
                    .and_then(|c| c.as_array())
                    .map_or(0, Vec::len)
            })
            .sum()
    }
}

/// Upper bound on how many tables we'll introspect per datasource.
/// Tunable via `NL2SQL_MAX_TABLES_PER_DATASOURCE` (default: 100_000).
#[must_use]
pub fn max_tables_per_datasource() -> u64 {
    std::env::var("NL2SQL_MAX_TABLES_PER_DATASOURCE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(100_000)
}

/// Trino/Presto needs a much more conservative default than OLTP databases.
/// Even with `SHOW TABLES`, a Hive-backed schema may still be very large, so
/// 100k can turn a normal "Fetch Schema" click into a heavy metadata scan.
#[must_use]
pub fn max_trino_tables_per_datasource() -> u64 {
    if let Some(v) = std::env::var("NL2SQL_TRINO_MAX_TABLES_PER_DATASOURCE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
    {
        return v;
    }
    max_tables_per_datasource().min(2_000)
}

#[must_use]
pub fn trino_schema_listing_timeout_secs() -> u64 {
    std::env::var("NL2SQL_TRINO_SCHEMA_LIST_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 5)
        .unwrap_or(120)
}

/// Retry budget for per-table DDL introspection. Three attempts,
/// exponential backoff: 0ms, 250ms, 1000ms.
const RETRY_ATTEMPTS: u32 = 3;

const MONGODB_SCHEMA_SAMPLE_SIZE: i64 = 100;
const MONGODB_MAX_NESTING_DEPTH: usize = 5;

async fn backoff(attempt: u32) {
    if attempt == 0 {
        return;
    }
    let n = u64::from(attempt);
    tokio::time::sleep(Duration::from_millis(250 * n * n)).await;
}

// ─── MongoDB ────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct MongoFieldStats {
    observed: usize,
    saw_null: bool,
    types: std::collections::BTreeSet<String>,
}

fn mongodb_type_name(value: &Bson) -> String {
    match value {
        Bson::Double(_) => "DOUBLE".to_string(),
        Bson::String(_) => "STRING".to_string(),
        Bson::Array(values) => {
            let element_types = values
                .iter()
                .map(mongodb_type_name)
                .collect::<std::collections::BTreeSet<_>>();
            if element_types.is_empty() {
                "ARRAY".to_string()
            } else {
                format!(
                    "ARRAY<{}>",
                    element_types.into_iter().collect::<Vec<_>>().join(" | ")
                )
            }
        }
        Bson::Document(_) => "DOCUMENT".to_string(),
        Bson::Boolean(_) => "BOOLEAN".to_string(),
        Bson::Null => "NULL".to_string(),
        Bson::RegularExpression(_) => "REGEX".to_string(),
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "JAVASCRIPT".to_string(),
        Bson::Int32(_) => "INT32".to_string(),
        Bson::Int64(_) => "INT64".to_string(),
        Bson::Timestamp(_) => "TIMESTAMP".to_string(),
        Bson::Binary(_) => "BINARY".to_string(),
        Bson::ObjectId(_) => "OBJECT_ID".to_string(),
        Bson::DateTime(_) => "DATETIME".to_string(),
        Bson::Symbol(_) => "SYMBOL".to_string(),
        Bson::Decimal128(_) => "DECIMAL128".to_string(),
        Bson::Undefined => "UNDEFINED".to_string(),
        Bson::MaxKey => "MAX_KEY".to_string(),
        Bson::MinKey => "MIN_KEY".to_string(),
        Bson::DbPointer(_) => "DB_POINTER".to_string(),
    }
}

fn collect_mongodb_fields(
    document: &Document,
    prefix: &str,
    depth: usize,
    fields: &mut BTreeMap<String, MongoFieldStats>,
) {
    for (name, value) in document {
        let field_name = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        let stats = fields.entry(field_name.clone()).or_default();
        stats.observed += 1;
        stats.saw_null |= matches!(value, Bson::Null);
        stats.types.insert(mongodb_type_name(value));
        if depth < MONGODB_MAX_NESTING_DEPTH {
            if let Bson::Document(nested) = value {
                collect_mongodb_fields(nested, &field_name, depth + 1, fields);
            }
        }
    }
}

async fn mongodb_client(config: &MongoConfig) -> Result<mongodb::Client, String> {
    let uri = build_mongodb_uri(config)?;
    let mut options = mongodb::options::ClientOptions::parse(&uri)
        .await
        .map_err(|error| format!("MongoDB connection configuration failed: {error}"))?;
    options.server_selection_timeout = Some(Duration::from_secs(10));
    options.connect_timeout = Some(Duration::from_secs(10));
    mongodb::Client::with_options(options)
        .map_err(|error| format!("MongoDB client creation failed: {error}"))
}

async fn discover_mongodb_collection(
    database: &mongodb::Database,
    collection_name: &str,
) -> Result<Value, String> {
    let collection = database.collection::<Document>(collection_name);
    let mut cursor = collection
        .find(doc! {})
        .limit(MONGODB_SCHEMA_SAMPLE_SIZE)
        .await
        .map_err(|error| {
            format!("MongoDB collection `{collection_name}` sampling failed: {error}")
        })?;
    let mut fields = BTreeMap::<String, MongoFieldStats>::new();
    let mut sampled = 0usize;
    while cursor
        .advance()
        .await
        .map_err(|error| format!("MongoDB collection `{collection_name}` cursor failed: {error}"))?
    {
        let document = cursor.deserialize_current().map_err(|error| {
            format!("MongoDB collection `{collection_name}` document decode failed: {error}")
        })?;
        sampled += 1;
        collect_mongodb_fields(&document, "", 0, &mut fields);
    }
    let columns = fields
        .into_iter()
        .map(|(name, stats)| {
            let combined_type = stats.types.into_iter().collect::<Vec<_>>().join(" | ");
            json!({
                "name": name,
                "type": combined_type,
                "nullable": stats.observed < sampled || stats.saw_null,
                "primary_key": name == "_id",
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "table_name": collection_name,
        "physical_table_name": collection_name,
        "columns": columns,
        "document_sample_size": sampled,
    }))
}

pub async fn discover_mongodb(config: &MongoConfig) -> Result<DiscoveryOutcome, String> {
    if config.database.trim().is_empty() {
        return Err("MongoDB database is required".to_string());
    }
    let client = mongodb_client(config).await?;
    let database = client.database(config.database.trim());
    database
        .run_command(doc! { "ping": 1 })
        .await
        .map_err(|error| format!("MongoDB connection failed: {error}"))?;
    let mut collection_names = database
        .list_collection_names()
        .await
        .map_err(|error| format!("MongoDB collection discovery failed: {error}"))?;
    collection_names.retain(|name| !name.starts_with("system."));
    collection_names.sort();
    let cap = usize::try_from(max_tables_per_datasource()).unwrap_or(usize::MAX);
    let cap_hit = collection_names.len() > cap;
    collection_names.truncate(cap);
    let mut tables = Vec::with_capacity(collection_names.len());
    let mut skipped = Vec::new();
    for collection_name in collection_names {
        match discover_mongodb_collection(&database, &collection_name).await {
            Ok(table) => tables.push(table),
            Err(error) => skipped.push((collection_name, error)),
        }
    }
    Ok(DiscoveryOutcome {
        tables,
        skipped,
        cap_hit,
        foreign_keys: Vec::new(),
    })
}

pub async fn discover_mongodb_table(
    config: &MongoConfig,
    collection_name: &str,
) -> Result<Option<Value>, String> {
    let client = mongodb_client(config).await?;
    let database = client.database(config.database.trim());
    let exists = database
        .list_collection_names()
        .await
        .map_err(|error| format!("MongoDB collection discovery failed: {error}"))?
        .into_iter()
        .any(|name| name == collection_name);
    if !exists {
        return Ok(None);
    }
    discover_mongodb_collection(&database, collection_name)
        .await
        .map(Some)
}

// ─── MySQL / TiDB ────────────────────────────────────────────────────────────

pub async fn discover_mysql(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
) -> Result<DiscoveryOutcome, String> {
    let url = build_mysql_url_parts(username, password, host, port, database);
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(15))
        .connect(&url)
        .await
        .map_err(|e| format!("MySQL connection failed: {e}"))?;

    let cap = max_tables_per_datasource();

    // Keep the column scan narrow. Joining TABLES repeats table metadata for
    // every column and is noticeably slower on remote MySQL metadata stores.
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(TABLE_NAME AS CHAR) AS table_name, \
                CAST(COLUMN_NAME AS CHAR) AS column_name, \
                CAST(COLUMN_TYPE AS CHAR) AS column_type, \
                CAST(IS_NULLABLE AS CHAR) AS is_nullable, \
                CAST(COALESCE(COLUMN_COMMENT, '') AS CHAR) AS column_comment \
         FROM information_schema.columns \
         WHERE table_schema = ? \
         ORDER BY TABLE_NAME, ORDINAL_POSITION",
    )
    .bind(database)
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("information_schema query failed: {e}"))?;

    let table_comment_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT CAST(TABLE_NAME AS CHAR), CAST(COALESCE(TABLE_COMMENT, '') AS CHAR) \
         FROM information_schema.tables \
         WHERE table_schema = ?",
    )
    .bind(database)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    // Discover foreign keys
    let fk_rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT \
            CAST(kcu.TABLE_NAME AS CHAR) AS source_table, \
            CAST(kcu.COLUMN_NAME AS CHAR) AS source_column, \
            CAST(kcu.REFERENCED_TABLE_NAME AS CHAR) AS target_table, \
            CAST(kcu.REFERENCED_COLUMN_NAME AS CHAR) AS target_column, \
            CAST(rc.CONSTRAINT_NAME AS CHAR) AS constraint_name \
         FROM information_schema.KEY_COLUMN_USAGE kcu \
         JOIN information_schema.REFERENTIAL_CONSTRAINTS rc \
           ON kcu.CONSTRAINT_NAME = rc.CONSTRAINT_NAME \
          AND kcu.TABLE_SCHEMA = rc.TABLE_SCHEMA \
         WHERE kcu.TABLE_SCHEMA = ? \
           AND kcu.REFERENCED_TABLE_NAME IS NOT NULL \
         ORDER BY kcu.TABLE_NAME, kcu.COLUMN_NAME",
    )
    .bind(database)
    .fetch_all(&pool)
    .await
    .unwrap_or_default(); // FK discovery is best-effort; don't fail the whole discovery

    let foreign_keys: Vec<ForeignKey> = fk_rows
        .into_iter()
        .map(|(src_tbl, src_col, tgt_tbl, tgt_col, _)| ForeignKey {
            source_table: src_tbl,
            source_column: src_col,
            target_table: tgt_tbl,
            target_column: tgt_col,
        })
        .collect();

    pool.close().await;

    let mut by_table: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let table_comments = table_comment_rows
        .into_iter()
        .filter(|(_, comment)| !comment.is_empty())
        .collect::<BTreeMap<_, _>>();
    for (table, column, col_type, is_nullable, col_comment) in rows {
        let nullable = is_nullable.eq_ignore_ascii_case("YES");
        let mut col_json = json!({
            "name": column,
            "type": col_type,
            "nullable": nullable,
        });
        if !col_comment.is_empty() {
            col_json["comment"] = json!(col_comment);
        }
        by_table.entry(table).or_default().push(col_json);
    }

    let cap_usize = usize::try_from(cap).unwrap_or(usize::MAX);
    let tables: Vec<Value> = by_table
        .into_iter()
        .take(cap_usize)
        .map(|(table_name, columns)| {
            let mut t = json!({ "table_name": table_name, "columns": columns });
            if let Some(tc) = table_comments.get(table_name.as_str()) {
                t["table_comment"] = json!(tc);
            }
            t
        })
        .collect();

    let cap_hit = tables.len() as u64 >= cap;
    if cap_hit {
        tracing::warn!(
            database,
            cap,
            "MySQL schema discovery hit NL2SQL_MAX_TABLES_PER_DATASOURCE cap; \
             some tables may be missing from schema_info"
        );
    }

    Ok(DiscoveryOutcome {
        tables,
        skipped: Vec::new(),
        cap_hit,
        foreign_keys,
    })
}

/// Discover the schema of a single MySQL table.
pub async fn discover_mysql_table(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    table_name: &str,
) -> Result<Option<Value>, String> {
    let url = build_mysql_url_parts(username, password, host, port, database);
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(15))
        .connect(&url)
        .await
        .map_err(|e| format!("MySQL connection failed: {e}"))?;

    let rows: Result<Vec<(String, String, String, String)>, _> = sqlx::query_as(
        "SELECT CAST(COLUMN_NAME AS CHAR), CAST(COLUMN_TYPE AS CHAR), \
                CAST(IS_NULLABLE AS CHAR), CAST(COLUMN_KEY AS CHAR) \
         FROM information_schema.columns \
         WHERE table_schema = ? AND table_name = ? \
         ORDER BY ORDINAL_POSITION",
    )
    .bind(database)
    .bind(table_name)
    .fetch_all(&pool)
    .await;

    pool.close().await;

    let rows = rows.map_err(|e| format!("information_schema query failed: {e}"))?;

    if rows.is_empty() {
        return Ok(None);
    }

    let columns: Vec<Value> = rows
        .into_iter()
        .map(|(col, col_type, is_nullable, col_key)| {
            json!({
                "name": col,
                "type": col_type,
                "nullable": is_nullable.eq_ignore_ascii_case("YES"),
                "primary_key": col_key.eq_ignore_ascii_case("PRI"),
            })
        })
        .collect();

    Ok(Some(json!({
        "table_name": table_name,
        "columns": columns,
    })))
}

// ─── PostgreSQL ──────────────────────────────────────────────────────────────

pub async fn discover_postgres(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
) -> Result<DiscoveryOutcome, String> {
    let url = build_postgres_url_parts(username, password, host, port, database);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(15))
        .connect(&url)
        .await
        .map_err(|e| format!("PostgreSQL connection failed: {e}"))?;

    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT table_schema, table_name, column_name, udt_name, is_nullable \
         FROM information_schema.columns \
         WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
         ORDER BY table_schema, table_name, ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("information_schema query failed: {e}"))?;

    // Discover foreign keys using PostgreSQL's pg_constraint/pg_class system.
    // We need to resolve the target table's schema via a separate columns lookup,
    // because information_schema.constraint_column_usage does not include schema.
    let fk_rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT \
            tc.table_schema, \
            kcu.table_name, \
            kcu.column_name, \
            ccu.table_name AS foreign_table_name, \
            ccu.column_name AS foreign_column_name, \
            tgt_col.table_schema AS target_schema \
         FROM information_schema.table_constraints AS tc \
         JOIN information_schema.key_column_usage AS kcu \
           ON tc.constraint_name = kcu.constraint_name \
          AND tc.table_schema = kcu.table_schema \
         JOIN information_schema.constraint_column_usage AS ccu \
           ON ccu.constraint_name = tc.constraint_name \
          AND ccu.table_schema = tc.table_schema \
         LEFT JOIN information_schema.columns AS tgt_col \
           ON tgt_col.table_name = ccu.table_name \
          AND tgt_col.column_name = ccu.column_name \
         WHERE tc.constraint_type = 'FOREIGN KEY' \
           AND tc.table_schema NOT IN ('pg_catalog', 'information_schema') \
         ORDER BY tc.table_schema, kcu.table_name, kcu.column_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    pool.close().await;

    let cap = max_tables_per_datasource();

    let mut by_table: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (schema, table, col, ty, is_nullable) in rows {
        let qualified = if schema == "public" {
            table
        } else {
            format!("{schema}.{table}")
        };
        by_table.entry(qualified).or_default().push(json!({
            "name": col,
            "type": ty,
            "nullable": is_nullable.eq_ignore_ascii_case("YES"),
        }));
    }

    let foreign_keys: Vec<ForeignKey> = fk_rows
        .into_iter()
        .map(|(schema, src_tbl, src_col, tgt_tbl, tgt_col, tgt_schema)| {
            let src_qualified = if schema == "public" {
                src_tbl.clone()
            } else {
                format!("{schema}.{src_tbl}")
            };
            // Qualify target table with its actual schema (resolved via LEFT JOIN on columns).
            let tgt_qualified = if tgt_schema.is_empty() || tgt_schema == "public" {
                tgt_tbl.clone()
            } else {
                format!("{tgt_schema}.{tgt_tbl}")
            };
            ForeignKey {
                source_table: src_qualified,
                source_column: src_col,
                target_table: tgt_qualified,
                target_column: tgt_col,
            }
        })
        .collect();

    let cap_usize = usize::try_from(cap).unwrap_or(usize::MAX);
    let tables: Vec<Value> = by_table
        .into_iter()
        .take(cap_usize)
        .map(|(table_name, columns)| json!({ "table_name": table_name, "columns": columns }))
        .collect();

    let cap_hit = tables.len() as u64 >= cap;
    if cap_hit {
        tracing::warn!(
            database,
            cap,
            "PostgreSQL schema discovery hit NL2SQL_MAX_TABLES_PER_DATASOURCE cap"
        );
    }

    Ok(DiscoveryOutcome {
        tables,
        skipped: Vec::new(),
        cap_hit,
        foreign_keys,
    })
}

/// Discover the schema of a single PostgreSQL table.
pub async fn discover_postgres_table(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    table_name: &str,
) -> Result<Option<Value>, String> {
    let url = build_postgres_url_parts(username, password, host, port, database);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(15))
        .connect(&url)
        .await
        .map_err(|e| format!("PostgreSQL connection failed: {e}"))?;

    let rows: Result<Vec<(String, String, String)>, _> = sqlx::query_as(
        "SELECT column_name, udt_name, is_nullable \
         FROM information_schema.columns \
         WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
           AND table_name = $1 \
         ORDER BY ordinal_position",
    )
    .bind(table_name)
    .fetch_all(&pool)
    .await;

    pool.close().await;

    let rows = rows.map_err(|e| format!("information_schema query failed: {e}"))?;

    if rows.is_empty() {
        return Ok(None);
    }

    let columns: Vec<Value> = rows
        .into_iter()
        .map(|(col, col_type, is_nullable)| {
            json!({
                "name": col,
                "type": col_type,
                "nullable": is_nullable.eq_ignore_ascii_case("YES"),
            })
        })
        .collect();

    Ok(Some(json!({
        "table_name": table_name,
        "columns": columns,
    })))
}

// ─── ClickHouse ──────────────────────────────────────────────────────────────

pub async fn discover_clickhouse(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
) -> Result<DiscoveryOutcome, String> {
    use tokio::io::AsyncBufReadExt;

    let addr = format!("http://{host}:{port}");
    let client = clickhouse::Client::default()
        .with_url(&addr)
        .with_user(username)
        .with_password(password)
        .with_database(database);

    let cap = max_tables_per_datasource();

    // List tables. Views/materialised views are excluded so NL2SQL
    // doesn't try to re-query them on every refresh; the semantics
    // layer works against physical tables only.
    let sql = format!(
        "SELECT name FROM system.tables \
         WHERE database = '{}' \
           AND engine NOT IN ('View','MaterializedView','LiveView') \
         LIMIT {cap}",
        database.replace('\'', "''")
    );
    let cursor = client
        .query(&sql)
        .fetch_bytes("JSONEachRow")
        .map_err(|e| format!("ClickHouse schema discovery failed: {e}"))?;

    let mut table_names: Vec<String> = Vec::new();
    let mut lines = cursor.lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| format!("ClickHouse response read failed: {e}"))?
    {
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                table_names.push(name.to_owned());
            }
        }
    }

    let cap_hit = table_names.len() as u64 >= cap;
    if cap_hit {
        tracing::warn!(
            database,
            cap,
            "ClickHouse schema discovery hit NL2SQL_MAX_TABLES_PER_DATASOURCE cap"
        );
    }

    let mut tables: Vec<Value> = Vec::with_capacity(table_names.len());
    let mut skipped: Vec<(String, String)> = Vec::new();

    for table_name in table_names {
        match describe_clickhouse_with_retry(&client, &table_name).await {
            Ok(cols) => {
                tables.push(json!({
                    "table_name": table_name,
                    "columns": cols,
                }));
            }
            Err(e) => {
                tracing::warn!(
                    table = %table_name,
                    error = %e,
                    "ClickHouse DESCRIBE failed after retries, skipping table"
                );
                skipped.push((table_name, e));
            }
        }
    }

    let all_table_names: Vec<String> = tables
        .iter()
        .map(|t| {
            t.get("table_name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_owned()
        })
        .collect();

    // Strategy 1: Column name inference — fast, no extra queries.
    let mut foreign_keys = infer_foreign_keys_from_column_names(&all_table_names, &tables);

    // Strategy 2: Sample-value cardinality inference — disambiguate non-standard
    // naming conventions (e.g. `uid`, `order_no` that don't match _id patterns).
    if should_infer_ch_fks() {
        let sample_fks =
            infer_fks_from_sample_values(&client, database, &all_table_names, &tables).await;
        let existing: std::collections::HashSet<(String, String)> = foreign_keys
            .iter()
            .map(|fk| (fk.source_table.clone(), fk.source_column.clone()))
            .collect();
        for fk in sample_fks {
            let key = (fk.source_table.clone(), fk.source_column.clone());
            if existing.contains(&key) {
                tracing::debug!(
                    src = %fk.source_table,
                    col = %fk.source_column,
                    tgt = %fk.target_table,
                    "FK already found by column-name inference, skipping sample-value inference"
                );
            } else {
                foreign_keys.push(fk);
            }
        }
    }

    Ok(DiscoveryOutcome {
        tables,
        skipped,
        cap_hit,
        foreign_keys,
    })
}

/// Discover the schema of a single ClickHouse table.
pub async fn discover_clickhouse_table(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    table_name: &str,
) -> Result<Option<Value>, String> {
    use tokio::io::AsyncBufReadExt;

    let addr = format!("http://{host}:{port}");
    let client = clickhouse::Client::default()
        .with_url(&addr)
        .with_user(username)
        .with_password(password)
        .with_database(database);

    let sql = format!("DESCRIBE TABLE `{}`", table_name.replace('`', "``"));
    let cursor = client
        .query(&sql)
        .fetch_bytes("JSONEachRow")
        .map_err(|e| format!("ClickHouse DESCRIBE failed: {e}"))?;

    let mut lines = cursor.lines();
    let mut cols: Vec<Value> = Vec::new();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| format!("ClickHouse response read failed: {e}"))?
    {
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            let col_type = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
            let default_type = v.get("default_type").and_then(|t| t.as_str()).unwrap_or("");
            // Skip materialized / alias columns — they are derived, not real columns
            if default_type == "MATERIALIZED" || default_type == "ALIAS" {
                continue;
            }
            let nullable = col_type.starts_with("Nullable(");
            cols.push(json!({
                "name": name,
                "type": col_type,
                "nullable": nullable,
            }));
        }
    }

    if cols.is_empty() {
        return Ok(None);
    }

    Ok(Some(json!({
        "table_name": table_name,
        "columns": cols,
    })))
}

/// Returns true when ClickHouse FK inference from sample values is enabled.
/// Controlled by `NL2SQL_CLICKHOUSE_FK_INFERENCE=true`.
fn should_infer_ch_fks() -> bool {
    std::env::var("NL2SQL_CLICKHOUSE_FK_INFERENCE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(true)
}

/// Column suffixes and names that suggest a foreign-key relationship.
/// Used to prune the O(n²) candidate space before issuing ClickHouse queries.
fn is_fk_like_column(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with("_id")
        || lower.ends_with("_uid")
        || lower.ends_with("_oid")
        || lower.ends_with("_no")
        || lower.ends_with("_key")
        || lower.ends_with("_ref")
        || lower.ends_with("_ref_id")
}

/// Column names that typically hold primary-key / referenced values.
/// When a column in the source table matches one of these names in the target,
/// we have a strong candidate FK pair.
fn is_pk_like_column(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "id"
        || lower.ends_with("_id")
        || lower == "uid"
        || lower == "oid"
        || lower == "key"
        || lower.ends_with("_uid")
        || lower.ends_with("_oid")
        || lower.ends_with("_key")
}

/// Infer foreign key relationships by sampling rows and computing join cardinality.
///
/// For every candidate FK column in table A that could reference a column in table B:
///   1. Sample up to N rows from A and B using ClickHouse's `sample()` function.
///   2. Run a sampled JOIN to estimate how many rows in A have a matching partner in B.
///   3. A relationship is considered a high-confidence FK when:
///        - coverage = matched_rows / A_rows >= 0.9  (almost every row in A joins)
///        - cardinality_ratio = distinct(A.col) / distinct(B.col) <= 1.1  (B-side is a PK)
///   4. Results are deduplicated by (source_table, source_column).
///
/// Candidate pairs are aggressively pruned: only FK-like source columns (ending in
/// `_id`, `_uid`, `_oid`, `_no`, `_key`, `_ref`) are checked against PK-like
/// target columns (`id`, `uid`, `oid`, `_id`, `_uid`, `_oid`, `_key`).
/// This keeps query count O(n) rather than O(n²×m²).
///
/// This strategy resolves naming-convention blind spots: `uid`, `order_no`, `parent_oid`
/// and other non-standard FK column names that `infer_foreign_keys_from_column_names` misses.
async fn infer_fks_from_sample_values(
    client: &clickhouse::Client,
    _database: &str,
    all_table_names: &[String],
    tables: &[serde_json::Value],
) -> Vec<ForeignKey> {
    let safename = |s: &str| s.replace('\'', "''");

    // Build a per-table column index: table_name -> Vec<(column_name, column_type)>
    let table_cols: std::collections::HashMap<String, Vec<(String, String)>> = {
        let mut map = std::collections::HashMap::new();
        for t in tables {
            let tn = match t.get("table_name").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let cols: Vec<(String, String)> = t
                .get("columns")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| {
                            let name = c.get("name")?.as_str()?.to_owned();
                            let typ = c
                                .get("type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("String")
                                .to_owned();
                            Some((name, typ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            map.insert(tn.to_owned(), cols);
        }
        map
    };

    // Generate candidates: only (FK-like source col, target table, PK-like target col).
    // This is O(n) per table pair rather than O(cols_src × cols_tgt).
    let mut candidates: Vec<(String, String, String, String, String, String)> = Vec::new();
    for src_table in all_table_names {
        let src_cols = match table_cols.get(src_table) {
            Some(c) => c,
            None => continue,
        };
        for src_col in src_cols.iter().filter(|(n, _)| is_fk_like_column(n)) {
            for tgt_table in all_table_names {
                if tgt_table == src_table {
                    continue;
                }
                let tgt_cols = match table_cols.get(tgt_table) {
                    Some(c) => c,
                    None => continue,
                };
                for tgt_col in tgt_cols.iter().filter(|(n, _)| is_pk_like_column(n)) {
                    candidates.push((
                        src_table.clone(),
                        src_col.0.clone(),
                        src_col.1.clone(),
                        tgt_table.clone(),
                        tgt_col.0.clone(),
                        tgt_col.1.clone(),
                    ));
                }
            }
        }
    }

    if candidates.is_empty() {
        tracing::debug!("infer_fks_from_sample_values: no FK-like column candidates found");
        return Vec::new();
    }

    tracing::debug!(
        pairs = candidates.len(),
        "infer_fks_from_sample_values: checking {} candidate pairs",
        candidates.len()
    );

    let coverage_thresh = std::env::var("NL2SQL_CH_FK_COVERAGE_THRESH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.85_f64);

    let cardinality_thresh = std::env::var("NL2SQL_CH_FK_CARD_RATIO_THRESH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.2_f64);

    let mut fks: Vec<ForeignKey> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    // Use SAMPLE 1/10 for fast probabilistic cardinality on large tables.
    for (src_tbl, src_col, _, tgt_tbl, tgt_col, _) in candidates {
        let s_src = safename(&src_tbl);
        let s_src_col = safename(&src_col);
        let s_tgt = safename(&tgt_tbl);
        let s_tgt_col = safename(&tgt_col);

        // One query per candidate: sampled join to get matched_rows,
        // plus uniqExact for distinct counts on both sides.
        let cardinality_sql = format!(
            "SELECT \
               countIf({src_col} != '')               AS sampled_src_rows, \
               uniqExactIf({src_col}, {src_col} != '') AS sampled_src_distinct, \
               uniqExactIf({tgt_col}, {tgt_col} != '') AS sampled_tgt_distinct, \
               ( \
                 SELECT count() \
                 FROM {s_src} SAMPLE 1/10 AS a \
                 GLOBAL INNER JOIN \
                   (SELECT {tgt_col} FROM {s_tgt} SAMPLE 1/10) AS b \
                 ON a.{src_col} = b.{tgt_col} \
               )                                      AS matched_rows \
             FORMAT JSONEachRow",
            src_col = s_src_col,
            tgt_col = s_tgt_col,
            s_src = s_src,
            s_tgt = s_tgt,
        );

        let output = match client.query(&cardinality_sql).fetch_all::<String>().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::trace!(
                    error = %e,
                    src = %src_tbl,
                    src_col = %src_col,
                    tgt = %tgt_tbl,
                    tgt_col = %tgt_col,
                    "cardinality query failed"
                );
                continue;
            }
        };

        let line = match output.first() {
            Some(l) => l,
            None => continue,
        };

        #[derive(serde::Deserialize)]
        struct CardRow {
            #[serde(rename = "sampled_src_rows")]
            sampled_src_rows: u64,
            #[serde(rename = "sampled_src_distinct")]
            sampled_src_distinct: u64,
            #[serde(rename = "sampled_tgt_distinct")]
            sampled_tgt_distinct: u64,
            #[serde(rename = "matched_rows")]
            matched_rows: u64,
        }

        let row: CardRow = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                tracing::trace!(error = %e, line, "failed to parse cardinality row");
                continue;
            }
        };

        if row.sampled_src_rows == 0 || row.sampled_tgt_distinct == 0 {
            continue;
        }

        let coverage = row.matched_rows as f64 / row.sampled_src_rows as f64;
        let card_ratio = row.sampled_src_distinct as f64 / row.sampled_tgt_distinct as f64;

        if coverage >= coverage_thresh && card_ratio <= cardinality_thresh {
            let key = (src_tbl.clone(), src_col.clone());
            if seen.insert(key) {
                tracing::info!(
                    src = %src_tbl,
                    src_col = %src_col,
                    tgt = %tgt_tbl,
                    tgt_col = %tgt_col,
                    coverage = coverage,
                    card_ratio = card_ratio,
                    "inferred FK via sample-value cardinality"
                );
                fks.push(ForeignKey {
                    source_table: src_tbl,
                    source_column: src_col,
                    target_table: tgt_tbl,
                    target_column: tgt_col,
                });
            }
        }
    }

    tracing::debug!(
        count = fks.len(),
        "infer_fks_from_sample_values: found {} high-confidence FKs",
        fks.len()
    );
    fks
}

/// Infer foreign key relationships from column naming conventions.
///
/// Heuristics:
/// - `{singular}_id` → references `table_name` where table_name singular form matches (e.g. `user_id` → `users(id)`)
/// - `id_{table}` → references `table_name` (e.g. `id_user` → `users(id)`)
/// - Columns ending in `_id` that match a table name (singular form)
///
/// Returns the set of inferred FKs, deduped by (source_table, source_column).
fn infer_foreign_keys_from_column_names(
    all_table_names: &[String],
    tables: &[serde_json::Value],
) -> Vec<ForeignKey> {
    // Build a map: singular_form -> table_name for fast lookup
    let singular_to_table: std::collections::HashMap<String, String> = all_table_names
        .iter()
        .filter_map(|name| {
            let singular = singularize(name);
            if singular == *name {
                None
            } else {
                Some((singular, name.clone()))
            }
        })
        .collect();

    // Also index by exact name for id_{table} pattern
    let exact_match: std::collections::HashSet<&str> =
        all_table_names.iter().map(|s| s.as_str()).collect();

    let mut fks: Vec<ForeignKey> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for table in tables {
        let table_name = table
            .get("table_name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_owned();

        let columns = table.get("columns").and_then(|c| c.as_array());

        for col_entry in columns.iter().flat_map(|c| c.iter()) {
            let col_name = col_entry
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default();

            if let Some(fk) =
                try_infer_fk_from_col_name(&table_name, col_name, &singular_to_table, &exact_match)
            {
                let key = (fk.source_table.clone(), fk.source_column.clone());
                if seen.insert(key) {
                    fks.push(fk);
                }
            }
        }
    }

    tracing::debug!(
        count = fks.len(),
        "inferred {} FK relationships from column naming conventions",
        fks.len(),
    );
    fks
}

/// Try to infer a FK from a single column name.
/// Returns `Some(ForeignKey)` if inference succeeds, `None` otherwise.
fn try_infer_fk_from_col_name(
    source_table: &str,
    col_name: &str,
    singular_to_table: &std::collections::HashMap<String, String>,
    exact_match: &std::collections::HashSet<&str>,
) -> Option<ForeignKey> {
    let col_lower = col_name.to_lowercase();

    // Pattern 1: `{singular}_id` → references `{plural}` table's primary key (assumed `id`)
    // e.g. `user_id` → `users.id`, `order_id` → `orders.id`
    if col_lower.ends_with("_id") {
        let stem = &col_lower[..col_lower.len() - 3]; // strip "_id"
        if stem.is_empty() {
            return None;
        }
        // Try exact plural match first
        if let Some(target_table) = singular_to_table.get(stem) {
            return Some(ForeignKey {
                source_table: source_table.to_owned(),
                source_column: col_name.to_owned(),
                target_table: target_table.clone(),
                target_column: "id".to_owned(),
            });
        }
        // Try singular form of a table name
        for (singular, target_table) in singular_to_table {
            if singular == stem {
                return Some(ForeignKey {
                    source_table: source_table.to_owned(),
                    source_column: col_name.to_owned(),
                    target_table: target_table.clone(),
                    target_column: "id".to_owned(),
                });
            }
        }
    }

    // Pattern 2: `id_{table}` → references `{table}.id`
    // e.g. `id_user` → `users.id`
    if col_lower.starts_with("id_") {
        let target = &col_lower[3..];
        if target.is_empty() {
            return None;
        }
        if exact_match.contains(target) {
            return Some(ForeignKey {
                source_table: source_table.to_owned(),
                source_column: col_name.to_owned(),
                target_table: target.to_owned(),
                target_column: "id".to_owned(),
            });
        }
        // Also try plural form
        let plural = pluralize(target);
        if exact_match.contains(&plural.as_str()) {
            return Some(ForeignKey {
                source_table: source_table.to_owned(),
                source_column: col_name.to_owned(),
                target_table: plural,
                target_column: "id".to_owned(),
            });
        }
    }

    None
}

/// Simple English singularize — handles common irregular and regular forms.
/// Returns the input unchanged if no recognized plural suffix is found.
fn singularize(word: &str) -> String {
    let lower = word.to_lowercase();

    // Irregular plurals
    let irregular: &[(&str, &str)] = &[
        ("people", "person"),
        ("men", "man"),
        ("women", "woman"),
        ("children", "child"),
        ("feet", "foot"),
        ("teeth", "tooth"),
        ("geese", "goose"),
        ("mice", "mouse"),
        ("lice", "louse"),
        ("data", "datum"),
        ("media", "medium"),
        ("phenomena", "phenomenon"),
        ("criteria", "criterion"),
        ("analyses", "analysis"),
        ("bases", "basis"),
        ("crises", "crisis"),
        ("diagnoses", "diagnosis"),
        ("hypotheses", "hypothesis"),
        ("oases", "oasis"),
        ("parentheses", "parenthesis"),
        ("synapses", "synapse"),
        ("theses", "thesis"),
        ("categories", "category"),
        ("cities", "city"),
        ("companies", "company"),
        ("countries", "country"),
        ("families", "family"),
        ("histories", "history"),
        ("industries", "industry"),
        ("libraries", "library"),
        ("memories", "memory"),
        ("messages", "message"),
        ("methods", "method"),
        ("orders", "order"),
        ("prices", "price"),
        ("products", "product"),
        ("properties", "property"),
        ("queries", "query"),
        ("roles", "role"),
        ("schemas", "schema"),
        ("sessions", "session"),
        ("statistics", "statistics"),
        ("strategies", "strategy"),
        ("tables", "table"),
        ("transactions", "transaction"),
        ("users", "user"),
        ("views", "view"),
    ];
    for (plural, singular) in irregular {
        if lower == *plural {
            let start_upper = word
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false);
            let result = if start_upper {
                let mut s = (*singular).to_string();
                s[..1].make_ascii_uppercase();
                s
            } else {
                singular.to_string()
            };
            if lower != word {
                // preserve original case
                return result;
            }
            return singular.to_string();
        }
    }

    // Regular: strip common plural suffixes
    if lower.ends_with("ies") && lower.len() > 3 {
        // category → categor  + y = category
        let stem = &lower[..lower.len() - 3];
        return format!("{}y", stem);
    }
    if lower.ends_with("es") && lower.len() > 2 {
        let stem = &lower[..lower.len() - 2];
        // boxes → box, matches → match, watches → watch
        if stem.ends_with('x')
            || stem.ends_with('s')
            || stem.ends_with('z')
            || stem.ends_with("sh")
            || stem.ends_with("ch")
        {
            return stem.to_string();
        }
        // addresses → address
        if stem.ends_with("ss") {
            return stem.to_string();
        }
        return stem.to_string();
    }
    if lower.ends_with('s') && lower.len() > 1 && !lower.ends_with("ss") {
        return lower[..lower.len() - 1].to_string();
    }

    word.to_string()
}

/// Simple English pluralize — handles common forms.
fn pluralize(word: &str) -> String {
    let lower = word.to_lowercase();

    // Irregular plurals (same list as above, reversed)
    let irregular: &[(&str, &str)] = &[
        ("person", "people"),
        ("man", "men"),
        ("woman", "women"),
        ("child", "children"),
        ("foot", "feet"),
        ("tooth", "teeth"),
        ("goose", "geese"),
        ("mouse", "mice"),
        ("louse", "lice"),
        ("datum", "data"),
        ("medium", "media"),
        ("phenomenon", "phenomena"),
        ("criterion", "criteria"),
        ("analysis", "analyses"),
        ("basis", "bases"),
        ("crisis", "crises"),
        ("diagnosis", "diagnoses"),
        ("hypothesis", "hypotheses"),
        ("oasis", "oases"),
        ("parenthesis", "parentheses"),
        ("synapse", "synapses"),
        ("thesis", "theses"),
        ("category", "categories"),
        ("city", "cities"),
        ("company", "companies"),
        ("country", "countries"),
        ("family", "families"),
        ("history", "histories"),
        ("industry", "industries"),
        ("library", "libraries"),
        ("memory", "memories"),
        ("message", "messages"),
        ("method", "methods"),
        ("order", "orders"),
        ("price", "prices"),
        ("product", "products"),
        ("property", "properties"),
        ("query", "queries"),
        ("role", "roles"),
        ("schema", "schemas"),
        ("session", "sessions"),
        ("strategy", "strategies"),
        ("table", "tables"),
        ("transaction", "transactions"),
        ("user", "users"),
        ("view", "views"),
    ];
    for (singular, plural) in irregular {
        if lower == *singular {
            let start_upper = word
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false);
            if start_upper {
                let mut p = (*plural).to_string();
                p[..1].make_ascii_uppercase();
                return p;
            }
            return plural.to_string();
        }
    }

    // y → ies (city → cities, family → families)
    if lower.ends_with('y')
        && lower.len() > 1
        && !lower.ends_with("ay")
        && !lower.ends_with("ey")
        && !lower.ends_with("oy")
        && !lower.ends_with("uy")
    {
        return format!("{}ies", &word[..word.len() - 1]);
    }
    // f / fe → ves (leaf → leaves, knife → knives)
    if lower.ends_with("fe") {
        return format!("{}ves", &word[..word.len() - 2]);
    }
    if lower.ends_with('f') {
        return format!("{}ves", &word[..word.len() - 1]);
    }
    // s, x, z, sh, ch, ss → es
    if lower.ends_with("ss")
        || lower.ends_with("sh")
        || lower.ends_with("ch")
        || lower.ends_with("ax")
        || lower.ends_with("ex")
        || lower.ends_with("ix")
        || lower.ends_with("ox")
        || lower.ends_with('z')
        || lower.ends_with('x')
    {
        return format!("{}es", word);
    }
    // o → es (potato → potatoes, hero → heroes) — but not bio, photo, etc.
    if lower.ends_with('o') && lower.len() > 2 {
        let exceptions = [
            "photo", "piano", "halo", "logo", "memo", "ratio", "dyno", "kilo", "pico", "nano",
            "micro",
        ];
        if !exceptions.contains(&lower.as_str()) {
            return format!("{}es", word);
        }
    }
    // Default: add s
    format!("{}s", word)
}

async fn describe_clickhouse_with_retry(
    client: &clickhouse::Client,
    table_name: &str,
) -> Result<Vec<Value>, String> {
    use tokio::io::AsyncBufReadExt;
    let mut last_err = String::new();
    for attempt in 0..RETRY_ATTEMPTS {
        backoff(attempt).await;
        let sql = format!("DESCRIBE TABLE `{}`", table_name.replace('`', "``"));
        match client.query(&sql).fetch_bytes("JSONEachRow") {
            Ok(cursor) => {
                let mut cols: Vec<Value> = Vec::new();
                let mut lines = cursor.lines();
                let mut parse_err: Option<String> = None;
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if line.is_empty() {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                                let name = v
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_owned();
                                let col_type = v
                                    .get("type")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or_default()
                                    .to_owned();
                                cols.push(json!({ "name": name, "type": col_type }));
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            parse_err = Some(e.to_string());
                            break;
                        }
                    }
                }
                match parse_err {
                    Some(e) => last_err = e,
                    None => return Ok(cols),
                }
            }
            Err(e) => {
                last_err = e.to_string();
            }
        }
    }
    Err(last_err)
}

// ─── Trino / Presto ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TrinoSchemasDiscovery {
    pub schemas: Vec<String>,
    pub method: String,
    pub warnings: Vec<String>,
}

fn trino_ident(identifier: &str) -> String {
    let trimmed = identifier.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        return trimmed.to_string();
    }
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return "\"\"".to_string();
    };
    let simple_first = first.is_ascii_alphabetic() || first == '_';
    let simple_rest = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if simple_first && simple_rest {
        trimmed.to_string()
    } else {
        format!("\"{}\"", trimmed.replace('"', "\"\""))
    }
}

fn trino_qualified_schema(catalog: &str, schema: &str) -> String {
    format!("{}.{}", trino_ident(catalog), trino_ident(schema))
}

fn trino_qualified_table(catalog: &str, schema: &str, table: &str) -> String {
    format!(
        "{}.{}.{}",
        trino_ident(catalog),
        trino_ident(schema),
        trino_ident(table)
    )
}

fn trino_full_table_name(catalog: &str, schema: &str, table: &str) -> String {
    format!("{}.{}.{}", catalog.trim(), schema.trim(), table.trim())
}

fn trino_schema_table_name(schema: &str, table: &str) -> String {
    format!("{}.{}", schema.trim(), table.trim())
}

fn parse_trino_table_reference(
    default_catalog: &str,
    default_schema: &str,
    table_ref: &str,
) -> (String, String, String) {
    let parts: Vec<&str> = table_ref
        .split('.')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        [catalog, schema, table, ..] => (
            catalog.trim_matches('"').to_string(),
            schema.trim_matches('"').to_string(),
            table.trim_matches('"').to_string(),
        ),
        [schema, table] => (
            default_catalog.to_string(),
            schema.trim_matches('"').to_string(),
            table.trim_matches('"').to_string(),
        ),
        [table] => (
            default_catalog.to_string(),
            default_schema.to_string(),
            table.trim_matches('"').to_string(),
        ),
        _ => (
            default_catalog.to_string(),
            default_schema.to_string(),
            table_ref.trim().trim_matches('"').to_string(),
        ),
    }
}

fn build_trino_client(
    host: &str,
    port: u16,
    catalog: &str,
    schema: &str,
    username: &str,
    password: Option<&str>,
    secure: bool,
    basic_auth: bool,
) -> Result<trino_rust_client::Client, String> {
    let normalized_host = crate::datasource_config::normalize_host_input(host);
    let port = normalized_host.port.unwrap_or(port);
    let secure = normalized_host.secure.unwrap_or(secure);
    let mut builder = trino_rust_client::ClientBuilder::new(username, &normalized_host.host)
        .port(port)
        .catalog(catalog)
        .schema(schema)
        .secure(secure);
    if basic_auth {
        builder = builder.auth(trino_rust_client::auth::Auth::Basic(
            username.to_string(),
            password.map(|p| p.to_string()),
        ));
    }
    builder
        .build()
        .map_err(|e| format!("Trino client build failed: {e}"))
}

async fn run_trino_first_column_query(
    cli: &trino_rust_client::Client,
    sql: &str,
    timeout_secs: u64,
) -> Result<Vec<String>, String> {
    let query = cli.get_all::<trino_rust_client::Row>(sql.to_string());
    let dataset = match tokio::time::timeout(Duration::from_secs(timeout_secs), query).await {
        Ok(Ok(dataset)) => dataset,
        Ok(Err(e)) if e.to_string() == "empty data" => return Ok(Vec::new()),
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err(format!("query timed out after {timeout_secs}s")),
    };
    let (_types, rows) = dataset.split();
    let mut values = Vec::new();
    for row in rows {
        let cols: Vec<Value> = row.into_json();
        if let Some(value) = cols.first().and_then(Value::as_str) {
            values.push(value.to_string());
        }
    }
    Ok(values)
}

async fn run_trino_statement_best_effort(
    cli: &trino_rust_client::Client,
    sql: &str,
    timeout_secs: u64,
) -> Result<(), String> {
    match run_trino_first_column_query(cli, sql, timeout_secs).await {
        Ok(_) => Ok(()),
        Err(e) if e == "empty data" => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn discover_trino_schemas(
    host: &str,
    port: u16,
    catalog: &str,
    username: &str,
    password: Option<&str>,
    secure: bool,
    basic_auth: bool,
) -> Result<TrinoSchemasDiscovery, String> {
    if host.trim().is_empty() {
        return Err("host is required".to_string());
    }
    if catalog.trim().is_empty() {
        return Err("catalog is required".to_string());
    }
    if username.trim().is_empty() {
        return Err("username is required".to_string());
    }

    let cli = build_trino_client(
        host, port, catalog, "default", username, password, secure, basic_auth,
    )?;
    let timeout_secs = trino_schema_listing_timeout_secs();
    let mut warnings = Vec::new();

    let show_catalogs_sql = "SHOW CATALOGS";
    match run_trino_first_column_query(&cli, show_catalogs_sql, timeout_secs).await {
        Ok(catalogs) => {
            if !catalogs.iter().any(|c| c.eq_ignore_ascii_case(catalog)) {
                warnings.push(format!(
                    "SHOW CATALOGS succeeded but catalog `{catalog}` was not listed"
                ));
            }
        }
        Err(e) => warnings.push(format!("SHOW CATALOGS failed: {e}")),
    }

    let attempts = [
        (
            format!("SHOW SCHEMAS FROM {}", trino_ident(catalog)),
            "show_schemas_from_catalog",
        ),
        (
            format!("SHOW SCHEMAS IN {}", trino_ident(catalog)),
            "show_schemas_in_catalog",
        ),
    ];
    let mut errors = Vec::new();
    for (sql, method) in attempts {
        match run_trino_first_column_query(&cli, &sql, timeout_secs).await {
            Ok(schemas) if !schemas.is_empty() => {
                return Ok(TrinoSchemasDiscovery {
                    schemas,
                    method: method.to_string(),
                    warnings,
                });
            }
            Ok(_) => errors.push(format!("{sql}: empty result")),
            Err(e) => errors.push(format!("{sql}: {e}")),
        }
    }

    let use_sql = format!("USE {}", trino_ident(catalog));
    match run_trino_statement_best_effort(&cli, &use_sql, timeout_secs).await {
        Ok(()) => match run_trino_first_column_query(&cli, "SHOW DATABASES", timeout_secs).await {
            Ok(schemas) if !schemas.is_empty() => {
                return Ok(TrinoSchemasDiscovery {
                    schemas,
                    method: "spark_use_catalog_show_databases".to_string(),
                    warnings,
                });
            }
            Ok(_) => errors.push("SHOW DATABASES: empty result".to_string()),
            Err(e) => errors.push(format!("SHOW DATABASES: {e}")),
        },
        Err(e) => errors.push(format!("{use_sql}: {e}")),
    }

    Err(format!(
        "Trino/Presto 获取 Schema 失败。catalog={catalog}。已尝试 SHOW CATALOGS、SHOW SCHEMAS FROM/IN catalog、USE catalog + SHOW DATABASES。错误：{}",
        errors.join(" | ")
    ))
}

async fn list_trino_tables_for_schema(
    cli: &trino_rust_client::Client,
    catalog: &str,
    schema: &str,
    timeout_secs: u64,
) -> Result<(Vec<String>, String), String> {
    let attempts = [
        (
            format!("SHOW TABLES IN {}", trino_qualified_schema(catalog, schema)),
            "show_tables_in_schema",
        ),
        (
            format!(
                "SHOW TABLES FROM {}",
                trino_qualified_schema(catalog, schema)
            ),
            "show_tables_from_schema",
        ),
    ];
    let mut errors = Vec::new();
    for (sql, method) in attempts {
        match run_trino_first_column_query(cli, &sql, timeout_secs).await {
            Ok(tables) => return Ok((tables, method.to_string())),
            Err(e) => errors.push(format!("{sql}: {e}")),
        }
    }

    Err(format!(
        "Trino/Presto 获取表列表失败。catalog={catalog}, schema={schema}。已尝试 SHOW TABLES IN/FROM catalog.schema。错误：{}",
        errors.join(" | ")
    ))
}

pub async fn discover_trino(
    host: &str,
    port: u16,
    catalog: &str,
    schema: &str,
    username: &str,
    password: Option<&str>,
    secure: bool,
    basic_auth: bool,
) -> Result<DiscoveryOutcome, String> {
    let schemas = normalize_trino_schemas(schema, std::iter::empty::<&str>());
    discover_trino_multi(
        host, port, catalog, &schemas, username, password, secure, basic_auth,
    )
    .await
}

pub async fn discover_trino_multi(
    host: &str,
    port: u16,
    catalog: &str,
    schemas: &[String],
    username: &str,
    password: Option<&str>,
    secure: bool,
    basic_auth: bool,
) -> Result<DiscoveryOutcome, String> {
    let schemas = normalize_trino_schemas("", schemas.iter().map(String::as_str));
    let default_schema = schemas.first().map(String::as_str).unwrap_or("default");
    let normalized_host = crate::datasource_config::normalize_host_input(host);
    let port = normalized_host.port.unwrap_or(port);
    let secure = normalized_host.secure.unwrap_or(secure);
    let cli = build_trino_client(
        host,
        port,
        catalog,
        default_schema,
        username,
        password,
        secure,
        basic_auth,
    )?;

    let cap = max_trino_tables_per_datasource();
    let list_timeout = Duration::from_secs(trino_schema_listing_timeout_secs());
    tracing::info!(
        host = %normalized_host.host,
        port,
        secure,
        basic_auth,
        catalog,
        schemas = ?schemas,
        cap,
        timeout_secs = list_timeout.as_secs(),
        "Trino schema discovery: listing tables"
    );

    let mut tables: Vec<Value> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut listed_table_count = 0usize;
    let mut foreign_keys = Vec::new();

    for schema in &schemas {
        if listed_table_count as u64 >= cap {
            break;
        }
        let (schema_tables, method) =
            match list_trino_tables_for_schema(&cli, catalog, schema, list_timeout.as_secs()).await
            {
                Ok(result) => result,
                Err(e) if e == "empty data" => (Vec::new(), "empty".to_string()),
                Err(e) => {
                    let schema_ref = trino_qualified_schema(catalog, schema);
                    tracing::warn!(
                        catalog,
                        schema = %schema,
                        error = %e,
                        "Trino schema discovery: schema table listing failed, skipping schema"
                    );
                    skipped.push((schema_ref, e));
                    continue;
                }
            };
        tracing::info!(
            catalog,
            schema = %schema,
            method = %method,
            table_count = schema_tables.len(),
            "Trino schema discovery: listed schema tables"
        );
        listed_table_count = listed_table_count.saturating_add(schema_tables.len());

        for table_name in schema_tables {
            if tables.len() as u64 >= cap {
                break;
            }
            match describe_trino_with_retry(&cli, catalog, schema, &table_name).await {
                Ok(cols) => {
                    let full_name = trino_full_table_name(catalog, schema, &table_name);
                    tables.push(json!({
                        "table_name": full_name,
                        "name": table_name,
                        "physical_table_name": table_name,
                        "catalog": catalog,
                        "schema": schema,
                        "qualified_name": trino_schema_table_name(schema, &table_name),
                        "fully_qualified_name": full_name,
                        "columns": cols,
                    }));
                }
                Err(e) => {
                    let full_name = trino_full_table_name(catalog, schema, &table_name);
                    tracing::warn!(
                        table = %full_name,
                        error = %e,
                        "Trino DESCRIBE failed after retries, skipping table"
                    );
                    skipped.push((full_name, e));
                }
            }
        }

        foreign_keys.extend(discover_trino_foreign_keys(&cli, schema, &tables).await);
    }

    if tables.is_empty() && !skipped.is_empty() {
        return Err(format!(
            "Trino/Presto 获取表结构失败：所选 schema 均未能列出表。catalog={catalog}, schemas={}。错误：{}",
            schemas.join(", "),
            skipped
                .iter()
                .map(|(schema, err)| format!("{schema}: {err}"))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }

    let cap_hit = listed_table_count as u64 >= cap;
    if cap_hit {
        tracing::warn!(
            catalog,
            cap,
            listed_table_count,
            "Trino schema discovery hit NL2SQL_TRINO_MAX_TABLES_PER_DATASOURCE cap"
        );
    }

    let _all_table_names: Vec<String> = tables
        .iter()
        .filter_map(|t| {
            t.get("table_name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_owned())
        })
        .collect();

    Ok(DiscoveryOutcome {
        tables,
        skipped,
        cap_hit,
        foreign_keys,
    })
}

/// Discover the schema of a single Trino table.
pub async fn discover_trino_table(
    host: &str,
    port: u16,
    catalog: &str,
    schema: &str,
    username: &str,
    password: Option<&str>,
    secure: bool,
    basic_auth: bool,
    table_name: &str,
) -> Result<Option<Value>, String> {
    let (catalog, schema, physical_table) =
        parse_trino_table_reference(catalog, schema, table_name);
    let cli = build_trino_client(
        host, port, &catalog, &schema, username, password, secure, basic_auth,
    )?;

    let cols = describe_trino_with_retry(&cli, &catalog, &schema, &physical_table).await?;

    if cols.is_empty() {
        return Ok(None);
    }

    let full_name = trino_full_table_name(&catalog, &schema, &physical_table);
    Ok(Some(json!({
        "table_name": full_name,
        "name": physical_table,
        "physical_table_name": physical_table,
        "catalog": catalog,
        "schema": schema,
        "qualified_name": trino_schema_table_name(&schema, &physical_table),
        "fully_qualified_name": full_name,
        "columns": cols,
    })))
}

/// Discover foreign keys for Trino using multiple strategies in priority order.
///
/// Strategy 1: Query `information_schema.table_constraints` + `key_column_usage`
///              (works for connectors that expose standard SQL metadata)
/// Strategy 2: Column name pattern matching -- same heuristic as ClickHouse
///              (best-effort fallback for connectors without FK metadata)
async fn discover_trino_foreign_keys(
    cli: &trino_rust_client::Client,
    schema: &str,
    tables: &[serde_json::Value],
) -> Vec<ForeignKey> {
    // Strategy 1: Try standard SQL foreign key query via information_schema
    if let Some(fks) = try_trino_information_schema_fks(cli, schema).await {
        if !fks.is_empty() {
            tracing::info!(
                count = fks.len(),
                "discovered {0} FKs via Trino information_schema",
                fks.len()
            );
            return fks;
        }
    }

    // Strategy 2: Fall back to column name inference
    let all_table_names: Vec<String> = tables
        .iter()
        .filter_map(|t| {
            t.get("table_name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_owned())
        })
        .collect();

    tracing::debug!(
        count = all_table_names.len(),
        "no FKs from information_schema, falling back to column name inference"
    );
    infer_foreign_keys_from_column_names(&all_table_names, tables)
}

/// Query Trino's information_schema for actual foreign key constraints.
/// Returns `Some(Vec<ForeignKey>)` on success (may be empty), `None` on failure.
async fn try_trino_information_schema_fks(
    cli: &trino_rust_client::Client,
    schema: &str,
) -> Option<Vec<ForeignKey>> {
    let query = format!(
        r#"SELECT
               kcu.table_name      AS source_table,
               kcu.column_name     AS source_column,
               ccu.table_name      AS target_table,
               ccu.column_name     AS target_column
         FROM information_schema.table_constraints  tc
         JOIN information_schema.key_column_usage  kcu
           ON tc.constraint_name = kcu.constraint_name
          AND tc.table_schema   = kcu.table_schema
         JOIN information_schema.constraint_column_usage ccu
           ON tc.constraint_name = ccu.constraint_name
          AND tc.table_schema   = ccu.table_schema
         WHERE tc.constraint_type = 'FOREIGN KEY'
           AND tc.table_schema   = '{schema_safe}'
         ORDER BY tc.table_name, kcu.column_name"#,
        schema_safe = schema.replace('\'', "''")
    );

    let result = match tokio::time::timeout(
        Duration::from_secs(trino_schema_listing_timeout_secs()),
        cli.get_all::<trino_rust_client::Row>(query),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "Trino information_schema FK query failed");
            return None;
        }
        Err(_) => {
            tracing::debug!(
                timeout_secs = trino_schema_listing_timeout_secs(),
                "Trino information_schema FK query timed out"
            );
            return None;
        }
    };

    let (_types, rows) = result.split();

    let mut fks = Vec::new();
    for row in rows {
        let vals: Vec<serde_json::Value> = row.into_json();
        let get =
            |i: usize| -> Option<String> { vals.get(i).and_then(|v| v.as_str().map(String::from)) };
        let (Some(src_tbl), Some(src_col), Some(tgt_tbl), Some(tgt_col)) =
            (get(0), get(1), get(2), get(3))
        else {
            continue;
        };

        fks.push(ForeignKey {
            source_table: src_tbl,
            source_column: src_col,
            target_table: tgt_tbl,
            target_column: tgt_col,
        });
    }

    Some(fks)
}

async fn describe_trino_with_retry(
    cli: &trino_rust_client::Client,
    catalog: &str,
    schema: &str,
    table_name: &str,
) -> Result<Vec<Value>, String> {
    let mut last_err = String::new();
    for attempt in 0..RETRY_ATTEMPTS {
        backoff(attempt).await;
        let sql = format!(
            "DESCRIBE TABLE {}",
            trino_qualified_table(catalog, schema, table_name)
        );
        match tokio::time::timeout(
            Duration::from_secs(trino_schema_listing_timeout_secs()),
            cli.get_all::<trino_rust_client::Row>(sql.clone()),
        )
        .await
        {
            Ok(Ok(ds)) => {
                let (_types, rows) = ds.split();
                let cols: Vec<Value> = rows
                    .into_iter()
                    .map(|r| {
                        let vals: Vec<Value> = r.into_json();
                        let name = vals
                            .first()
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_owned();
                        let col_type = vals
                            .get(1)
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_owned();
                        json!({ "name": name, "type": col_type })
                    })
                    .collect();
                return Ok(cols);
            }
            Ok(Err(e)) => last_err = e.to_string(),
            Err(_) => {
                last_err = format!(
                    "{sql} timed out after {}s",
                    trino_schema_listing_timeout_secs()
                )
            }
        }
    }
    Err(last_err)
}

/// Unified schema discovery wrapper. Delegates to the appropriate database-specific
/// function based on the `db_type` field in the config JSON.
///
/// Config JSON schema per db_type:
///
/// - **mysql**: `{"host": "...", "port": 3306, "database": "...", "username": "...", "password": "..."}`
/// - **postgres**: `{"host": "...", "port": 5432, "database": "...", "username": "...", "password": "..."}`
/// - **clickhouse**: `{"host": "...", "port": 8123, "database": "...", "username": "default", "password": "..."}`
/// - **trino**: `{"host": "...", "port": 8443, "catalog": "...", "schema": "...", "schemas": ["..."], "username": "...", "password": "..."}`
#[derive(Debug, Default, Clone)]
pub struct SchemaDiscovery;

impl SchemaDiscovery {
    pub fn new() -> Self {
        Self
    }

    /// Discover the live schema for a datasource given its type and config JSON.
    pub async fn discover(
        &self,
        db_type: &str,
        config: &serde_json::Value,
    ) -> Result<DiscoveryOutcome, String> {
        match db_type.to_lowercase().as_str() {
            "mysql" | "tidb" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(3306) as u16;
                let database = config
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("root");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                discover_mysql(host, port, database, username, password).await
            }
            "postgres" | "postgresql" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(5432) as u16;
                let database = config
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("postgres");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                discover_postgres(host, port, database, username, password).await
            }
            "clickhouse" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(8123) as u16;
                let database = config
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                discover_clickhouse(host, port, database, username, password).await
            }
            "trino" | "presto" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(8443) as u16;
                let catalog = config
                    .get("catalog")
                    .and_then(|v| v.as_str())
                    .unwrap_or("iceberg");
                let schema = config
                    .get("schema")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let schemas = normalize_trino_schemas(
                    schema,
                    config
                        .get("schemas")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str()),
                );
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("admin");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let secure = config
                    .get("ssl")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(port == 443);
                let basic_auth = config
                    .get("basic_auth")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(!password.is_empty());
                discover_trino_multi(
                    host,
                    port,
                    catalog,
                    &schemas,
                    username,
                    Some(password),
                    secure,
                    basic_auth,
                )
                .await
            }
            "mongodb" => {
                let config: MongoConfig = serde_json::from_value(config.clone())
                    .map_err(|error| format!("invalid MongoDB config: {error}"))?;
                discover_mongodb(&config).await
            }
            other => Err(format!("unsupported db_type: {other}")),
        }
    }

    /// Discover the schema of a single table.
    pub async fn discover_table(
        &self,
        db_type: &str,
        config: &serde_json::Value,
        table_name: &str,
    ) -> Result<Option<Value>, String> {
        match db_type.to_lowercase().as_str() {
            "mysql" | "tidb" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(3306) as u16;
                let database = config
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("root");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                discover_mysql_table(host, port, database, username, password, table_name).await
            }
            "postgres" | "postgresql" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(5432) as u16;
                let database = config
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("postgres");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                discover_postgres_table(host, port, database, username, password, table_name).await
            }
            "clickhouse" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(8123) as u16;
                let database = config
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                discover_clickhouse_table(host, port, database, username, password, table_name)
                    .await
            }
            "trino" | "presto" => {
                let host = config
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("localhost");
                let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(8443) as u16;
                let catalog = config
                    .get("catalog")
                    .and_then(|v| v.as_str())
                    .unwrap_or("iceberg");
                let schema = config
                    .get("schema")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let schemas = normalize_trino_schemas(
                    schema,
                    config
                        .get("schemas")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str()),
                );
                let (_, resolved_schema, _) =
                    parse_trino_table_reference(catalog, schema, table_name);
                let schema = if schemas.iter().any(|s| s == &resolved_schema) {
                    resolved_schema.as_str()
                } else {
                    schemas.first().map(String::as_str).unwrap_or(schema)
                };
                let username = config
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("admin");
                let password = config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let secure = config
                    .get("ssl")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(port == 443);
                let basic_auth = config
                    .get("basic_auth")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(!password.is_empty());
                discover_trino_table(
                    host,
                    port,
                    catalog,
                    schema,
                    username,
                    Some(password),
                    secure,
                    basic_auth,
                    table_name,
                )
                .await
            }
            "mongodb" => {
                let config: MongoConfig = serde_json::from_value(config.clone())
                    .map_err(|error| format!("invalid MongoDB config: {error}"))?;
                discover_mongodb_table(&config, table_name).await
            }
            other => Err(format!("unsupported db_type: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mongodb_schema_sampling_flattens_nested_fields_and_tracks_nullability() {
        let mut fields = BTreeMap::new();
        collect_mongodb_fields(
            &doc! {
                "_id": mongodb::bson::oid::ObjectId::new(),
                "profile": { "city": "Shanghai", "age": 30 },
                "tags": ["paid", "mobile"],
            },
            "",
            0,
            &mut fields,
        );
        collect_mongodb_fields(
            &doc! {
                "_id": mongodb::bson::oid::ObjectId::new(),
                "profile": { "city": "Beijing", "nickname": null },
            },
            "",
            0,
            &mut fields,
        );

        assert_eq!(fields["profile.city"].observed, 2);
        assert_eq!(fields["profile.age"].observed, 1);
        assert!(fields["profile.age"].types.contains("INT32"));
        assert!(fields["tags"].types.contains("ARRAY<STRING>"));
        assert!(fields["_id"].types.contains("OBJECT_ID"));
        assert!(fields["profile.nickname"].saw_null);
    }

    #[test]
    fn trino_ident_keeps_simple_identifiers_and_quotes_special_ones() {
        assert_eq!(trino_ident("iceberg"), "iceberg");
        assert_eq!(trino_ident("mps_prod"), "mps_prod");
        assert_eq!(trino_ident("iceberg-prod"), "\"iceberg-prod\"");
        assert_eq!(trino_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn parse_trino_table_reference_supports_table_schema_and_catalog_schema() {
        assert_eq!(
            parse_trino_table_reference("iceberg", "mps_prod", "business_order"),
            (
                "iceberg".to_string(),
                "mps_prod".to_string(),
                "business_order".to_string()
            )
        );
        assert_eq!(
            parse_trino_table_reference("iceberg", "mps_prod", "ods.business_order"),
            (
                "iceberg".to_string(),
                "ods".to_string(),
                "business_order".to_string()
            )
        );
        assert_eq!(
            parse_trino_table_reference("iceberg", "mps_prod", "hive.ods.business_order"),
            (
                "hive".to_string(),
                "ods".to_string(),
                "business_order".to_string()
            )
        );
    }
}
