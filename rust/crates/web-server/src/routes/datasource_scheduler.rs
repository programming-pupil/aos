//! Periodic background schema refresh for data sources.
//!
//! Runs every `SCHEMA_REFRESH_INTERVAL_SECS` (default: 1 hour, configurable via
//! the `SCHEMA_REFRESH_INTERVAL_SECS` env var).
//!
//! Logic per data source:
//!   1. Connect to the data source and introspect current schema.
//!   2. Compare with the stored `schema_info` in the platform `data_sources` table.
//!   3. If unchanged → skip.
//!   4. If changed → update `schema_info` in the platform SQLite database.
//!   5. If changed AND the tenant has an embedding model configured → trigger
//!      semantic re-indexing (embeddings + AI descriptions) via `SchemaDescriber`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::{Row, SqlitePool};
use tokio::time::{interval, MissedTickBehavior};

use super::data_sources::decrypt_config;
use crate::nl2sql::refresh_lock::RefreshLock;
use crate::nl2sql::schema_describer::{NoopProgress, SchemaDescriber};
use crate::nl2sql::schema_diff;
use crate::nl2sql::schema_monitor;
use crate::state::AppState;

const DEFAULT_INTERVAL_SECS: u64 = 3600; // 1 hour

/// Spawn the periodic schema-refresh task.
///
/// Returns a [`tokio::sync::watch::Sender`] and a [`tokio::task::JoinHandle`];
/// call `.send(true)` on the sender at shutdown and `.await` the handle to
/// ensure any in-flight refresh cycle completes (or aborts at the next
/// awaitable point) instead of leaking connections / half-written state.
pub fn start_periodic_schema_refresh(
    state: AppState,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let interval_secs: u64 = std::env::var("SCHEMA_REFRESH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS);

    tracing::info!(
        "periodic schema refresh: interval={interval_secs}s, enabled={}",
        interval_secs > 0
    );

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let handle = tokio::spawn(async move {
        if interval_secs == 0 {
            tracing::warn!("SCHEMA_REFRESH_INTERVAL_SECS=0, periodic schema refresh is disabled");
            // Still honour shutdown signal; otherwise the handle never
            // resolves and graceful shutdown blocks forever.
            let _ = shutdown_rx.changed().await;
            return;
        }

        let mut ticker = interval(Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await; // skip the immediate first tick

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    run_refresh_cycle(&state).await;
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("periodic schema refresh: shutdown signal received");
                        break;
                    }
                }
            }
        }
    });

    (shutdown_tx, handle)
}

async fn run_refresh_cycle(state: &AppState) {
    tracing::info!("starting periodic schema refresh cycle");

    // Reap tasks that have been stuck in running/pending for over 30 minutes.
    // This handles the case where a worker goroutine panicked or was cancelled
    // without updating the task row, leaving it to block future refreshes.
    let _ = sqlx::query(
        "UPDATE nl2sql_refresh_tasks \
         SET status = 'failed', \
             error_message = 'task timed out (no progress for 30 minutes)' \
         WHERE status IN ('running', 'pending') \
           AND updated_at < datetime(CURRENT_TIMESTAMP, '-30 minutes')",
    )
    .execute(&state.db)
    .await
    .map(|r| {
        if r.rows_affected() > 0 {
            tracing::warn!(count = r.rows_affected(), "reaped stuck refresh tasks");
        }
    });

    // Include both tenant-level (user_id IS NULL) and personal (user_id IS NOT NULL) data sources.
    let rows: Vec<(String, String, Option<String>, String, Value)> = sqlx::query_as(
        r#"
        SELECT tenant_id, id, user_id, db_type, CAST(config AS TEXT) as config
        FROM data_sources
        WHERE db_type IN ('mysql', 'tidb', 'postgres', 'clickhouse', 'presto', 'trino', 'mongodb')
          AND enabled = 1
        "#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_else(|e| {
        tracing::error!("failed to fetch data sources for refresh: {e}");
        Vec::new()
    });

    if rows.is_empty() {
        tracing::debug!("no supported data sources to refresh");
        cleanup_query_understanding_cache(state).await;
        return;
    }

    tracing::info!("found {} data sources to check", rows.len());

    // Group by (tenant_id, effective_user):
    // - tenant-level datasource: effective_user = tenant_id
    // - personal datasource:     effective_user = user_id
    //
    // NOTE: API keys are tenant-scoped in `api_keys`, so config resolution must
    // always use the real tenant_id (not effective_user).
    let mut by_user: std::collections::HashMap<(String, String), Vec<_>> =
        std::collections::HashMap::new();
    for row in &rows {
        let tenant_id = row.0.clone();
        let effective_user = row.2.as_ref().unwrap_or(&row.0).clone();
        by_user
            .entry((tenant_id, effective_user))
            .or_default()
            .push(row);
    }

    let total_sources = rows.len();
    let mut processed_count = 0usize;
    let mut changed_count = 0usize;
    let mut error_count = 0usize;

    for ((group_tenant_id, effective_user), sources) in by_user {
        let embed_cfg =
            crate::nl2sql::resolve_embedding_config(&state.db, &group_tenant_id, None).await;
        let embed_store = state.nl2sql_embedding_store.clone();

        let chat_cfg = match state.config_registry.as_ref() {
            Some(registry) => {
                match crate::nl2sql::resolve_chat_config_db_only(
                    registry,
                    &group_tenant_id,
                    &effective_user,
                    &state.default_model,
                    None,
                )
                .await
                {
                    Ok(cfg) => Some(cfg),
                    Err(e) => {
                        tracing::warn!(
                            tenant_id = %group_tenant_id,
                            user_id = %effective_user,
                            error = %e,
                            "scheduler: failed to resolve DB chat config, skipping semantic re-indexing"
                        );
                        None
                    }
                }
            }
            None => None,
        };

        for ds in sources {
            let (ds_tenant_id, ds_id, user_id, db_type, config_json) = ds;
            let effective_user_id = user_id.as_deref().unwrap_or(&ds_tenant_id);
            let result = refresh_single(
                &state.db,
                &state.data_dir,
                &ds_tenant_id,
                effective_user_id,
                &ds_id,
                &db_type,
                &config_json,
                embed_store.clone(),
                embed_cfg.clone(),
                chat_cfg.clone(),
            )
            .await;
            processed_count += 1;
            match result {
                Ok(true) => {
                    changed_count += 1;
                    tracing::info!(
                        ds_id = %ds_id,
                        "schema changed for data source"
                    );
                }
                Ok(false) => {
                    tracing::trace!(ds_id = %ds_id, "schema unchanged");
                }
                Err(e) => {
                    error_count += 1;
                    tracing::warn!(ds_id = %ds_id, error = %e, "failed to refresh data source");
                }
            }
        }
    }

    tracing::info!(
        total = total_sources,
        processed = processed_count,
        changed = changed_count,
        errors = error_count,
        "periodic schema refresh cycle complete"
    );

    // Clean up expired entries from the NL2SQL query understanding cache.
    cleanup_query_understanding_cache(state).await;

    // Process any pending auto_reindex tasks created by schema change notifications.
    schema_monitor::process_auto_reindex_tasks(state).await;
}

/// Refreshes a single data source. Returns `Ok(true)` if schema changed, `Ok(false)` if unchanged.
async fn refresh_single(
    db: &SqlitePool,
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
    ds_id: &str,
    db_type: &str,
    config_json: &Value,
    embed_store: Option<Arc<crate::nl2sql::embedding::EmbeddingStoreRegistry>>,
    embed_cfg: Option<crate::nl2sql::EmbeddingTenantConfig>,
    chat_cfg: Option<crate::nl2sql::ChatTenantConfig>,
) -> Result<bool, String> {
    // Acquire the per-datasource advisory lock up front. If someone else
    // is already refreshing this datasource (user-triggered async task,
    // another scheduler tick that's still running), skip silently and let
    // the next tick pick it up.
    let _lock = match RefreshLock::try_acquire(db, ds_id)
        .await
        .map_err(|e| format!("lock error: {e}"))?
    {
        Some(guard) => guard,
        None => {
            tracing::debug!(ds_id, "another refresh is in flight, skipping this tick");
            return Ok(false);
        }
    };

    // Decrypt config to get connection details.
    let config_val =
        decrypt_config(config_json, data_dir).map_err(|e| format!("decrypt config failed: {e}"))?;

    let _trino_permit = if matches!(db_type, "presto" | "trino") {
        Some(
            crate::routes::nl2sql::agent_executor::acquire_trino_user_permit(tenant_id, user_id)
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let live_schema = fetch_current_schema(db_type, &config_val).await?;

    // Compare with stored schema_info.
    let stored: Option<String> = sqlx::query(
        "SELECT CAST(schema_info AS TEXT) as schema_info FROM data_sources WHERE id = ?",
    )
    .bind(ds_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("DB error: {e}"))?
    .map(|r| r.get::<Option<Option<String>>, _>("schema_info"))
    .flatten()
    .flatten();

    let stored_json: serde_json::Value = stored
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);

    // Manual tables live only in the stored `schema_info` (they don't
    // exist in the source database). We rescue them before doing the diff
    // or writing the new schema, or the scheduler would classify them as
    // "removed" and purge their semantics. The schema_diff pass already
    // ignores `is_manual == true` entries; this merge is what keeps them
    // durable in the `schema_info` JSON across refreshes.
    let manual_tables = schema_diff::extract_manual_tables(&stored_json);
    let mut merged_tables = live_schema.tables.clone();
    merged_tables.extend(manual_tables);

    // Build the enriched schema object: {"tables": [...], "foreign_keys": [...]}
    let schema_obj = serde_json::json!({
        "tables": merged_tables,
        "foreign_keys": live_schema.foreign_keys,
    });
    let current_json = &schema_obj;

    // Structural diff: ignore ordering/casing noise and only look at real
    // business differences. Manual tables are filtered out by the diff
    // itself so their absence from `live_schema` is harmless.
    let diff = schema_diff::diff_schemas(&stored_json, &current_json);
    if diff.is_empty() {
        // Schema unchanged — but check if embeddings are missing (first-time indexing).
        if let (Some(store), Some(cfg), Some(chat)) = (embed_store, embed_cfg, chat_cfg) {
            let has_any_vectors = match crate::nl2sql::embedding_profiles::resolve_profiles(
                db,
                tenant_id,
                Some("nl2sql"),
            )
            .await
            {
                Ok(profiles) => store
                    .profile_store(
                        tenant_id,
                        &profiles.local.id,
                        &profiles.local.config.model,
                        profiles.local.config.base_url.clone(),
                    )
                    .and_then(|profile_store| profile_store.indexed_keys(ds_id))
                    .map(|keys| !keys.is_empty())
                    .unwrap_or(false),
                Err(_) => false,
            };
            if !has_any_vectors {
                tracing::info!(
                    ds_id,
                    "schema unchanged but no vectors found, triggering initial indexing"
                );
                let describer = SchemaDescriber::new(db.clone(), store, Some(cfg), Some(chat));
                let task_id = uuid::Uuid::new_v4().to_string();
                let _ = sqlx::query(
                    "INSERT INTO nl2sql_refresh_tasks \
                     (task_id, tenant_id, trigger_source, datasource_id, status, total_tables) \
                     VALUES (?, ?, 'scheduler', ?, 'running', ?)",
                )
                .bind(&task_id)
                .bind(tenant_id)
                .bind(ds_id)
                .bind(merged_tables.len() as i32)
                .execute(db)
                .await;

                match describer.refresh_datasource(tenant_id, ds_id).await {
                    Ok(r) => {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'completed', progress = 100, \
                             processed_tables = ?, completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                        )
                        .bind(r.tables_processed as i32)
                        .bind(&task_id)
                        .execute(db)
                        .await;
                        tracing::info!(
                            ds_id,
                            tables = r.tables_processed,
                            "initial indexing complete"
                        );
                    }
                    Err(e) => {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                             error_message = ?, completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                        )
                        .bind(e.to_string())
                        .bind(&task_id)
                        .execute(db)
                        .await;
                        tracing::warn!(ds_id, error = %e, "initial indexing failed");
                    }
                }
            }
        }
        return Ok(false);
    }

    // Schema changed — update in database.
    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(&current_json)
        .bind(ds_id)
        .execute(db)
        .await
        .map_err(|e| format!("failed to update schema_info: {e}"))?;

    tracing::info!(
        ds_id,
        tables = merged_tables.len(),
        changed = diff.changed.len(),
        removed = diff.removed.len(),
        "schema changed, updated schema_info"
    );

    // Purge semantic rows for tables that no longer exist so stale vectors
    // don't poison future NL2SQL routing. We leave the SQLite embedding
    // store alone — its rows are scoped by (ds, table) and simply become
    // orphan data; cheap to garbage-collect later. Errors are logged but
    // not propagated — they don't block the rest of the refresh cycle,
    // but must be visible so operators can act on them.
    for removed in &diff.removed {
        if let Err(e) = sqlx::query(
            "DELETE FROM nl2sql_table_semantics WHERE datasource_id = ? AND table_name = ? AND deleted_at IS NULL",
        )
        .bind(ds_id)
        .bind(removed)
        .execute(db)
        .await
        {
            tracing::warn!(ds_id, table = %removed, error = %e, "failed to purge nl2sql_table_semantics for removed table");
        }
        if let Err(e) = sqlx::query(
            "DELETE FROM nl2sql_table_desc_semantics WHERE datasource_id = ? AND table_name = ? AND deleted_at IS NULL",
        )
        .bind(ds_id)
        .bind(removed)
        .execute(db)
        .await
        {
            tracing::warn!(ds_id, table = %removed, error = %e, "failed to purge nl2sql_table_desc_semantics for removed table");
        }
    }

    // Trigger semantic re-indexing only for tables that actually changed.
    if let (Some(store), Some(cfg), Some(chat)) = (embed_store, embed_cfg, chat_cfg) {
        let describer = SchemaDescriber::new(db.clone(), store, Some(cfg.clone()), Some(chat));

        let changed = diff.changed.clone();
        if changed.is_empty() {
            tracing::debug!(ds_id, "only removals — skipping semantic re-indexing");
            return Ok(true);
        }

        // Record a task row so operators/UI can see scheduler-driven
        // refreshes in the same history as user-initiated ones.
        let task_id = uuid::Uuid::new_v4().to_string();
        let total_tables = i32::try_from(changed.len()).unwrap_or(i32::MAX);
        if let Err(e) = sqlx::query(
            "INSERT INTO nl2sql_refresh_tasks \
             (task_id, tenant_id, trigger_source, datasource_id, status, total_tables) \
             VALUES (?, ?, 'scheduler', ?, 'running', ?)",
        )
        .bind(&task_id)
        .bind(tenant_id)
        .bind(ds_id)
        .bind(total_tables)
        .execute(db)
        .await
        {
            tracing::warn!(ds_id, error = %e, "failed to insert scheduler refresh task row");
        }

        match describer
            .refresh_tables(tenant_id, ds_id, &changed, NoopProgress)
            .await
        {
            Ok(result) => {
                let failed_json = if result.failed_tables.is_empty() {
                    None
                } else {
                    serde_json::to_value(
                        result
                            .failed_tables
                            .iter()
                            .map(|(name, err)| serde_json::json!({ "table": name, "error": err }))
                            .collect::<Vec<_>>(),
                    )
                    .ok()
                };
                let all_failed = result.tables_processed == 0 && !result.failed_tables.is_empty();
                let status = if all_failed { "failed" } else { "completed" };
                if let Err(e) = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = ?, progress = 100, \
                         processed_tables = ?, failed_tables = ?, completed_at = CURRENT_TIMESTAMP \
                     WHERE task_id = ?",
                )
                .bind(status)
                .bind(i32::try_from(result.tables_processed).unwrap_or(i32::MAX))
                .bind(failed_json)
                .bind(&task_id)
                .execute(db)
                .await
                {
                    tracing::warn!(ds_id, error = %e, "failed to finalise scheduler refresh task row");
                }
                tracing::info!(
                    ds_id,
                    tables = result.tables_processed,
                    columns = result.columns_processed,
                    "partial semantic re-indexing complete"
                );
            }
            Err(e) => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = ?, completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                )
                .bind(e.to_string())
                .bind(&task_id)
                .execute(db)
                .await;
                tracing::warn!(ds_id, error = %e, "partial semantic re-indexing failed, skipping");
            }
        }
    }

    Ok(true)
}

/// Outcome of fetching the current schema from a live data source.
struct LiveSchema {
    /// Table definitions in the same format as before.
    tables: Vec<serde_json::Value>,
    /// Foreign key relationships discovered from the source DB.
    foreign_keys: Vec<serde_json::Value>,
}

/// Fetches the current schema from the live data source.
///
/// Delegates to the shared [`crate::nl2sql::schema_discovery`] module so
/// the scheduler and the interactive `POST /data-sources/:id/discover`
/// endpoint always agree on the output shape. Historically these two
/// paths drifted (scheduler had `LIMIT 1000`, no retries; interactive
/// path used the `clickhouse` crate vs reqwest) which caused
/// `schema_diff` to fire false positives on every scheduler tick.
///
/// Returns both `tables` and `foreign_keys` so the scheduler can
/// persist them in the enriched `{"tables": [...], "foreign_keys": [...]}` format.
async fn fetch_current_schema(
    db_type: &str,
    config: &serde_json::Value,
) -> Result<LiveSchema, String> {
    use crate::nl2sql::schema_discovery as sd;

    if db_type == "mongodb" {
        let mongo_config: nl2sql_core::datasource_config::MongoConfig =
            serde_json::from_value(config.clone())
                .map_err(|error| format!("invalid MongoDB config: {error}"))?;
        let outcome = sd::discover_mongodb(&mongo_config).await?;
        return Ok(LiveSchema {
            tables: outcome.tables,
            foreign_keys: Vec::new(),
        });
    }

    let host = config
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or("missing host")?;
    let port_default: i64 = match db_type {
        "postgres" => 5432,
        "clickhouse" => 8123,
        "presto" | "trino" => 8080,
        _ => 3306,
    };
    let port = u16::try_from(
        config
            .get("port")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(port_default),
    )
    .map_err(|_| "port out of range".to_owned())?;
    let username = config
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or("missing username")?;
    let password = config
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match db_type {
        "mysql" | "tidb" => {
            let database = config
                .get("database")
                .and_then(|v| v.as_str())
                .ok_or("missing database")?;
            let outcome = sd::discover_mysql(host, port, database, username, password).await?;
            Ok(LiveSchema {
                tables: outcome.tables,
                foreign_keys: outcome
                    .foreign_keys
                    .into_iter()
                    .map(|fk| {
                        serde_json::json!({
                            "source_table": fk.source_table,
                            "source_column": fk.source_column,
                            "target_table": fk.target_table,
                            "target_column": fk.target_column,
                        })
                    })
                    .collect(),
            })
        }
        "postgres" => {
            let database = config
                .get("database")
                .and_then(|v| v.as_str())
                .ok_or("missing database")?;
            let outcome = sd::discover_postgres(host, port, database, username, password).await?;
            Ok(LiveSchema {
                tables: outcome.tables,
                foreign_keys: outcome
                    .foreign_keys
                    .into_iter()
                    .map(|fk| {
                        serde_json::json!({
                            "source_table": fk.source_table,
                            "source_column": fk.source_column,
                            "target_table": fk.target_table,
                            "target_column": fk.target_column,
                        })
                    })
                    .collect(),
            })
        }
        "clickhouse" => {
            let database = config
                .get("database")
                .and_then(|v| v.as_str())
                .ok_or("missing database")?;
            let outcome = sd::discover_clickhouse(host, port, database, username, password).await?;
            Ok(LiveSchema {
                tables: outcome.tables,
                foreign_keys: outcome
                    .foreign_keys
                    .into_iter()
                    .map(|fk| {
                        serde_json::json!({
                            "source_table": fk.source_table,
                            "source_column": fk.source_column,
                            "target_table": fk.target_table,
                            "target_column": fk.target_column,
                        })
                    })
                    .collect(),
            })
        }
        "presto" | "trino" => {
            let catalog = config
                .get("catalog")
                .and_then(|v| v.as_str())
                .ok_or("missing catalog")?;
            let schema = config
                .get("schema")
                .and_then(|v| v.as_str())
                .ok_or("missing schema")?;
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
            let outcome = sd::discover_trino(
                host,
                port,
                catalog,
                schema,
                username,
                Some(password),
                secure,
                basic_auth,
            )
            .await?;
            Ok(LiveSchema {
                tables: outcome.tables,
                foreign_keys: outcome
                    .foreign_keys
                    .into_iter()
                    .map(|fk| {
                        serde_json::json!({
                            "source_table": fk.source_table,
                            "source_column": fk.source_column,
                            "target_table": fk.target_table,
                            "target_column": fk.target_column,
                        })
                    })
                    .collect(),
            })
        }
        _ => Err(format!("unsupported db_type: {db_type}")),
    }
}

/// Delete expired entries from the NL2SQL query understanding cache.
///
/// Runs as part of the periodic schema refresh cycle. Rows whose TTL has elapsed
/// are deleted so stale entries do not serve outdated routing/understanding
/// results.
const QUERY_UNDERSTANDING_CACHE_CLEANUP_SQL: &str = "DELETE FROM nl2sql_query_understanding_cache
     WHERE rowid IN (
       SELECT rowid FROM nl2sql_query_understanding_cache
       WHERE resolved_at IS NOT NULL
         AND resolved_at < datetime(CURRENT_TIMESTAMP, printf('-%d hours', cache_ttl_hours))
       LIMIT 10000
     )";

pub async fn cleanup_query_understanding_cache(state: &AppState) {
    let result = sqlx::query(QUERY_UNDERSTANDING_CACHE_CLEANUP_SQL)
        .execute(&state.db)
        .await;

    match result {
        Ok(result) => {
            let deleted = result.rows_affected();
            if deleted > 0 {
                tracing::debug!("cleaned up {} expired query cache entries", deleted);
            }
        }
        Err(e) => {
            tracing::warn!("failed to clean up query cache: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QUERY_UNDERSTANDING_CACHE_CLEANUP_SQL;

    #[tokio::test]
    async fn sqlite_query_understanding_cache_cleanup_respects_row_ttl() {
        let db = crate::test_sqlite_pool().await;
        sqlx::query(
            "INSERT INTO tenants (id, name, slug) VALUES ('tenant-cache', 'Cache', 'cache')",
        )
        .execute(&db)
        .await
        .expect("insert cache test tenant");
        sqlx::query(
            "INSERT INTO data_sources (id, tenant_id, name, db_type, config)
             VALUES ('datasource-cache', 'tenant-cache', 'Cache', 'mysql', '{}')",
        )
        .execute(&db)
        .await
        .expect("insert cache test datasource");
        sqlx::query(
            "INSERT INTO nl2sql_query_understanding_cache
               (tenant_id, datasource_id, question_hash, intent, resolved_at, cache_ttl_hours)
             VALUES
               ('tenant-cache', 'datasource-cache', 'expired', 'query',
                datetime(CURRENT_TIMESTAMP, '-25 hours'), 24),
               ('tenant-cache', 'datasource-cache', 'active', 'query',
                datetime(CURRENT_TIMESTAMP, '-23 hours'), 24)",
        )
        .execute(&db)
        .await
        .expect("insert cache test entries");

        let deleted = sqlx::query(QUERY_UNDERSTANDING_CACHE_CLEANUP_SQL)
            .execute(&db)
            .await
            .expect("clean up expired query understanding cache entries")
            .rows_affected();
        let hashes: Vec<String> = sqlx::query_scalar(
            "SELECT question_hash FROM nl2sql_query_understanding_cache ORDER BY question_hash",
        )
        .fetch_all(&db)
        .await
        .expect("load remaining cache entries");

        assert_eq!(deleted, 1);
        assert_eq!(hashes, vec!["active".to_string()]);
        db.close().await;
    }
}
