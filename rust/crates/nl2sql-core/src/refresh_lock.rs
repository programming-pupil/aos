//! Per-datasource advisory lock for semantic refresh.
//!
//! A semantic refresh of a single datasource must not overlap with another
//! refresh of the same datasource — otherwise two concurrent runs race to
//! upsert into `nl2sql_*_semantics`, producing duplicated LLM calls and an
//! indeterminate final state. This happens in practice when:
//!
//!   * the periodic scheduler kicks in while a user clicks "refresh index",
//!   * two admin users both trigger the async refresh endpoint,
//!   * the same cron tick overlaps with the previous one (slow LLM).
//!
//! AOS is single-process and single-instance, so a process-local keyed lock is
//! sufficient and avoids holding a SQLite writer connection during LLM work.

use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

fn held_locks() -> &'static Mutex<HashSet<String>> {
    static LOCKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard for one datasource refresh within this AOS process.
pub struct RefreshLock {
    name: String,
}

impl RefreshLock {
    /// Non-blocking: returns `Ok(None)` when another worker already holds
    /// the lock, `Ok(Some(guard))` when we acquired it. Callers should
    /// skip the refresh and move on when they see `None`.
    pub async fn try_acquire(
        _pool: &SqlitePool,
        datasource_id: &str,
    ) -> anyhow::Result<Option<Self>> {
        let name = lock_name(datasource_id);
        let mut locks = held_locks()
            .lock()
            .map_err(|_| anyhow::anyhow!("refresh lock registry is poisoned"))?;
        if locks.insert(name.clone()) {
            Ok(Some(Self { name }))
        } else {
            Ok(None)
        }
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        if let Ok(mut locks) = held_locks().lock() {
            locks.remove(&self.name);
        }
    }
}

fn lock_name(datasource_id: &str) -> String {
    format!("nl2sql_refresh:{datasource_id}")
}

#[cfg(test)]
mod tests {
    use super::RefreshLock;
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn lock_is_keyed_and_released_on_drop() {
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .expect("open sqlite");
        let first = RefreshLock::try_acquire(&pool, "one")
            .await
            .expect("acquire")
            .expect("first lock");
        assert!(RefreshLock::try_acquire(&pool, "one")
            .await
            .expect("contended")
            .is_none());
        assert!(RefreshLock::try_acquire(&pool, "two")
            .await
            .expect("other key")
            .is_some());
        drop(first);
        assert!(RefreshLock::try_acquire(&pool, "one")
            .await
            .expect("reacquire")
            .is_some());
    }
}
