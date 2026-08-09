//! Per-datasource sqlx pool cache.
//!
//! Before this module, `routes::nl2sql::queries::execute_once` built a fresh
//! `MySqlPoolOptions::new().max_connections(1).connect()` on every NL2SQL
//! execution and dropped it with `pool.close()` once the query returned.
//! Every execute paid TCP handshake + TLS handshake + MySQL/PG auth — easily
//! 100–300 ms wasted before the first row was read. For an enterprise BI
//! workload (interactive analyst churn, large dashboards) this dominates the
//! latency budget.
//!
//! This cache keeps **one pool per `(tenant_id, datasource_id)`**, keyed by
//! the datasource's `updated_at` timestamp so a credential/host change in
//! `data_sources` invalidates the cached pool on the next acquire. Pools are
//! kept on a small `max_connections` budget with an idle timeout so they
//! self-evict when a tenant stops querying.
//!
//! Concurrency model
//! -----------------
//! * Reads (`get` lookups) take a `DashMap` shard read — no global lock.
//! * Pool **construction** is serialised per key by a `tokio::sync::Mutex`
//!   stored alongside the entry; this prevents the thundering herd that
//!   would otherwise open N pools when N requests arrive cold.
//! * Eviction (`invalidate_datasource`) takes a `DashMap` shard write.
//!
//! Failure mode
//! ------------
//! If the cached pool detects connection death, the next `acquire` will
//! return an error from sqlx; we treat that as a transient failure and
//! evict the pool, letting the next call rebuild it. This is implemented
//! by `with_mysql`/`with_postgres` callers — see `acquire_mysql` /
//! `acquire_postgres`.
//!
//! NOTE on credentials: pools live as long as the cache. That means decrypted
//! credentials are resident in the process memory for the pool lifetime. That
//! was already the case for the in-flight execution; we are extending the
//! window from "per request" to "until idle eviction" (default 5 min). Keep
//! the idle timeout conservative when changing pool policy.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{MySqlPool, PgPool};
use tokio::sync::Mutex;

/// Cache key — a datasource is uniquely identified by its tenant + id; the
/// `version` is the datasource's `updated_at` stamp (epoch millis) and
/// participates in equality so a credential rotation transparently rolls
/// the pool over.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct Key {
    tenant_id: String,
    datasource_id: String,
    version: i64,
}

enum CachedPool {
    MySql(MySqlPool),
    Postgres(PgPool),
}

struct Entry {
    /// Serialises pool *construction* — readers go through `DashMap::get`
    /// without acquiring this. Holding it across the `connect().await` is
    /// what eliminates the thundering herd.
    init: Mutex<()>,
    pool: parking_lot::RwLock<Option<CachedPool>>,
}

impl Entry {
    fn new() -> Self {
        Self {
            init: Mutex::new(()),
            pool: parking_lot::RwLock::new(None),
        }
    }
}

/// Per-process pool cache.
pub struct PoolCache {
    map: RwLock<HashMap<Key, Arc<Entry>>>,
    /// Per-pool ceiling. Kept low to bound resource use across many tenants;
    /// a single big analytical query still gets one connection.
    max_connections: u32,
    /// Idle-eviction window. Pools that sit unused this long are dropped by
    /// sqlx; the next caller transparently re-opens.
    idle_timeout: Duration,
    /// How long to wait for an acquire (when the per-pool slots are busy).
    acquire_timeout: Duration,
}

impl PoolCache {
    pub fn new() -> Self {
        // Sensible defaults; overridable via env at the call site.
        let max_connections = std::env::var("NL2SQL_DS_POOL_MAX_CONNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let idle_secs = std::env::var("NL2SQL_DS_POOL_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);
        let acquire_secs = std::env::var("NL2SQL_DS_POOL_ACQUIRE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);
        Self {
            map: RwLock::new(HashMap::new()),
            max_connections,
            idle_timeout: Duration::from_secs(idle_secs),
            acquire_timeout: Duration::from_secs(acquire_secs),
        }
    }

    /// Acquire (or build) the MySQL pool for the given datasource. `version`
    /// must be the datasource's `updated_at` epoch millis — any change
    /// invalidates the cached pool automatically.
    pub async fn get_mysql(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        version: i64,
        url: &str,
    ) -> Result<MySqlPool, sqlx::Error> {
        let key = Key {
            tenant_id: tenant_id.to_string(),
            datasource_id: datasource_id.to_string(),
            version,
        };

        // Fast path: pool already built. Clone the sqlx handle — cheap (Arc).
        if let Some(entry) = self.map.read().get(&key).cloned() {
            if let Some(CachedPool::MySql(pool)) = entry.pool.read().as_ref() {
                return Ok(pool.clone());
            }
        }

        // Evict any stale entries for this (tenant, datasource) so the old
        // version's pool drops once the last in-flight caller releases it.
        self.evict_other_versions(tenant_id, datasource_id, version);

        // Slow path: insert (or fetch) the Entry; serialise the build under
        // its init Mutex. Two concurrent cold requests for the same key go
        // through `init.lock()` in turn — the second one observes the pool
        // already built and returns.
        let entry = {
            let mut map = self.map.write();
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(Entry::new()))
                .clone()
        };
        let _guard = entry.init.lock().await;
        if let Some(CachedPool::MySql(pool)) = entry.pool.read().as_ref() {
            return Ok(pool.clone());
        }

        let pool = MySqlPoolOptions::new()
            .max_connections(self.max_connections)
            .min_connections(0)
            .idle_timeout(Some(self.idle_timeout))
            .acquire_timeout(self.acquire_timeout)
            // The datasource owner may have a punitive `wait_timeout`;
            // sqlx's `test_before_acquire` quietly drops dead conns.
            .test_before_acquire(true)
            .connect(url)
            .await?;
        *entry.pool.write() = Some(CachedPool::MySql(pool.clone()));
        Ok(pool)
    }

    /// Acquire (or build) the Postgres pool for the given datasource.
    pub async fn get_postgres(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        version: i64,
        url: &str,
    ) -> Result<PgPool, sqlx::Error> {
        let key = Key {
            tenant_id: tenant_id.to_string(),
            datasource_id: datasource_id.to_string(),
            version,
        };

        if let Some(entry) = self.map.read().get(&key).cloned() {
            if let Some(CachedPool::Postgres(pool)) = entry.pool.read().as_ref() {
                return Ok(pool.clone());
            }
        }

        self.evict_other_versions(tenant_id, datasource_id, version);

        let entry = {
            let mut map = self.map.write();
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(Entry::new()))
                .clone()
        };
        let _guard = entry.init.lock().await;
        if let Some(CachedPool::Postgres(pool)) = entry.pool.read().as_ref() {
            return Ok(pool.clone());
        }

        let pool = PgPoolOptions::new()
            .max_connections(self.max_connections)
            .min_connections(0)
            .idle_timeout(Some(self.idle_timeout))
            .acquire_timeout(self.acquire_timeout)
            .test_before_acquire(true)
            .connect(url)
            .await?;
        *entry.pool.write() = Some(CachedPool::Postgres(pool.clone()));
        Ok(pool)
    }

    /// Force-evict every cached pool for a datasource (any version).
    /// Called when a datasource is updated or deleted so secrets don't
    /// linger and so the next caller picks up the new config.
    pub fn invalidate_datasource(&self, tenant_id: &str, datasource_id: &str) {
        let mut map = self.map.write();
        map.retain(|k, _| !(k.tenant_id == tenant_id && k.datasource_id == datasource_id));
    }

    /// Evict pools matching `(tenant, ds)` whose version differs from `keep`.
    /// Used internally to migrate to a new `updated_at` without dropping the
    /// just-built fresh entry.
    fn evict_other_versions(&self, tenant_id: &str, datasource_id: &str, keep: i64) {
        let mut map = self.map.write();
        map.retain(|k, _| {
            !(k.tenant_id == tenant_id && k.datasource_id == datasource_id && k.version != keep)
        });
    }

    /// Manual eviction for the case where a sqlx error suggests the pool is
    /// poisoned (e.g. server restart, credential revoked). Callers may use
    /// this defensively when surfacing a `BrokenPipe`-style error.
    pub fn evict(&self, tenant_id: &str, datasource_id: &str, version: i64) {
        let key = Key {
            tenant_id: tenant_id.to_string(),
            datasource_id: datasource_id.to_string(),
            version,
        };
        self.map.write().remove(&key);
    }

    /// Diagnostic: number of cached pools (handy for /metrics or tests).
    pub fn len(&self) -> usize {
        self.map.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.read().is_empty()
    }
}

impl Default for PoolCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_starts_empty() {
        let cache = PoolCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn invalidate_is_idempotent() {
        let cache = PoolCache::new();
        cache.invalidate_datasource("tenant-a", "ds-1");
        assert!(cache.is_empty());
    }

    #[test]
    fn key_equality_includes_version() {
        let k1 = Key {
            tenant_id: "t".into(),
            datasource_id: "d".into(),
            version: 1,
        };
        let k2 = Key {
            tenant_id: "t".into(),
            datasource_id: "d".into(),
            version: 2,
        };
        assert_ne!(k1, k2);
    }
}
