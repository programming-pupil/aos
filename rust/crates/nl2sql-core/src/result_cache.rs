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

pub async fn lookup(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    hash: &str,
) -> Option<CacheHit> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT generated_sql, result_snapshot \
         FROM nl2sql_result_cache \
         WHERE tenant_id = ? AND datasource_id = ? AND question_hash = ? \
           AND expires_at > CURRENT_TIMESTAMP AND invalidated_at IS NULL \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(hash)
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
) {
    let snapshot = rows
        .map(|r| {
            let capped: Vec<_> = r.iter().take(max_rows()).cloned().collect();
            serde_json::to_string(&capped).ok()
        })
        .flatten();

    if let Err(e) = sqlx::query(
        "INSERT INTO nl2sql_result_cache \
           (tenant_id, datasource_id, question_hash, question, generated_sql, query_id, result_snapshot, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime(CURRENT_TIMESTAMP, printf('%+d hours', ?))) \
         ON CONFLICT DO UPDATE SET \
           generated_sql = excluded.generated_sql, \
           query_id = COALESCE(query_id, excluded.query_id), \
           result_snapshot = COALESCE(excluded.result_snapshot, result_snapshot), \
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
    let question = sqlx::query_scalar::<_, Option<String>>(
        "SELECT question FROM nl2sql_queries WHERE id = ? AND tenant_id = ? LIMIT 1",
    )
    .bind(query_id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or_else(|| query_id.to_string());
    let hash = question_hash(tenant_id, datasource_id, &question);

    if let Err(e) = sqlx::query(
        "INSERT INTO nl2sql_result_cache \
           (tenant_id, datasource_id, question_hash, question, generated_sql, query_id, result_snapshot, expires_at, invalidated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime(CURRENT_TIMESTAMP, printf('%+d hours', ?)), NULL) \
         ON CONFLICT DO UPDATE SET \
           generated_sql = excluded.generated_sql, \
           query_id = excluded.query_id, \
           result_snapshot = excluded.result_snapshot, \
           expires_at = excluded.expires_at, \
           invalidated_at = NULL",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(&hash)
    .bind(&question)
    .bind(generated_sql)
    .bind(query_id)
    .bind(snapshot)
    .bind(ttl_hours())
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
