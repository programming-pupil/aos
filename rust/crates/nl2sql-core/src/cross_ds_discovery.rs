//! Cross-datasource relationship discovery.
//!
//! Scans all datasources belonging to a tenant and automatically discovers
//! semantic relationships between columns across datasources (e.g. `users.email`
//! in DS-A relates to `customers.contact_email` in DS-B).
//!
//! Discovered relations are stored in `nl2sql_cross_datasource_relations` with
//! `match_type='auto'` and can be promoted to `match_type='foreign_key'` or
//! `match_type='custom'` after admin review.
//!
//! Discovery strategy:
//! 1. **Name matching**: columns with identical names (email, user_id, product_id, etc.)
//! 2. **Type compatibility**: same SQL type family (VARCHAR email ↔ VARCHAR email)
//! 3. **Semantic similarity**: column embedding cosine similarity > threshold
//!    (requires column embeddings to already be computed)

use sqlx::SqlitePool;
use std::collections::HashSet;

/// Maximum number of auto-discovered cross-datasource relations to insert per run.
/// Prevents flooding the relations table on first discovery.
const MAX_AUTO_DISCOVERED_PER_RUN: usize = 1000;

/// Discover and persist cross-datasource relationships for a tenant.
/// Called by a background job or after schema refresh.
/// Returns the number of new relations discovered.
pub async fn discover_cross_datasource_relations(
    db: &SqlitePool,
    tenant_id: &str,
) -> anyhow::Result<usize> {
    // Load all datasources for this tenant that have schema_info populated
    let datasources: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, db_type FROM data_sources \
         WHERE tenant_id = ? AND schema_info IS NOT NULL AND schema_info != ''",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    if datasources.len() < 2 {
        return Ok(0); // Need at least 2 datasources for cross-ds relations
    }

    // Collect all column schemas across all datasources
    let mut all_columns: Vec<ColumnMeta> = Vec::new();
    for (ds_id, db_type) in &datasources {
        let cols = load_datasource_columns(db, ds_id).await?;
        all_columns.extend(cols.into_iter().map(|c| ColumnMeta {
            datasource_id: ds_id.clone(),
            _db_type: db_type.clone(),
            table_name: c.table_name,
            column_name: c.column_name,
            data_type: c.data_type,
        }));
    }

    if all_columns.len() < 2 {
        return Ok(0);
    }

    // Find candidate relations: same column name across different datasources
    let candidates = find_name_matched_candidates(&all_columns);

    // Filter by type compatibility
    let compatible: Vec<_> = candidates
        .into_iter()
        .filter(|c| compatible_types(&c.left.data_type, &c.right.data_type))
        .collect();

    // Deduplicate against existing auto-discovered relations
    let existing: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT relation_hash FROM nl2sql_cross_datasource_relations \
         WHERE tenant_id = ? AND match_type = 'id'",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    .map(|h: Vec<String>| h.into_iter().collect())
    .unwrap_or_default();

    let new_relations: Vec<_> = compatible
        .into_iter()
        .filter(|c| !existing.contains(&c.relation_hash))
        .take(MAX_AUTO_DISCOVERED_PER_RUN)
        .collect();

    if new_relations.is_empty() {
        return Ok(0);
    }

    // Batch insert
    let mut tx = sqlx::Acquire::begin(db).await?;
    let mut inserted = 0usize;

    for rel in &new_relations {
        // Set confidence based on match type heuristic.
        // verified=false for auto-discovered unless confidence >= 0.6 (requires LLM confirmation).
        let confidence = if rel.left.column_name.to_lowercase().ends_with("_id") {
            0.85
        } else if rel.left.column_name.to_lowercase().contains("email") {
            0.65
        } else if rel.left.column_name.to_lowercase().contains("name") {
            0.50
        } else {
            0.60
        };
        let verified = confidence >= 0.6;

        sqlx::query(
            "INSERT OR IGNORE INTO nl2sql_cross_datasource_relations \
             (tenant_id, left_datasource_id, left_table, left_column, \
              right_datasource_id, right_table, right_column, \
              relation_hash, semantic_description, match_type, confidence, verified) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'auto', ?, ?)",
        )
        .bind(tenant_id)
        .bind(&rel.left.datasource_id)
        .bind(&rel.left.table_name)
        .bind(&rel.left.column_name)
        .bind(&rel.right.datasource_id)
        .bind(&rel.right.table_name)
        .bind(&rel.right.column_name)
        .bind(&rel.relation_hash)
        .bind(&rel.description)
        .bind(confidence)
        .bind(verified)
        .execute(&mut *tx)
        .await?;
        inserted += 1;
    }

    tx.commit().await?;

    tracing::info!(
        tenant_id = %tenant_id,
        datasources = datasources.len(),
        columns_scanned = all_columns.len(),
        relations_discovered = inserted,
        "cross_datasource_discovery: completed"
    );

    Ok(inserted)
}

/// Column metadata loaded from a datasource's schema_info JSON.
struct ColumnMeta {
    datasource_id: String,
    _db_type: String,
    table_name: String,
    column_name: String,
    data_type: String,
}

struct ColumnRef {
    datasource_id: String,
    table_name: String,
    column_name: String,
    data_type: String,
}

struct CandidateRelation {
    left: ColumnRef,
    right: ColumnRef,
    relation_hash: String,
    description: String,
}

/// Load all column metadata from a datasource's schema_info JSON.
async fn load_datasource_columns(
    db: &SqlitePool,
    datasource_id: &str,
) -> anyhow::Result<Vec<ColumnMeta>> {
    let schema_info: Option<String> =
        sqlx::query_scalar("SELECT schema_info FROM data_sources WHERE id = ?")
            .bind(datasource_id)
            .fetch_optional(db)
            .await?;

    let schema_info = match schema_info {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(Vec::new()),
    };

    let json: serde_json::Value = serde_json::from_str(&schema_info)
        .map_err(|e| anyhow::anyhow!("failed to parse schema_info for {}: {}", datasource_id, e))?;

    let tables = json
        .get("tables")
        .and_then(|v| v.as_array())
        .map(|a| a.to_vec());
    let tables = match tables {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let mut cols = Vec::new();
    for table in tables {
        let table_name = table
            .get("table_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let columns = table
            .get("columns")
            .and_then(|v| v.as_array())
            .map(|a| a.to_vec());
        let columns = match columns {
            Some(c) => c,
            None => continue,
        };

        for col in columns {
            let column_name = col
                .get("column_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let data_type = col
                .get("data_type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if !table_name.is_empty() && !column_name.is_empty() {
                cols.push(ColumnMeta {
                    datasource_id: datasource_id.to_string(),
                    _db_type: String::new(),
                    table_name: table_name.clone(),
                    column_name,
                    data_type,
                });
            }
        }
    }

    Ok(cols)
}

/// Find candidate relations between columns with matching names across datasources.
fn find_name_matched_candidates(columns: &[ColumnMeta]) -> Vec<CandidateRelation> {
    // Group columns by (normalized) column name
    let mut by_name: std::collections::HashMap<String, Vec<&ColumnMeta>> =
        std::collections::HashMap::new();

    for col in columns {
        let key = col.column_name.to_lowercase();
        by_name.entry(key).or_default().push(col);
    }

    let mut candidates = Vec::new();

    for (col_name, cols) in &by_name {
        if cols.len() < 2 {
            continue;
        }

        // Columns with same name across at least 2 datasources are candidates
        let ds_set: HashSet<_> = cols.iter().map(|c| &c.datasource_id).collect();
        if ds_set.len() < 2 {
            continue;
        }

        // Create pairwise relations between all datasource pairs
        for i in 0..cols.len() {
            for j in (i + 1)..cols.len() {
                let left = &cols[i];
                let right = &cols[j];

                if left.datasource_id == right.datasource_id {
                    continue; // Skip same-datasource pairs
                }

                let relation_hash = sha256_relation_hash(
                    &left.datasource_id,
                    &left.table_name,
                    &left.column_name,
                    &right.datasource_id,
                    &right.table_name,
                    &right.column_name,
                );

                let description = format!(
                    "Auto-matched: {}.{} in '{}' relates to {}.{} in '{}' (column name: {})",
                    left.table_name,
                    left.column_name,
                    left.datasource_id,
                    right.table_name,
                    right.column_name,
                    right.datasource_id,
                    col_name
                );

                candidates.push(CandidateRelation {
                    left: ColumnRef {
                        datasource_id: left.datasource_id.clone(),
                        table_name: left.table_name.clone(),
                        column_name: left.column_name.clone(),
                        data_type: left.data_type.clone(),
                    },
                    right: ColumnRef {
                        datasource_id: right.datasource_id.clone(),
                        table_name: right.table_name.clone(),
                        column_name: right.column_name.clone(),
                        data_type: right.data_type.clone(),
                    },
                    relation_hash,
                    description,
                });
            }
        }
    }

    candidates
}

/// SHA256-based hash for deduplication (matches the DB column definition).
fn sha256_relation_hash(lds: &str, lt: &str, lc: &str, rds: &str, rt: &str, rc: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(lds.as_bytes());
    hasher.update(b"|");
    hasher.update(lt.as_bytes());
    hasher.update(b"|");
    hasher.update(lc.as_bytes());
    hasher.update(b"|");
    hasher.update(rds.as_bytes());
    hasher.update(b"|");
    hasher.update(rt.as_bytes());
    hasher.update(b"|");
    hasher.update(rc.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Check if two SQL type strings are type-compatible for cross-datasource linking.
/// Very permissive — primary key / foreign key columns in different DBs may have
/// slightly different type names but be semantically equivalent.
fn compatible_types(type_a: &str, type_b: &str) -> bool {
    if type_a == type_b {
        return true;
    }
    let a = type_a.to_lowercase();
    let b = type_b.to_lowercase();

    // Strip size/precision annotations: VARCHAR(255) → VARCHAR
    let strip_size = |s: &str| -> String { s.split('(').next().unwrap_or(s).trim().to_string() };

    let (na, nb) = (strip_size(&a), strip_size(&b));
    if na == nb {
        return true;
    }

    // VARCHAR ≈ CHAR ≈ TEXT
    let text_family = |s: &str| s.contains("char") || s.contains("text") || s == "string";
    if text_family(&na) && text_family(&nb) {
        return true;
    }

    // INT family
    let int_family = |s: &str| {
        s.contains("int") || s == "integer" || s == "bigint" || s == "smallint" || s == "tinyint"
    };
    if int_family(&na) && int_family(&nb) {
        return true;
    }

    // DECIMAL ≈ FLOAT ≈ DOUBLE
    let numeric_family = |s: &str| {
        s.contains("decimal")
            || s.contains("numeric")
            || s == "float"
            || s == "double"
            || s == "real"
    };
    if numeric_family(&na) && numeric_family(&nb) {
        return true;
    }

    // DATE ≈ DATETIME ≈ TIMESTAMP
    let date_family =
        |s: &str| s.contains("date") || s.contains("time") || s == "timestamp" || s == "datetime";
    if date_family(&na) && date_family(&nb) {
        return true;
    }

    false
}
