use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

fn ttl_hours() -> i64 {
    std::env::var("NL2SQL_RESULT_CACHE_TTL_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

fn max_rows() -> usize {
    std::env::var("NL2SQL_RESULT_CACHE_MAX_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000)
}

pub fn question_hash(tenant_id: &str, datasource_id: &str, question: &str) -> String {
    let normalized = question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut h = Sha256::new();
    h.update(tenant_id.as_bytes());
    h.update(b"|");
    h.update(datasource_id.as_bytes());
    h.update(b"|");
    h.update(normalized.as_bytes());
    format!("{:x}", h.finalize())
}

pub struct CacheHit {
    pub generated_sql: String,
    pub result_snapshot: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLineage {
    pub intent_hash: String,
    pub schema_hash: String,
    pub metric_contracts_hash: String,
    pub join_contracts_hash: String,
    pub policy_hash: String,
    pub compiler_version: String,
}

pub async fn lookup(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    hash: &str,
    lineage: &CacheLineage,
) -> Option<CacheHit> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT generated_sql, result_snapshot \
         FROM nl2sql_result_cache \
         WHERE tenant_id = ? AND datasource_id = ? AND question_hash = ? \
           AND intent_hash = ? AND schema_hash = ? \
           AND metric_contracts_hash = ? AND join_contracts_hash = ? \
           AND policy_hash = ? AND compiler_version = ? \
           AND expires_at > CURRENT_TIMESTAMP AND invalidated_at IS NULL \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(hash)
    .bind(&lineage.intent_hash)
    .bind(&lineage.schema_hash)
    .bind(&lineage.metric_contracts_hash)
    .bind(&lineage.join_contracts_hash)
    .bind(&lineage.policy_hash)
    .bind(&lineage.compiler_version)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if let Some((sql, snapshot_str)) = row {
        let db2 = db.clone();
        let h = hash.to_string();
        let t = tenant_id.to_string();
        let d = datasource_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = sqlx::query(
                "UPDATE nl2sql_result_cache SET hit_count = hit_count + 1 \
                 WHERE tenant_id = ? AND datasource_id = ? AND question_hash = ?",
            )
            .bind(&t)
            .bind(&d)
            .bind(&h)
            .execute(&db2)
            .await
            {
                tracing::warn!(
                    error = %e,
                    tenant_id = %t,
                    datasource_id = %d,
                    question_hash = %h,
                    "nl2sql result cache: failed to increment hit_count (lookup)"
                );
            }
        });

        let result_snapshot = snapshot_str
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        Some(CacheHit {
            generated_sql: sql,
            result_snapshot,
        })
    } else {
        None
    }
}

pub async fn store(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    hash: &str,
    question: &str,
    generated_sql: &str,
    query_id: Option<&str>,
    rows: Option<&[serde_json::Value]>,
    lineage: &CacheLineage,
) {
    let snapshot = rows
        .map(|r| {
            let capped: Vec<_> = r.iter().take(max_rows()).cloned().collect();
            serde_json::to_string(&capped).ok()
        })
        .flatten();

    if let Err(e) = sqlx::query(
        "INSERT INTO nl2sql_result_cache \
           (tenant_id, datasource_id, question_hash, question, generated_sql, query_id,
            result_snapshot, expires_at, intent_hash, schema_hash,
            metric_contracts_hash, join_contracts_hash, policy_hash, compiler_version) \
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime(CURRENT_TIMESTAMP, printf('%+d hours', ?)),
                 ?, ?, ?, ?, ?, ?) \
         ON CONFLICT DO UPDATE SET \
           generated_sql = excluded.generated_sql, \
           query_id = COALESCE(query_id, excluded.query_id), \
           result_snapshot = COALESCE(excluded.result_snapshot, result_snapshot), \
           intent_hash = excluded.intent_hash, \
           schema_hash = excluded.schema_hash, \
           metric_contracts_hash = excluded.metric_contracts_hash, \
           join_contracts_hash = excluded.join_contracts_hash, \
           policy_hash = excluded.policy_hash, \
           compiler_version = excluded.compiler_version, \
           expires_at = excluded.expires_at, \
           invalidated_at = NULL, \
           hit_count = 0",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(hash)
    .bind(question)
    .bind(generated_sql)
    .bind(query_id)
    .bind(snapshot)
    .bind(ttl_hours())
    .bind(&lineage.intent_hash)
    .bind(&lineage.schema_hash)
    .bind(&lineage.metric_contracts_hash)
    .bind(&lineage.join_contracts_hash)
    .bind(&lineage.policy_hash)
    .bind(&lineage.compiler_version)
    .execute(db)
    .await
    {
        tracing::warn!(
            error = %e,
            tenant_id = %tenant_id,
            datasource_id = %datasource_id,
            question_hash = %hash,
            query_id = ?query_id,
            "nl2sql result cache: failed to store cache row"
        );
    }
}

pub async fn invalidate_datasource(db: &SqlitePool, tenant_id: &str, datasource_id: &str) {
    if let Err(e) = sqlx::query(
        "UPDATE nl2sql_result_cache SET invalidated_at = CURRENT_TIMESTAMP \
         WHERE tenant_id = ? AND datasource_id = ? AND invalidated_at IS NULL",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            error = %e,
            tenant_id = %tenant_id,
            datasource_id = %datasource_id,
            "nl2sql result cache: failed to invalidate datasource cache"
        );
    }
}

pub enum CacheLookupResult {
    Hit(CacheHit),
    Expired,
    NotFound,
}

/// Lookup result cache by (tenant_id, query_id).
/// Returns the cache entry only if it is valid (result_snapshot is present).
/// If the entry exists but has no result_snapshot, returns `CacheLookupResult::Expired`.
pub async fn lookup_by_query_id(
    db: &SqlitePool,
    tenant_id: &str,
    query_id: &str,
) -> CacheLookupResult {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT generated_sql, result_snapshot \
         FROM nl2sql_result_cache \
         WHERE tenant_id = ? AND query_id = ? \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(query_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    match row {
        None => CacheLookupResult::NotFound,
        Some((sql, snapshot_str)) => {
            if snapshot_str.is_none() || snapshot_str.as_deref() == Some("") {
                CacheLookupResult::Expired
            } else {
                let db2 = db.clone();
                let t = tenant_id.to_string();
                let q = query_id.to_string();
                tokio::spawn(async move {
                    if let Err(e) = sqlx::query(
                        "UPDATE nl2sql_result_cache SET hit_count = hit_count + 1 \
                         WHERE tenant_id = ? AND query_id = ?",
                    )
                    .bind(&t)
                    .bind(&q)
                    .execute(&db2)
                    .await
                    {
                        tracing::warn!(
                            error = %e,
                            tenant_id = %t,
                            query_id = %q,
                            "nl2sql result cache: failed to increment hit_count (query_id lookup)"
                        );
                    }
                });

                let result_snapshot = snapshot_str
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok());
                CacheLookupResult::Hit(CacheHit {
                    generated_sql: sql,
                    result_snapshot,
                })
            }
        }
    }
}

/// Update the result_snapshot in the cache for a given query.
/// Called after SQL execution completes so the snapshot is available for the explain endpoint.
pub async fn update_snapshot(
    db: &SqlitePool,
    tenant_id: &str,
    query_id: &str,
    datasource_id: &str,
    generated_sql: &str,
    rows: &[serde_json::Value],
) {
    let capped: Vec<_> = rows.iter().take(max_rows()).cloned().collect();
    let snapshot = serde_json::to_string(&capped).ok();
    if let Err(e) = sqlx::query(
        "UPDATE nl2sql_result_cache
         SET result_snapshot = ?, generated_sql = ?, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND datasource_id = ? AND query_id = ?
           AND expires_at > CURRENT_TIMESTAMP AND invalidated_at IS NULL",
    )
    .bind(snapshot)
    .bind(generated_sql)
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(query_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            error = %e,
            tenant_id = %tenant_id,
            query_id = %query_id,
            datasource_id = %datasource_id,
            "nl2sql result cache: failed to upsert result snapshot"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn cache_db() -> SqlitePool {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE nl2sql_result_cache (
                tenant_id TEXT NOT NULL,
                datasource_id TEXT NOT NULL,
                question_hash TEXT NOT NULL,
                question TEXT NOT NULL,
                generated_sql TEXT NOT NULL,
                query_id TEXT,
                result_snapshot TEXT,
                intent_hash TEXT NOT NULL DEFAULT '',
                schema_hash TEXT NOT NULL DEFAULT '',
                metric_contracts_hash TEXT NOT NULL DEFAULT '',
                join_contracts_hash TEXT NOT NULL DEFAULT '',
                policy_hash TEXT NOT NULL DEFAULT '',
                compiler_version TEXT NOT NULL DEFAULT '',
                expires_at TEXT NOT NULL,
                invalidated_at TEXT,
                hit_count INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                UNIQUE (tenant_id, datasource_id, question_hash)
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        db
    }

    fn lineage() -> CacheLineage {
        CacheLineage {
            intent_hash: "intent-v1".into(),
            schema_hash: "schema-v1".into(),
            metric_contracts_hash: "metrics-v1".into(),
            join_contracts_hash: "joins-v1".into(),
            policy_hash: "policy-v1".into(),
            compiler_version: "compiler-v1".into(),
        }
    }

    #[tokio::test]
    async fn cache_requires_the_complete_semantic_lineage() {
        let db = cache_db().await;
        let hash = question_hash("tenant", "datasource", "orders yesterday");
        let baseline = lineage();
        store(
            &db,
            "tenant",
            "datasource",
            &hash,
            "orders yesterday",
            "SELECT COUNT(*) FROM orders",
            Some("query-1"),
            None,
            &baseline,
        )
        .await;
        assert!(lookup(&db, "tenant", "datasource", &hash, &baseline)
            .await
            .is_some());

        for changed in [
            CacheLineage {
                schema_hash: "schema-v2".into(),
                ..baseline.clone()
            },
            CacheLineage {
                metric_contracts_hash: "metrics-v2".into(),
                ..baseline.clone()
            },
            CacheLineage {
                join_contracts_hash: "joins-v2".into(),
                ..baseline.clone()
            },
            CacheLineage {
                policy_hash: "policy-v2".into(),
                ..baseline.clone()
            },
            CacheLineage {
                compiler_version: "compiler-v2".into(),
                ..baseline.clone()
            },
            CacheLineage {
                intent_hash: "intent-v2".into(),
                ..baseline.clone()
            },
        ] {
            assert!(lookup(&db, "tenant", "datasource", &hash, &changed)
                .await
                .is_none());
        }
    }

    #[tokio::test]
    async fn legacy_or_unbound_cache_rows_are_never_released() {
        let db = cache_db().await;
        let hash = question_hash("tenant", "datasource", "roi");
        sqlx::query(
            "INSERT INTO nl2sql_result_cache
                (tenant_id, datasource_id, question_hash, question, generated_sql, expires_at)
             VALUES ('tenant', 'datasource', ?, 'roi', 'SELECT secret FROM wrong_table',
                     datetime(CURRENT_TIMESTAMP, '+1 hour'))",
        )
        .bind(&hash)
        .execute(&db)
        .await
        .unwrap();
        assert!(lookup(&db, "tenant", "datasource", &hash, &lineage())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn snapshot_update_cannot_create_or_widen_a_cache_binding() {
        let db = cache_db().await;
        update_snapshot(
            &db,
            "tenant",
            "query-missing",
            "datasource",
            "SELECT * FROM wrong_table",
            &[serde_json::json!({"secret": true})],
        )
        .await;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nl2sql_result_cache")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
