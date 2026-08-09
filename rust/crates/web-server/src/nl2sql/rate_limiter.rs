//! Tenant-level rate limiter for NL2SQL operations using a token bucket.
//!
//! Two enforcement modes are supported:
//!
//! 1. **In-memory (default)** — `parking_lot::RwLock<HashMap<tenant, TokenBucket>>`.
//!    O(1) per check, zero RPC. Limits hold **per replica**, so an N-replica
//!    deployment effectively sees `N × rpm` of headroom per tenant. This is
//!    fine for single-replica and small clusters.
//!
//! 2. **DB-backed (`NL2SQL_DISTRIBUTED_RATE_LIMIT=1`)** — the in-memory bucket
//!    is consulted first (fast path); on miss the limiter performs a
//!    serialised UPSERT against `nl2sql_rate_limit_buckets` so a single
//!    tenant cannot burst past `rpm` across all replicas. The DB row is
//!    keyed by `(tenant_id, bucket='llm')` and uses lazy refill (writers
//!    compute elapsed since `last_refill_at` and replenish on read).
//!
//! Both paths share the same `TokenBucket` math so behaviour is identical
//! semantically; the DB path adds one round-trip on rejection (no DB write
//! on the happy path of "in-memory says yes").
//!
//! Configuration:
//!   * `NL2SQL_LLM_RATE_LIMIT_RPM` — requests per minute (default 60).
//!   * `NL2SQL_DISTRIBUTED_RATE_LIMIT` — `"1"` to enable DB enforcement.

// The rpm-as-f64 cast is intentional: token-bucket math is float-based and
// rpm is a small integer (env-supplied, bounded). The truncation cast in
// `retry_after_secs` is bounded by the bucket capacity which never exceeds
// reasonable u32 ranges. Both are safe in practice.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use parking_lot::RwLock;
use sqlx::Row;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Token bucket rate limiter scoped per tenant.
pub struct TenantRateLimiter {
    /// Per-tenant in-memory buckets.
    buckets: RwLock<HashMap<String, TokenBucket>>,
    /// How many tokens each bucket holds (requests per minute).
    requests_per_minute: usize,
    /// When true, the limiter additionally consults
    /// `nl2sql_rate_limit_buckets` so multi-replica deployments share state.
    distributed: bool,
}

struct TokenBucket {
    /// Available tokens (requests remaining).
    tokens: f64,
    /// When the bucket was last refilled.
    last_refill: Instant,
    /// Replenish rate: tokens per second.
    replenish_rate: f64,
    /// Max tokens (requests per minute) for refill ceiling.
    max_tokens: f64,
}

impl TokenBucket {
    fn new(requests_per_minute: usize) -> Self {
        let tokens = requests_per_minute as f64;
        Self {
            tokens,
            last_refill: Instant::now(),
            replenish_rate: requests_per_minute as f64 / 60.0,
            max_tokens: requests_per_minute as f64,
        }
    }

    fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        let new_tokens = elapsed * self.replenish_rate;
        self.tokens = (self.tokens + new_tokens).min(self.max_tokens);
        self.last_refill = Instant::now();
    }
}

impl TenantRateLimiter {
    pub fn new(requests_per_minute: usize, distributed: bool) -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            requests_per_minute,
            distributed,
        }
    }

    /// Try to acquire a token for the given tenant (in-memory only).
    /// Returns true if the request is allowed; false means rate limit
    /// exceeded **on this replica**.
    fn try_acquire_local(&self, tenant_id: &str) -> bool {
        let mut buckets = self.buckets.write();
        let bucket = buckets
            .entry(tenant_id.to_string())
            .or_insert_with(|| TokenBucket::new(self.requests_per_minute));
        bucket.try_acquire()
    }

    /// Try to acquire a token, optionally consulting the DB for cluster-wide
    /// enforcement. Falls back to in-memory if the DB path errors so a
    /// transient DB blip doesn't deny user traffic.
    pub async fn try_acquire(&self, db: &SqlitePool, tenant_id: &str) -> bool {
        // Fast path: local bucket. If it says no, no need to hit the DB —
        // we can't possibly allow the request.
        if !self.try_acquire_local(tenant_id) {
            return false;
        }
        if !self.distributed {
            return true;
        }
        // Slow path: distributed enforcement. We've already taken a local
        // token; consult the DB to ensure cluster-wide budget. On any DB
        // error, we keep the local decision (fail-open for availability).
        match try_acquire_db(db, tenant_id, self.requests_per_minute).await {
            Ok(true) => true,
            Ok(false) => {
                // Cluster-wide budget exhausted; return the local token by
                // re-incrementing the bucket so the user isn't double-charged.
                self.refund_local(tenant_id);
                false
            }
            Err(e) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    error = %e,
                    "distributed rate limiter DB error — falling back to local decision"
                );
                true
            }
        }
    }

    fn refund_local(&self, tenant_id: &str) {
        let mut buckets = self.buckets.write();
        if let Some(bucket) = buckets.get_mut(tenant_id) {
            bucket.tokens = (bucket.tokens + 1.0).min(bucket.max_tokens);
        }
    }

    /// Returns the configured requests-per-minute limit.
    pub fn limit(&self) -> usize {
        self.requests_per_minute
    }

    /// Returns seconds until the next token is available for a tenant.
    /// Best-effort: reflects the local bucket only.
    pub fn retry_after_secs(&self, tenant_id: &str) -> u64 {
        let buckets = self.buckets.read();
        match buckets.get(tenant_id) {
            Some(b) if b.tokens < 1.0 => {
                let needed = 1.0 - b.tokens;
                let secs = needed / b.replenish_rate;
                secs.ceil() as u64
            }
            _ => 0,
        }
    }
}

impl Default for TenantRateLimiter {
    fn default() -> Self {
        let rpm = std::env::var("NL2SQL_LLM_RATE_LIMIT_RPM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        let distributed = std::env::var("NL2SQL_DISTRIBUTED_RATE_LIMIT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self::new(rpm, distributed)
    }
}

/// DB-backed bucket: lazy refill + atomic decrement in a short transaction.
/// Returns `Ok(true)` when a token was consumed, `Ok(false)` when the
/// cluster-wide budget is exhausted.
async fn try_acquire_db(db: &SqlitePool, tenant_id: &str, rpm: usize) -> Result<bool, sqlx::Error> {
    let capacity = rpm as f64;
    let rate_per_sec = capacity / 60.0;
    let mut tx = db.begin().await?;

    // Upsert-on-miss: a new tenant gets a full bucket. The composite PK
    // `(tenant_id, bucket)` ensures uniqueness.
    sqlx::query(
        "INSERT OR IGNORE INTO nl2sql_rate_limit_buckets \
         (tenant_id, bucket, tokens, capacity, rate_per_sec, last_refill_at) \
         VALUES (?, 'llm', ?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(tenant_id)
    .bind(capacity)
    .bind(capacity)
    .bind(rate_per_sec)
    .execute(&mut *tx)
    .await?;

    // Lock the row for the read-modify-write below.
    let row = sqlx::query(
        "SELECT tokens, capacity, rate_per_sec, \
                CAST(unixepoch(last_refill_at) AS REAL) AS last_secs \
         FROM nl2sql_rate_limit_buckets \
         WHERE tenant_id = ? AND bucket = 'llm'",
    )
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await?;

    let mut tokens: f64 = row.get("tokens");
    let cap: f64 = row.get("capacity");
    let rate: f64 = row.get("rate_per_sec");
    let last_secs: f64 = row.try_get::<f64, _>("last_secs").unwrap_or(0.0);

    // Compute "now" via the DB clock so we don't drift across replicas.
    let now_row = sqlx::query("SELECT CAST(unixepoch(CURRENT_TIMESTAMP) AS REAL) AS now_secs")
        .fetch_one(&mut *tx)
        .await?;
    let now_secs: f64 = now_row.try_get::<f64, _>("now_secs").unwrap_or(last_secs);
    let elapsed = (now_secs - last_secs).max(0.0);
    tokens = (tokens + elapsed * rate).min(cap);

    let allowed = tokens >= 1.0;
    if allowed {
        tokens -= 1.0;
    }

    sqlx::query(
        "UPDATE nl2sql_rate_limit_buckets \
         SET tokens = ?, last_refill_at = CURRENT_TIMESTAMP \
         WHERE tenant_id = ? AND bucket = 'llm'",
    )
    .bind(tokens)
    .bind(tenant_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(allowed)
}

/// Wrapper that applies tenant-level rate limiting to an async operation.
pub async fn with_rate_limit<F, T>(
    limiter: &Arc<TenantRateLimiter>,
    db: &SqlitePool,
    tenant_id: &str,
    _user_id: &str,
    f: F,
) -> Result<T, crate::error::AppError>
where
    F: std::future::Future<Output = Result<T, crate::error::AppError>>,
{
    if !limiter.try_acquire(db, tenant_id).await {
        tracing::warn!(tenant_id = %tenant_id, "rate limit exceeded for NL2SQL operation");
        return Err(crate::error::AppError::TooManyRequests(format!(
            "NL2SQL rate limit exceeded ({}/min). Retry after {}s.",
            limiter.limit(),
            limiter.retry_after_secs(tenant_id)
        )));
    }
    f.await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_bucket_refunds_on_distributed_rejection() {
        let limiter = TenantRateLimiter::new(1, false);
        assert!(limiter.try_acquire_local("t1"));
        // Bucket is now empty.
        assert!(!limiter.try_acquire_local("t1"));
        // Refund and try again.
        limiter.refund_local("t1");
        assert!(limiter.try_acquire_local("t1"));
    }

    #[test]
    fn limit_returns_configured_rpm() {
        let limiter = TenantRateLimiter::new(120, false);
        assert_eq!(limiter.limit(), 120);
    }

    #[test]
    fn unknown_tenant_retry_after_zero() {
        let limiter = TenantRateLimiter::new(60, false);
        assert_eq!(limiter.retry_after_secs("nope"), 0);
    }
}
