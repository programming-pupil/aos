//! Web Server — HTTP/WebSocket API layer for the enterprise `WebUI`.

mod auth;
mod auth_middleware;
mod config;
mod email;
mod error;
#[cfg(feature = "nl2sql")]
pub mod nl2sql;
mod routes;
mod semantic_kernel_store;
mod state;
mod telemetry;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::Body,
    http::{HeaderValue, Request, StatusCode, Uri},
    middleware::Next,
    response::Response,
    Router,
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

fn web_ui_service(web_dir: PathBuf) -> ServeDir<ServeFile> {
    let index_file = web_dir.join("index.html");
    if !index_file.is_file() {
        panic!("Web UI index file does not exist: {}", index_file.display());
    }
    ServeDir::new(web_dir).fallback(ServeFile::new(index_file))
}

pub(crate) fn sqlite_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(feature = "nl2sql")]
pub fn warm_local_embedding_model(cache_dir: PathBuf) -> anyhow::Result<()> {
    crate::nl2sql::embedding::configure_local_embedding_cache_dir(cache_dir)?;
    crate::nl2sql::embedding::warm_local_embedding_model()
}

#[cfg(feature = "nl2sql")]
pub fn shutdown_local_embedding_model() {
    crate::nl2sql::embedding::shutdown_local_embedding_model();
}

/// Acquire SQLite's single-writer lock before a transaction reads state that it
/// will later update. This avoids deferred-transaction read-to-write upgrade
/// failures under concurrent requests; `busy_timeout` can then serialize the
/// short write transactions normally.
pub(crate) async fn acquire_sqlite_write_lock(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> std::result::Result<(), sqlx::Error> {
    sqlx::query::<sqlx::Sqlite>("UPDATE aos_setup_lock SET lock_id = lock_id WHERE lock_id = 1")
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) async fn test_sqlite_pool() -> sqlx::SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open SQLite test database");
    sqlx::migrate!("./sqlite-migrations")
        .run(&pool)
        .await
        .expect("apply SQLite test migrations");
    pool
}

#[cfg(test)]
pub(crate) async fn test_sqlite_file_pool() -> (sqlx::SqlitePool, PathBuf) {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    let database_path =
        std::env::temp_dir().join(format!("aos-sqlite-test-{}.db", uuid::Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&database_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("open file-backed SQLite test database");
    sqlx::migrate!("./sqlite-migrations")
        .run(&pool)
        .await
        .expect("apply SQLite test migrations");
    (pool, database_path)
}

#[cfg(test)]
mod sqlite_baseline_tests {
    use sha2::Digest;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::borrow::Cow;

    #[test]
    fn historical_semantic_kernel_migration_checksum_is_stable() {
        let checksum = hex::encode(sha2::Sha384::digest(include_bytes!(
            "../sqlite-migrations/0017_semantic_kernel_core.sql"
        )));
        assert_eq!(
            checksum,
            "58772fbbda2f10a4d1fb421caaf7eb3f55f20e06edb7c8cbcf9807992518676bd5f9e9a0db0ffe13889d079ef76e280f"
        );
    }

    async fn migrate_through(pool: &sqlx::SqlitePool, max_version: i64) {
        let full = sqlx::migrate!("./sqlite-migrations");
        let partial = sqlx::migrate::Migrator {
            migrations: Cow::Owned(
                full.iter()
                    .filter(|migration| migration.version <= max_version)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: true,
            no_tx: false,
        };
        partial
            .run(pool)
            .await
            .unwrap_or_else(|error| panic!("migrate through {max_version}: {error}"));
    }

    #[tokio::test]
    async fn baseline_migration_is_idempotent_and_seeds_setup_lock_once() {
        let pool = crate::test_sqlite_pool().await;
        sqlx::migrate!("./sqlite-migrations")
            .run(&pool)
            .await
            .expect("reapply SQLite migrations");

        let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .expect("count migration ledger");
        let setup_lock_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aos_setup_lock WHERE lock_id = 1")
                .fetch_one(&pool)
                .await
                .expect("count setup lock seed");
        let repository_auto_sync_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('rd_repository_settings') \
             WHERE name IN ('auto_sync_enabled','auto_sync_interval_minutes','last_auto_sync_at','last_sync_error')",
        )
        .fetch_one(&pool)
        .await
        .expect("count repository auto-sync columns");
        let agent_market_source_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('rd_agent_profiles') \
             WHERE name IN ('source','source_item_id')",
        )
        .fetch_one(&pool)
        .await
        .expect("count agent market source columns");
        let model_profile_table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'model_capability_profiles'",
        )
        .fetch_one(&pool)
        .await
        .expect("count model capability profile table");
        let api_key_profile_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('api_keys')
             WHERE name = 'model_profile_id'",
        )
        .fetch_one(&pool)
        .await
        .expect("count API key model profile column");
        let rd_spec_repository_ids_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('rd_specs')
             WHERE name = 'repository_ids_json'",
        )
        .fetch_one(&pool)
        .await
        .expect("count Plan Mode repository selection column");

        assert!(migration_count >= 17);
        assert_eq!(setup_lock_count, 1);
        assert_eq!(repository_auto_sync_column_count, 4);
        assert_eq!(agent_market_source_column_count, 2);
        assert_eq!(model_profile_table_count, 1);
        assert_eq!(api_key_profile_column_count, 1);
        assert_eq!(rd_spec_repository_ids_column_count, 1);
        pool.close().await;
    }

    #[tokio::test]
    async fn semantic_contract_scope_migration_maps_only_unambiguous_legacy_rows() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open legacy contract fixture");
        sqlx::raw_sql(
            "CREATE TABLE data_sources (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL);
             CREATE TABLE nl2sql_metrics (id INTEGER PRIMARY KEY);
             CREATE TABLE nl2sql_join_paths (id INTEGER PRIMARY KEY);
             CREATE TABLE metric_contracts (
               id TEXT NOT NULL, tenant_id TEXT NOT NULL, version INTEGER NOT NULL,
               status TEXT NOT NULL, contract_json TEXT NOT NULL, valid_from TEXT NOT NULL,
               valid_until TEXT, PRIMARY KEY(tenant_id, id, version));
             CREATE TABLE join_contracts (
               id TEXT NOT NULL, tenant_id TEXT NOT NULL, version INTEGER NOT NULL,
               status TEXT NOT NULL, contract_json TEXT NOT NULL,
               PRIMARY KEY(tenant_id, id, version));
             INSERT INTO data_sources VALUES
               ('single-ds', 'single-tenant'),
               ('multi-a', 'multi-tenant'),
               ('multi-b', 'multi-tenant');
             INSERT INTO metric_contracts VALUES
               ('orders', 'single-tenant', 1, 'published', '{}', '2026-01-01', NULL),
               ('roi', 'multi-tenant', 2, 'published', '{}', '2026-01-01', NULL);
             INSERT INTO join_contracts VALUES
               ('orders-users', 'single-tenant', 1, 'published', '{}'),
               ('revenue-cost', 'multi-tenant', 3, 'published', '{}');",
        )
        .execute(&pool)
        .await
        .expect("seed legacy contract fixture");

        sqlx::raw_sql(include_str!(
            "../sqlite-migrations/0030_semantic_contract_production_scope.sql"
        ))
        .execute(&pool)
        .await
        .expect("upgrade legacy semantic contracts");

        let single_metric: (String, String) = sqlx::query_as(
            "SELECT datasource_id, status FROM metric_contracts
             WHERE tenant_id = 'single-tenant' AND id = 'orders'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(single_metric, ("single-ds".into(), "published".into()));

        let ambiguous_metric: (String, String, String) = sqlx::query_as(
            "SELECT datasource_id, status, lineage_json FROM metric_contracts
             WHERE tenant_id = 'multi-tenant' AND id = 'roi'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ambiguous_metric.0, "__legacy_unscoped__");
        assert_eq!(ambiguous_metric.1, "legacy_unscoped");
        assert!(ambiguous_metric.2.contains("blocked_ambiguous_datasource"));

        let join_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM join_contracts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(join_rows, 2);
    }

    #[tokio::test]
    async fn n_minus_one_and_two_snapshots_upgrade_without_semantic_data_loss() {
        for snapshot_version in [31_i64, 32_i64] {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .expect("open upgrade fixture");
            migrate_through(&pool, snapshot_version).await;

            sqlx::query(
                "INSERT INTO metric_contracts
                    (id, tenant_id, datasource_id, source_metric_id, version, status,
                     contract_json, lineage_json, valid_from, valid_until)
                 VALUES ('metric:legacy', 'tenant-upgrade', 'ds-upgrade', 7, 3, 'active',
                         '{\"id\":\"metric:legacy\"}', '{\"source\":\"upgrade-fixture\"}',
                         '2026-01-01T00:00:00Z', NULL)",
            )
            .execute(&pool)
            .await
            .expect("seed metric contract");
            sqlx::query(
                "INSERT INTO join_contracts
                    (id, tenant_id, datasource_id, source_kind, source_id, version,
                     status, contract_json, lineage_json, valid_from, valid_until)
                 VALUES ('join:legacy', 'tenant-upgrade', 'ds-upgrade', 'join_path', 9, 2,
                         'active', '{\"id\":\"join:legacy\"}',
                         '{\"source\":\"upgrade-fixture\"}',
                         '2026-01-01T00:00:00Z', NULL)",
            )
            .execute(&pool)
            .await
            .expect("seed join contract");
            sqlx::query(
                "INSERT INTO capability_tokens
                    (id, tenant_id, user_id, session_id, tool_name, resource_scope,
                     action_scope, executor_scope, child_scope, expires_at, remaining_uses)
                 VALUES ('legacy-capability', 'tenant-upgrade', 'user-upgrade', 'session-upgrade',
                         'read_file', 'workspace', 'read', 'native', NULL,
                         '2099-01-01T00:00:00Z', 2)",
            )
            .execute(&pool)
            .await
            .expect("seed capability");

            let full = sqlx::migrate!("./sqlite-migrations");
            full.run(&pool).await.expect("upgrade snapshot to current");
            full.run(&pool)
                .await
                .expect("repeated startup must keep the migration ledger stable");

            let metric: (String, String, i64) = sqlx::query_as(
                "SELECT contract_json, lineage_json, version FROM metric_contracts
                 WHERE tenant_id = 'tenant-upgrade' AND datasource_id = 'ds-upgrade'
                   AND id = 'metric:legacy'",
            )
            .fetch_one(&pool)
            .await
            .expect("load upgraded metric contract");
            assert_eq!(
                metric,
                (
                    "{\"id\":\"metric:legacy\"}".into(),
                    "{\"source\":\"upgrade-fixture\"}".into(),
                    3,
                )
            );
            let join: (String, String, i64) = sqlx::query_as(
                "SELECT contract_json, lineage_json, version FROM join_contracts
                 WHERE tenant_id = 'tenant-upgrade' AND datasource_id = 'ds-upgrade'
                   AND id = 'join:legacy'",
            )
            .fetch_one(&pool)
            .await
            .expect("load upgraded join contract");
            assert_eq!(
                join,
                (
                    "{\"id\":\"join:legacy\"}".into(),
                    "{\"source\":\"upgrade-fixture\"}".into(),
                    2,
                )
            );
            let capability: (i64, String, Option<String>, Option<String>) = sqlx::query_as(
                "SELECT remaining_uses, policy_version, parent_token_id, revoked_at
                 FROM capability_tokens WHERE id = 'legacy-capability'",
            )
            .fetch_one(&pool)
            .await
            .expect("load upgraded capability");
            assert_eq!(capability, (2, "capability-policy-v1".into(), None, None));
            let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
                .fetch_one(&pool)
                .await
                .expect("count current migration ledger");
            assert!(
                migration_count >= 39,
                "upgrade fixture must apply the current durable schema migrations"
            );
            pool.close().await;
        }
    }

    #[tokio::test]
    async fn deep_research_budget_migration_only_updates_the_legacy_default() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("legacy-default", 1800_i64),
            ("tenant-customized", 1799_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 4, 12, 3,
                         300, 300, 300, 30, 10, 120, 90, 420)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert budget profile fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0004_deep_research_runtime_budget.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply deep research budget migration statement");

        let migrated: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'legacy-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load migrated default budget");
        assert_eq!(migrated, (540, 90, 120, 240));

        let customized: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'tenant-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load customized budget");
        assert_eq!(customized, (1799, 300, 300, 420));
        pool.close().await;
    }

    #[tokio::test]
    async fn bounded_research_migration_preserves_customized_profiles() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("bounded-default", 540_i64),
            ("bounded-customized", 541_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 4, 12, 3,
                         90, 120, 90, 30, 10, 45, 75, 240)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert bounded budget fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0007_deep_research_bounded_execution.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply bounded research migration statement");

        let migrated: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs
             FROM pm_budget_profiles WHERE tenant_id = 'bounded-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load bounded default");
        assert_eq!(migrated, (480, 3, 8, 110));

        let customized: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs
             FROM pm_budget_profiles WHERE tenant_id = 'bounded-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load bounded custom profile");
        assert_eq!(customized, (541, 4, 12, 90));
        pool.close().await;
    }

    #[tokio::test]
    async fn marginal_evidence_budget_migration_preserves_customized_profiles() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("marginal-default", 480_i64),
            ("marginal-customized", 481_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 3, 8, 3,
                         110, 120, 90, 30, 10, 45, 75, 240)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert marginal evidence budget fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0010_deep_research_marginal_evidence_budget.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply marginal evidence budget migration statement");

        let migrated: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs, source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'marginal-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load marginal default budget");
        assert_eq!(migrated, (390, 2, 6, 90, 100, 150));

        let customized: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs, source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'marginal-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load customized marginal budget");
        assert_eq!(customized, (481, 3, 8, 110, 120, 240));
        pool.close().await;
    }

    #[tokio::test]
    async fn experience_budget_migration_preserves_customized_profiles() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("experience-default", 390_i64),
            ("experience-customized", 391_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 2, 6, 3,
                         90, 100, 75, 30, 10, 45, 60, 150)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert experience budget fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0013_deep_research_experience_budget.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply experience budget migration statement");

        let migrated: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, source_slot_api_fetch_secs,
                    retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'experience-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load experience default budget");
        assert_eq!(migrated, (360, 75, 90, 60, 120));

        let customized: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, source_slot_api_fetch_secs,
                    retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'experience-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load customized experience budget");
        assert_eq!(customized, (391, 90, 100, 75, 150));
        pool.close().await;
    }
}
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub use error::{AppError, Result};
pub use state::AppState;

const DEFAULT_TOKIO_WORKER_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(feature = "bot-agents")]
fn init_bot_gateway_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(not(feature = "bot-agents"))]
fn init_bot_gateway_tls_provider() {}

fn init_tracing() {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "web_server=debug,agent_gateway=debug,agent_gateway::runtime_builder=debug,runtime=debug,tower_http=debug,billing=info".into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .try_init();
}

fn log_startup_phase(started: Instant, phase: &'static str) {
    tracing::info!(
        phase = %phase,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "startup phase completed"
    );
}

fn enabled_feature_list() -> String {
    let mut features = Vec::new();
    if cfg!(feature = "agent") {
        features.push("agent");
    }
    if cfg!(feature = "pm") {
        features.push("pm");
    }
    if cfg!(feature = "nl2sql") {
        features.push("nl2sql");
    }
    if cfg!(feature = "rd") {
        features.push("rd");
    }
    if cfg!(feature = "bot-agents") {
        features.push("bot-agents");
    }
    if cfg!(feature = "projects") {
        features.push("projects");
    }
    if features.is_empty() {
        "default".to_string()
    } else {
        features.join(",")
    }
}

pub fn configured_tokio_worker_stack_size_bytes() -> usize {
    std::env::var("AOS_TOKIO_WORKER_STACK_SIZE_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 2 * 1024 * 1024)
        .unwrap_or(DEFAULT_TOKIO_WORKER_STACK_SIZE_BYTES)
}

fn log_startup_fingerprint() {
    let current_exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("unknown ({error})"));
    tracing::info!(
        current_exe = %current_exe,
        package_version = env!("CARGO_PKG_VERSION"),
        build_profile = if cfg!(debug_assertions) { "debug" } else { "release" },
        enabled_features = %enabled_feature_list(),
        tokio_worker_stack_size_bytes = configured_tokio_worker_stack_size_bytes(),
        rust_log = ?std::env::var("RUST_LOG").ok(),
        "web-server startup fingerprint"
    );
}

async fn log_http_errors(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = crate::auth_middleware::sanitized_request_uri(req.uri());
    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status();
    if status.is_server_error() {
        tracing::error!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "http request returned 5xx"
        );
    } else if status == StatusCode::PRECONDITION_REQUIRED {
        tracing::warn!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "http request blocked before setup completed"
        );
    } else if status.is_client_error() {
        tracing::warn!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "http request returned 4xx"
        );
    }
    response
}

fn normalized_cors_origin(raw: &str) -> Option<HeaderValue> {
    let uri = raw.trim().parse::<Uri>().ok()?;
    let scheme = uri.scheme_str()?;
    let authority = uri.authority()?.as_str();
    HeaderValue::from_str(&format!("{scheme}://{authority}")).ok()
}

fn build_cors_layer(base_url: &str) -> CorsLayer {
    let configured = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let configured_origins = configured
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .collect::<Vec<_>>();

    if configured_origins.iter().any(|origin| *origin == "*") {
        tracing::warn!(
            "CORS_ALLOWED_ORIGINS explicitly allows every origin; use a fixed origin list outside isolated development"
        );
        return CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
    }

    let raw_origins = if configured_origins.is_empty() {
        vec![base_url]
    } else {
        configured_origins
    };
    let mut origins = raw_origins
        .into_iter()
        .filter_map(normalized_cors_origin)
        .collect::<Vec<_>>();
    origins.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    origins.dedup();

    let layer = CorsLayer::new().allow_methods(Any).allow_headers(Any);
    if origins.is_empty() {
        tracing::warn!(
            base_url,
            "no valid CORS origins were configured; cross-origin browser requests are disabled"
        );
        layer
    } else {
        layer.allow_origin(AllowOrigin::list(origins))
    }
}

#[cfg(test)]
mod cors_tests {
    use super::normalized_cors_origin;

    #[test]
    fn cors_origin_keeps_only_scheme_and_authority() {
        let origin = normalized_cors_origin("https://aos.example.com/app/path?ignored=1")
            .expect("valid origin");
        assert_eq!(origin, "https://aos.example.com");
    }

    #[test]
    fn cors_origin_rejects_relative_or_malformed_values() {
        assert!(normalized_cors_origin("/relative").is_none());
        assert!(normalized_cors_origin("not a url").is_none());
    }
}

/// Build the Axum router with all routes mounted.
pub fn build_router(state: AppState) -> Router<()> {
    init_bot_gateway_tls_provider();

    let cors = build_cors_layer(&state.base_url);
    let app_state = if state.usage_writer.is_some() {
        state
    } else {
        let usage_writer = crate::routes::chat::TokenUsageWriter::new(state.db.clone());
        state.with_usage_writer(usage_writer)
    };

    #[allow(unused_mut)]
    let mut base: Router<AppState> = Router::new()
        .nest("/api/v1/auth", routes::auth::routes(app_state.clone()))
        .nest("/api/v1/setup", routes::setup::routes())
        .nest("/api/v1/users", routes::users::routes(app_state.clone()))
        .nest(
            "/api/v1/notifications",
            routes::notifications::routes(app_state.clone()),
        )
        .nest("/api/v1/dashboard", routes::dashboard::routes(&app_state))
        .nest("/api/v1/mcp", routes::mcp::routes(app_state.clone()))
        .nest(
            "/api/v1/memory",
            routes::memory_continuity::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/workspace",
            routes::personal_workspace::routes(app_state.clone()),
        )
        .nest("/api/v1/skills", routes::skills::routes(app_state.clone()))
        .nest("/api/v1/hooks", routes::hooks::routes(app_state.clone()))
        .nest(
            "/api/v1/apikeys",
            routes::apikeys::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/sessions",
            routes::sessions::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/tenants",
            routes::tenants::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/chat",
            routes::chat::routes(app_state.clone())
                .merge(routes::chat_capabilities::routes(app_state.clone())),
        )
        .nest(
            "/api/v1/chat",
            routes::chat_intelligence::routes(app_state.clone()),
        )
        .nest("/api/v1/uploads", routes::upload::routes(app_state.clone()))
        .nest("/api/v1/config", routes::config::routes(app_state.clone()))
        .nest("/api/v1/demo", routes::demo::routes(app_state.clone()))
        .nest(
            "/api/v1/agent-ops",
            routes::agent_ops::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/agent-runtime",
            routes::agent_runtime::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/tasks",
            routes::task_control::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/bot-identities",
            routes::task_control::identity_routes(app_state.clone()),
        )
        // WebSocket endpoints (with auth middleware)
        .nest("/ws", routes::system_events::ws_routes(app_state.clone()));

    #[cfg(feature = "bot-agents")]
    {
        base = base.nest(
            "/api/v1/bot-agents",
            routes::bot_agents::routes(app_state.clone()),
        );
        base = base.nest(
            "/api/v1/super-assistant",
            routes::super_assistant::routes(app_state.clone()),
        );
    }
    #[cfg(feature = "agent")]
    {
        base = base.nest("/api/v1/agent", routes::agent::routes(app_state.clone()));
    }
    #[cfg(feature = "pm")]
    {
        base = base.nest("/api/v1/pm", routes::pm::routes(app_state.clone()));
    }
    #[cfg(feature = "projects")]
    {
        base = base.nest(
            "/api/v1/projects",
            routes::projects::routes(app_state.clone()),
        );
    }
    #[cfg(feature = "rd")]
    {
        base = base.nest("/api/v1/rd", routes::rd::routes(app_state.clone()));
    }
    #[cfg(feature = "nl2sql")]
    {
        base = base
            .nest(
                "/api/v1/data-sources",
                routes::data_sources::routes(app_state.clone()),
            )
            .nest("/api/v1/nl2sql", routes::nl2sql::routes(app_state.clone()));
    }

    base.layer(axum::middleware::from_fn_with_state(
        app_state.clone(),
        crate::auth_middleware::require_setup_initialized,
    ))
    .layer(cors)
    .layer(axum::middleware::from_fn(log_http_errors))
    .layer(TraceLayer::new_for_http())
    .with_state(app_state)
}

/// Build the agent session manager.
///
/// Injects the Super_Assistant "extract → persist → compact" hook factory so
/// that every runtime the manager builds runs the extract → persist → pinned
/// closure at its real auto-compaction trigger, persisting key info to
/// Unified_Memory *before* compaction commits (先持久化再压缩, Req 4.1 / 4.3 /
/// 4.9). The factory captures only a cheap DB pool handle (not `AppState`), so
/// the hook — held per session by the manager — never forms a reference cycle
/// back to the manager stored inside `AppState`.
pub fn build_agent_manager(
    db: &sqlx::SqlitePool,
    data_dir: std::path::PathBuf,
    config_home: std::path::PathBuf,
    config_registry: Arc<agent_gateway::TenantConfigRegistry>,
) -> std::result::Result<Arc<agent_gateway::AgentSessionManager>, agent_gateway::GatewayError> {
    let hook_config_registry = config_registry.clone();
    let compaction_hook_factory: agent_gateway::CompactionHookFactory =
        Arc::new(move |ctx: agent_gateway::CompactionHookContext| {
            let tenant_id = ctx.tenant_id.clone();
            let user_id = ctx.user_id.clone();
            let session_id = ctx.session_id.clone();
            let hook = crate::routes::super_assistant::RuntimeCompactionHook::new(
                ctx.db.clone(),
                tenant_id.clone(),
                user_id.clone(),
                session_id.clone(),
                ctx.app,
            )
            .with_config_registry(hook_config_registry.clone(), ctx.model.clone())
            .with_execution_kernel(Arc::new(
                crate::semantic_kernel_store::RuntimeExecutionKernel::new(
                    ctx.db, tenant_id, user_id, session_id,
                ),
            ));
            Arc::new(hook) as Arc<dyn runtime::CompactionHook>
        });
    agent_gateway::build_session_manager_with_registry(
        db,
        data_dir,
        config_home,
        config_registry,
        Some(compaction_hook_factory),
    )
}

/// Build the minimal application state needed by a standalone PM worker
/// process. It intentionally skips HTTP-only background services.
#[cfg(all(feature = "pm", feature = "agent"))]
pub async fn init_pm_worker_state(
    data_dir: PathBuf,
    default_model: Option<String>,
) -> anyhow::Result<AppState> {
    init_tracing();
    log_startup_fingerprint();
    let started = Instant::now();
    let mut state = AppState::new(data_dir.clone(), default_model).await?;
    let usage_writer = crate::routes::chat::TokenUsageWriter::new(state.db.clone());
    state = state.with_usage_writer(usage_writer);
    let config_registry = Arc::new(agent_gateway::TenantConfigRegistry::new(state.db.clone()));
    state.config_registry = Some(config_registry.clone());
    let config_home = data_dir.clone();
    let agent_manager = build_agent_manager(&state.db, data_dir, config_home, config_registry)?;
    state.agent_manager = Some(agent_manager);
    log_startup_phase(started, "pm_worker_state");
    Ok(state)
}

#[cfg(all(feature = "pm", feature = "agent"))]
pub async fn run_pm_worker_loop(state: AppState) {
    use std::time::Duration;
    use tokio::time::{interval, MissedTickBehavior};

    let runtime_interval_secs = std::env::var("PM_RESEARCH_TASK_RUNTIME_POLL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5)
        .max(1);
    tracing::info!(
        runtime_interval_secs,
        "standalone PM worker process started"
    );

    let mut runtime_ticker = interval(Duration::from_secs(runtime_interval_secs));
    runtime_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    runtime_ticker.tick().await;

    loop {
        tokio::select! {
            _ = runtime_ticker.tick() => {
                if let Err(error) = crate::routes::agent::run_pm_background_runtime_cycle(&state).await {
                    tracing::warn!(
                        error = %error,
                        error_debug = ?error,
                        "standalone PM worker runtime cycle failed"
                    );
                }
            }
            _ = pm_worker_shutdown_signal() => {
                tracing::info!("standalone PM worker shutdown received");
                break;
            }
        }
    }
}

#[cfg(all(feature = "pm", feature = "agent"))]
async fn pm_worker_shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

async fn web_server_shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/// Start the HTTP server.
pub async fn serve(addr: SocketAddr, data_dir: PathBuf) {
    serve_with_options(addr, data_dir, None, None, None).await;
}

/// Start the HTTP server with optional telemetry, model, and Web UI overrides.
pub async fn serve_with_options(
    addr: SocketAddr,
    data_dir: PathBuf,
    telemetry_dir: Option<PathBuf>,
    default_model: Option<String>,
    web_dir: Option<PathBuf>,
) {
    #[cfg(feature = "nl2sql")]
    crate::nl2sql::embedding::configure_local_embedding_cache_for_data_dir(&data_dir)
        .expect("failed to configure local embedding cache");

    let startup_started = Instant::now();
    init_tracing();
    log_startup_fingerprint();
    log_startup_phase(startup_started, "tracing_init");

    // Initialize system events broadcast channel
    routes::system_events::init_broadcast_channel();

    let phase_started = Instant::now();
    let mut state = AppState::new(data_dir.clone(), default_model)
        .await
        .expect("failed to init app state");
    let usage_writer = crate::routes::chat::TokenUsageWriter::new(state.db.clone());
    state = state.with_usage_writer(usage_writer);
    log_startup_phase(phase_started, "app_state");

    #[cfg(feature = "nl2sql")]
    let embed_store = {
        // Initialize the registry of physically isolated tenant/profile stores.
        let phase_started = Instant::now();
        let embed_store = match crate::nl2sql::embedding::EmbeddingStoreRegistry::open(
            data_dir.join("nl2sql").join("embedding-profiles"),
        ) {
            Ok(store) => {
                log_startup_phase(phase_started, "nl2sql_embedding_store_open");
                let store = Arc::new(store);
                tracing::info!("NL2SQL embedding profile registry initialized");
                Some(store)
            }
            Err(e) => {
                log_startup_phase(phase_started, "nl2sql_embedding_store_open");
                tracing::warn!(
                    "failed to init NL2SQL embedding store: {e}. Semantic routing disabled."
                );
                None
            }
        };
        state.nl2sql_embedding_store = embed_store.clone();
        embed_store
    };

    #[cfg(feature = "nl2sql")]
    if let Some(registry) = embed_store.clone() {
        crate::nl2sql::embedding_reindex_worker::start(state.db.clone(), registry);
        tracing::info!("NL2SQL embedding shadow-index worker started");
    }

    // Initialize RD embedding store (SQLite-backed, best-effort). The store is
    // only used for semantic context ranking; repository/task indexing is
    // scheduled asynchronously so Code 开发 main flows never wait on it.
    #[cfg(feature = "rd")]
    {
        let phase_started = Instant::now();
        state.rd_embedding_store = match crate::routes::rd::embedding::RdEmbeddingStore::open(
            &data_dir.join("rd").join("embeddings.db"),
        ) {
            Ok(store) => {
                log_startup_phase(phase_started, "rd_embedding_store_open");
                tracing::info!("RD embedding store initialized");
                Some(Arc::new(store))
            }
            Err(e) => {
                log_startup_phase(phase_started, "rd_embedding_store_open");
                tracing::warn!(
                    "failed to init RD embedding store: {e}. RD semantic retrieval disabled."
                );
                None
            }
        };
    }

    // Initialize NL2SQL routing engine if store is available
    #[cfg(feature = "nl2sql")]
    {
        let routing_engine = embed_store.as_ref().and_then(|registry| {
            let embed_url = std::env::var("EMBEDDING_BASE_URL").ok();
            let embed_api_key =
                runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_EMBEDDING_ENV_FALLBACK")
                    .then(|| {
                        std::env::var("OPENAI_API_KEY")
                            .ok()
                            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                    })
                    .flatten();
            let config = crate::nl2sql::local_embedding_config_for_runtime();
            let profile_id = config.profile_id("__runtime_default__");
            registry
                .profile_store(
                    "__runtime_default__",
                    &profile_id,
                    std::env::var("EMBEDDING_MODEL")
                        .ok()
                        .as_deref()
                        .unwrap_or(crate::nl2sql::LOCAL_EMBEDDING_MODEL),
                    embed_url.clone(),
                )
                .ok()
                .map(|store| {
                    Arc::new(crate::nl2sql::routing::RoutingEngine::new(
                        store,
                        std::env::var("EMBEDDING_MODEL")
                            .ok()
                            .as_deref()
                            .or(Some(crate::nl2sql::LOCAL_EMBEDDING_MODEL)),
                        embed_url,
                        embed_api_key,
                    ))
                })
        });
        state.nl2sql_routing_engine = routing_engine;
    }

    let phase_started = Instant::now();
    let consumer_dir = telemetry_dir.unwrap_or_else(|| data_dir.clone());
    telemetry::start_telemetry_consumer(consumer_dir, state.db.clone());
    log_startup_phase(phase_started, "telemetry_consumer_start");

    let config_registry = Arc::new(agent_gateway::TenantConfigRegistry::new(state.db.clone()));
    state.config_registry = Some(config_registry.clone());
    #[cfg(feature = "nl2sql")]
    routes::nl2sql::reference::start_sql_knowledge_import_worker(state.clone());

    #[cfg(feature = "agent")]
    {
        let phase_started = Instant::now();
        let config_home = data_dir.clone();
        let agent_manager = build_agent_manager(
            &state.db,
            data_dir.clone(),
            config_home.clone(),
            config_registry.clone(),
        )
        .expect("failed to build agent session manager");
        state.agent_manager = Some(agent_manager.clone());
        log_startup_phase(phase_started, "agent_manager_build");
    }

    #[cfg(feature = "projects")]
    {
        let phase_started = Instant::now();
        let gitlab_manager = Arc::new(agent_gateway::GitlabProjectManager::new(
            state.db.clone(),
            data_dir.clone(),
        ));
        state.gitlab_manager = Some(gitlab_manager);
        log_startup_phase(phase_started, "gitlab_manager_build");
    }

    // Start background periodic MCP server health checks
    let phase_started = Instant::now();
    routes::mcp::start_periodic_mcp_checker(state.clone());
    log_startup_phase(phase_started, "mcp_checker_start");

    let phase_started = Instant::now();
    routes::task_control_worker::ensure_task_control_schema(&state)
        .await
        .expect("WatchDog control-plane schema health check failed");
    routes::task_control_worker::start_task_control_workers(state.clone());
    log_startup_phase(phase_started, "task_control_workers_start");

    // On startup, mark any tasks that were left in 'running'/'pending' by a
    // previous crash as 'failed'. This prevents them from blocking future
    // refreshes indefinitely. This is safe because no worker is executing
    // them — the process that owned them is gone.
    #[cfg(feature = "nl2sql")]
    {
        let db = state.db.clone();
        tokio::spawn(async move {
            let phase_started = Instant::now();
            let result = sqlx::query(
                "UPDATE nl2sql_refresh_tasks \
                 SET status = 'failed', \
                     error_message = 'server restarted while task was in progress' \
                 WHERE status IN ('running', 'pending')",
            )
            .execute(&db)
            .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => tracing::warn!(
                    count = r.rows_affected(),
                    "startup: marked orphaned refresh tasks as failed"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "startup: failed to clean up orphaned tasks"),
            }
            log_startup_phase(phase_started, "nl2sql_orphan_refresh_cleanup_background");
        });
    }

    // Start background periodic schema refresh + semantic re-indexing. Keep
    // the handle + shutdown sender so a Ctrl-C can stop the cycle cleanly
    // instead of having the task abort mid-refresh.
    #[cfg(feature = "nl2sql")]
    let (scheduler_shutdown, scheduler_handle) =
        routes::datasource_scheduler::start_periodic_schema_refresh(state.clone());
    #[cfg(feature = "pm")]
    let (pm_scheduler_shutdown, pm_scheduler_handle) =
        routes::pm_scheduler::start_periodic_pm_scheduler(state.clone());
    #[cfg(feature = "rd")]
    if let Err(error) = routes::rd::recover_interrupted_plan_generations(&state.db).await {
        tracing::warn!(%error, "failed to recover interrupted RD plan generation states");
    }
    #[cfg(feature = "nl2sql")]
    match routes::nl2sql::attribution::recover_interrupted_attribution_tasks(&state.db).await {
        Ok(count) if count > 0 => tracing::warn!(
            count,
            "archived interrupted data-attribution tasks while preserving durable progress"
        ),
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "failed to recover interrupted data-attribution tasks")
        }
    }
    #[cfg(feature = "rd")]
    let (rd_repository_scheduler_shutdown, rd_repository_scheduler_handle) =
        routes::rd::start_periodic_repository_sync(state.clone());
    #[cfg(feature = "bot-agents")]
    let phase_started = Instant::now();
    #[cfg(feature = "bot-agents")]
    routes::bot_agents_inbound::start_bot_agent_inbound_runtime(state.clone());
    #[cfg(feature = "bot-agents")]
    log_startup_phase(phase_started, "bot_agent_inbound_runtime_start");
    #[cfg(feature = "bot-agents")]
    {
        let phase_started = Instant::now();
        routes::bot_agents::start_bot_gateway_queue_worker(state.clone());
        log_startup_phase(phase_started, "bot_gateway_queue_worker_start");
    }
    #[cfg(feature = "nl2sql")]
    let (ann_shutdown, ann_handle) = if let Some(store) = embed_store.clone() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let interval_secs = std::env::var("NL2SQL_ANN_SNAPSHOT_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30)
            .max(5);
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // skip the immediate first tick during server startup
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let store = Arc::clone(&store);
                        match tokio::task::spawn_blocking(move || store.persist_ann_snapshots_if_dirty()).await {
                            Ok(Ok(count)) if count > 0 => tracing::info!(count, "ANN profile snapshots persisted to disk"),
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => tracing::warn!(error = %e, "ANN snapshot persist failed"),
                            Err(e) => tracing::warn!(error = %e, "ANN snapshot worker join failed"),
                        }
                    }
                    changed = rx.changed() => {
                        if changed.is_err() || *rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let shutdown_state = state.clone();
    let phase_started = Instant::now();
    let api_router = build_router(state);
    let router = if let Some(web_dir) = web_dir {
        tracing::info!(web_dir = %web_dir.display(), "serving built Web UI");
        Router::new()
            .merge(api_router)
            .fallback_service(web_ui_service(web_dir))
    } else {
        api_router
    };
    log_startup_phase(phase_started, "router_build");

    let phase_started = Instant::now();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind port");
    log_startup_phase(phase_started, "tcp_bind");
    tracing::info!(
        addr = %addr,
        startup_elapsed_ms = startup_started.elapsed().as_millis() as u64,
        "listening on {addr}"
    );

    // Keep graceful shutdown bounded. Long-lived SSE/WebSocket connections can
    // otherwise keep Axum alive indefinitely after SIGTERM.
    let (shutdown_tx, mut shutdown_observer) = tokio::sync::watch::channel(false);
    let mut server_shutdown = shutdown_tx.subscribe();
    tokio::spawn(async move {
        web_server_shutdown_signal().await;
        tracing::info!("shutdown signal received, stopping server");
        let _ = shutdown_tx.send(true);
    });
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = server_shutdown.changed().await;
        }),
    );
    tokio::pin!(server);
    let server_result = tokio::select! {
        result = &mut server => result,
        changed = shutdown_observer.changed() => {
            if changed.is_err() {
                (&mut server).await
            } else {
                match tokio::time::timeout(std::time::Duration::from_secs(5), &mut server).await {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::warn!("HTTP connections did not drain within 5s; forcing server close");
                        Ok(())
                    }
                }
            }
        }
    };
    if let Err(e) = server_result {
        tracing::error!("server error: {e}");
    }

    // Tell the scheduler to stop and wait for its current cycle.
    #[cfg(feature = "nl2sql")]
    let _ = scheduler_shutdown.send(true);
    #[cfg(feature = "pm")]
    let _ = pm_scheduler_shutdown.send(true);
    #[cfg(feature = "rd")]
    let _ = rd_repository_scheduler_shutdown.send(true);
    #[cfg(feature = "nl2sql")]
    if let Some(tx) = ann_shutdown {
        let _ = tx.send(true);
    }
    // Use one shared deadline instead of waiting for each worker serially. A
    // timed-out task is explicitly aborted so shutdown duration stays bounded.
    let mut shutdown_handles: Vec<(&'static str, tokio::task::JoinHandle<()>)> = Vec::new();
    #[cfg(feature = "nl2sql")]
    shutdown_handles.push(("scheduler", scheduler_handle));
    #[cfg(feature = "pm")]
    shutdown_handles.push(("pm scheduler", pm_scheduler_handle));
    #[cfg(feature = "rd")]
    shutdown_handles.push(("repository scheduler", rd_repository_scheduler_handle));
    #[cfg(feature = "nl2sql")]
    if let Some(handle) = ann_handle {
        shutdown_handles.push(("ANN snapshot worker", handle));
    }
    let shutdown_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    for (name, mut handle) in shutdown_handles {
        match tokio::time::timeout_at(shutdown_deadline, &mut handle).await {
            Ok(Ok(())) => tracing::info!(task = name, "background task exited cleanly"),
            Ok(Err(error)) => {
                tracing::warn!(task = name, %error, "background task panicked on exit");
            }
            Err(_) => {
                tracing::warn!(
                    task = name,
                    "background task missed shutdown deadline; aborting"
                );
                handle.abort();
                let _ = handle.await;
            }
        }
    }
    if let Err(error) = shutdown_state.mark_clean_shutdown().await {
        tracing::warn!(%error, "failed to checkpoint SQLite or clear the unclean marker");
    }
}

#[cfg(test)]
mod web_ui_service_tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    #[tokio::test]
    async fn agent_manager_reuses_web_config_registry() {
        let pool = crate::test_sqlite_pool().await;
        let registry = Arc::new(agent_gateway::TenantConfigRegistry::new(pool.clone()));
        let data_dir = std::env::temp_dir().join(format!("aos-agent-{}", uuid::Uuid::new_v4()));
        let manager = build_agent_manager(&pool, data_dir.clone(), data_dir, registry.clone())
            .expect("build agent manager");

        assert!(Arc::ptr_eq(&registry, &manager.config_registry()));
        pool.close().await;
    }

    #[tokio::test]
    async fn spa_routes_fall_back_to_index_with_success_status() {
        let web_dir = std::env::temp_dir().join(format!("aos-web-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&web_dir).expect("create temporary Web UI directory");
        std::fs::write(web_dir.join("index.html"), "<html>AOS WebUI</html>")
            .expect("write temporary index");

        let response = web_ui_service(web_dir.clone())
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("serve SPA route");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(Body::new(response.into_body()), 1024)
            .await
            .expect("read response body");
        assert_eq!(&body[..], b"<html>AOS WebUI</html>");

        std::fs::remove_dir_all(web_dir).expect("remove temporary Web UI directory");
    }
}
