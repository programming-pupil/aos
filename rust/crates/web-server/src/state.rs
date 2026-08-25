//! Application state shared across all route handlers.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub(crate) struct PlatformLifecycle {
    _instance_lock: File,
    unclean_marker: PathBuf,
}

fn acquire_platform_lifecycle(
    data_dir: &std::path::Path,
) -> anyhow::Result<(PlatformLifecycle, bool)> {
    std::fs::create_dir_all(data_dir)?;
    let lock_path = data_dir.join("aos.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock_file).map_err(|error| {
        anyhow::anyhow!(
            "another AOS instance is already using data directory {}: {error}",
            data_dir.display()
        )
    })?;

    let unclean_marker = data_dir.join("aos.unclean");
    let recovered_from_unclean_shutdown = unclean_marker.exists();
    let marker_tmp = data_dir.join("aos.unclean.tmp");
    let mut marker = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&marker_tmp)?;
    writeln!(marker, "pid={}", std::process::id())?;
    marker.sync_all()?;
    std::fs::rename(&marker_tmp, &unclean_marker)?;

    Ok((
        PlatformLifecycle {
            _instance_lock: lock_file,
            unclean_marker,
        },
        recovered_from_unclean_shutdown,
    ))
}

fn start_database_pool_metrics(pool: SqlitePool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let size = pool.size();
            let idle = pool.num_idle();
            tracing::info!(
                size,
                idle,
                active = usize::try_from(size)
                    .unwrap_or(usize::MAX)
                    .saturating_sub(idle),
                "SQLite pool health"
            );
        }
    });
}

fn allow_insecure_development_secrets() -> bool {
    cfg!(debug_assertions)
        && std::env::var("AOS_ALLOW_INSECURE_DEV_SECRETS")
            .ok()
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn validate_startup_secret(
    name: &str,
    value: &str,
    minimum_bytes: usize,
    exact_bytes: Option<usize>,
    allow_insecure: bool,
) -> anyhow::Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{name} must not be empty; run scripts/generate-env.sh or set it explicitly");
    }
    if let Some(expected) = exact_bytes {
        if value.len() != expected {
            anyhow::bail!(
                "{name} must be exactly {expected} bytes, got {}",
                value.len()
            );
        }
    } else if value.len() < minimum_bytes {
        anyhow::bail!(
            "{name} must be at least {minimum_bytes} bytes, got {}",
            value.len()
        );
    }

    let lower = trimmed.to_ascii_lowercase();
    let known_placeholder = lower.contains("change-me")
        || lower.contains("replace-me")
        || lower.starts_with("your-")
        || lower.starts_with("dev-secret")
        || trimmed == "12345678901234567890123456789012";
    if known_placeholder && !allow_insecure {
        anyhow::bail!(
            "{name} uses a public development placeholder; run scripts/generate-env.sh or set a unique secret"
        );
    }
    Ok(())
}

fn required_startup_secret(
    name: &str,
    minimum_bytes: usize,
    exact_bytes: Option<usize>,
    allow_insecure: bool,
) -> anyhow::Result<String> {
    let value = std::env::var(name)
        .map_err(|_| anyhow::anyhow!("{name} environment variable must be set"))?;
    validate_startup_secret(name, &value, minimum_bytes, exact_bytes, allow_insecure)?;
    Ok(value)
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub data_dir: PathBuf,
    pub(crate) platform_lifecycle: Option<Arc<PlatformLifecycle>>,
    /// AOS platform database with workload-isolated connection pools. All three
    /// pools share one WAL database, while background and telemetry work cannot
    /// consume the interactive request connection budget.
    pub db: SqlitePool,
    pub control_db: SqlitePool,
    pub telemetry_db: SqlitePool,
    #[cfg(feature = "pm")]
    pub(crate) pm_telemetry: Arc<crate::routes::agent::PmTelemetrySink>,
    pub jwt_secret: Arc<RwLock<String>>,
    pub base_url: String,
    pub default_model: String,
    pub setup_initialized_cache: Arc<AtomicBool>,
    pub usage_writer: Option<Arc<crate::routes::chat::TokenUsageWriter>>,
    pub agent_manager: Option<Arc<agent_gateway::AgentSessionManager>>,
    #[cfg(feature = "projects")]
    pub gitlab_manager: Option<Arc<agent_gateway::GitlabProjectManager>>,
    pub config_registry: Option<Arc<agent_gateway::TenantConfigRegistry>>,
    /// NL2SQL embedding store — SQLite-backed with LRU cache.
    #[cfg(feature = "nl2sql")]
    pub nl2sql_embedding_store: Option<Arc<crate::nl2sql::embedding::EmbeddingStoreRegistry>>,
    /// RD embedding store — SQLite-backed semantic index for Code 开发.
    #[cfg(feature = "rd")]
    pub rd_embedding_store: Option<Arc<crate::routes::rd::embedding::RdEmbeddingStore>>,
    /// NL2SQL routing engine — semantic data source/table selection.
    #[cfg(feature = "nl2sql")]
    pub nl2sql_routing_engine: Option<Arc<crate::nl2sql::routing::RoutingEngine>>,
    /// NL2SQL datasource pool cache — reuses per-tenant per-datasource sqlx
    /// pools across executions so we don't pay TCP+TLS+auth on every query.
    #[cfg(feature = "nl2sql")]
    pub nl2sql_pool_cache: Arc<crate::nl2sql::datasource_pool::PoolCache>,
    /// NL2SQL per-tenant rate limiter (in-memory + optional DB-backed for
    /// multi-replica). Default: 60 req/min, single-replica.
    #[cfg(feature = "nl2sql")]
    pub nl2sql_rate_limiter: Arc<crate::nl2sql::rate_limiter::TenantRateLimiter>,
}

impl AppState {
    pub async fn new(data_dir: PathBuf, default_model: Option<String>) -> anyhow::Result<Self> {
        let allow_insecure = allow_insecure_development_secrets();
        let jwt_secret = required_startup_secret("JWT_SECRET", 32, None, allow_insecure)?;
        let _encryption_key =
            required_startup_secret("ENCRYPTION_KEY", 32, Some(32), allow_insecure)?;
        #[cfg(feature = "projects")]
        let _token_encryption_key =
            required_startup_secret("TOKEN_ENCRYPTION_KEY", 32, None, allow_insecure)?;

        let (platform_lifecycle, recovered_from_unclean_shutdown) =
            acquire_platform_lifecycle(&data_dir)?;
        let database_path = data_dir.join("aos.db");
        let max_connections = env_u32("AOS_SQLITE_MAX_CONNECTIONS", 4).clamp(1, 8);
        // Background governance, task, and encryption workers share this
        // SQLite pool with interactive requests. Give short write
        // transactions enough time to serialize instead of surfacing
        // avoidable `database is locked` errors to worker loops.
        let busy_timeout_ms = env_u64("AOS_SQLITE_BUSY_TIMEOUT_MS", 30_000).clamp(1_000, 60_000);
        let acquire_timeout_secs = env_u64("AOS_SQLITE_ACQUIRE_TIMEOUT_SECS", 30).clamp(1, 300);
        tracing::info!(
            database_path = %database_path.display(),
            max_connections,
            busy_timeout_ms,
            acquire_timeout_secs,
            "initializing SQLite platform database"
        );
        let connect_options = SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_millis(busy_timeout_ms))
            .pragma("wal_autocheckpoint", "1000")
            .pragma("temp_store", "MEMORY");
        let db = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
            .connect_with(connect_options.clone())
            .await?;
        sqlx::migrate!("./sqlite-migrations").run(&db).await?;
        crate::semantic_kernel_store::process_fault_point("migration.after_commit");
        let recovered_turns =
            crate::semantic_kernel_store::recover_abandoned_runtime_turns(&db).await?;
        if recovered_turns > 0 {
            tracing::warn!(
                recovered_turns,
                "recovered abandoned runtime turns and released their budgets"
            );
        }
        let recovered_dispatches =
            crate::governed_provider::recover_incomplete_dispatches(&db).await?;
        if recovered_dispatches > 0 {
            tracing::warn!(
                recovered_dispatches,
                "recovered provider dispatches left non-terminal by the previous process"
            );
        }
        let internal_process_tck = cfg!(debug_assertions)
            && std::env::var("AOS_INTERNAL_PROCESS_TCK").as_deref() == Ok("1");
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&db)
            .await?;
        if foreign_keys != 1 {
            anyhow::bail!("SQLite foreign key enforcement is disabled");
        }
        if recovered_from_unclean_shutdown {
            let quick_check: String = sqlx::query_scalar("PRAGMA quick_check")
                .fetch_one(&db)
                .await?;
            if quick_check != "ok" {
                anyhow::bail!("SQLite quick_check failed after an unclean shutdown: {quick_check}");
            }
            tracing::warn!("previous AOS shutdown was unclean; SQLite quick_check passed");
        }
        let control_db = SqlitePoolOptions::new()
            .max_connections(max_connections.clamp(1, 2))
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
            .connect_with(connect_options.clone())
            .await?;
        let telemetry_db = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
            .connect_with(connect_options)
            .await?;
        if !internal_process_tck {
            crate::semantic_kernel_store::start_encryption_key_rotation_worker(
                control_db.clone(),
                data_dir.clone(),
            );
            crate::semantic_memory_worker::start_memory_governance_worker(control_db.clone());
        }
        start_database_pool_metrics(db.clone());

        let default_model = default_model.unwrap_or_else(|| {
            std::env::var("DEFAULT_MODEL")
                .unwrap_or_else(|_| "anthropic/claude-opus-4-8".to_string())
        });

        #[cfg(feature = "pm")]
        let pm_telemetry =
            crate::routes::agent::PmTelemetrySink::start(telemetry_db.clone(), &data_dir).await?;

        Ok(Self {
            data_dir,
            platform_lifecycle: Some(Arc::new(platform_lifecycle)),
            db,
            control_db,
            telemetry_db,
            #[cfg(feature = "pm")]
            pm_telemetry,
            jwt_secret: Arc::new(RwLock::new(jwt_secret)),
            base_url: std::env::var("BASE_URL")
                .unwrap_or_else(|_| "http://localhost:3000".to_string()),
            default_model,
            setup_initialized_cache: Arc::new(AtomicBool::new(false)),
            usage_writer: None,
            agent_manager: None,
            #[cfg(feature = "projects")]
            gitlab_manager: None,
            config_registry: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_embedding_store: None,
            #[cfg(feature = "rd")]
            rd_embedding_store: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_routing_engine: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_pool_cache: Arc::new(crate::nl2sql::datasource_pool::PoolCache::new()),
            #[cfg(feature = "nl2sql")]
            nl2sql_rate_limiter: Arc::new(crate::nl2sql::rate_limiter::TenantRateLimiter::default()),
        })
    }

    #[must_use]
    pub fn usage_writer(&self) -> &Arc<crate::routes::chat::TokenUsageWriter> {
        self.usage_writer
            .as_ref()
            .expect("usage_writer not initialized")
    }

    #[must_use]
    pub fn agent_manager(&self) -> &Arc<agent_gateway::AgentSessionManager> {
        self.agent_manager
            .as_ref()
            .expect("agent_manager not initialized")
    }

    #[cfg(feature = "projects")]
    #[must_use]
    pub fn gitlab_manager(&self) -> &Arc<agent_gateway::GitlabProjectManager> {
        self.gitlab_manager
            .as_ref()
            .expect("gitlab_manager not initialized")
    }

    #[must_use]
    pub fn config_registry(&self) -> &Arc<agent_gateway::TenantConfigRegistry> {
        self.config_registry
            .as_ref()
            .expect("config_registry not initialized")
    }

    pub fn setup_initialized_cached(&self) -> bool {
        self.setup_initialized_cache.load(Ordering::Relaxed)
    }

    pub fn mark_setup_initialized(&self) {
        self.setup_initialized_cache.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn control_db(&self) -> &SqlitePool {
        &self.control_db
    }

    #[must_use]
    pub fn telemetry_db(&self) -> &SqlitePool {
        &self.telemetry_db
    }

    pub async fn mark_clean_shutdown(&self) -> anyhow::Result<()> {
        sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .fetch_all(&self.db)
            .await?;
        if let Some(lifecycle) = &self.platform_lifecycle {
            match std::fs::remove_file(&lifecycle.unclean_marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    #[cfg(feature = "pm")]
    #[must_use]
    pub(crate) fn pm_telemetry(&self) -> &Arc<crate::routes::agent::PmTelemetrySink> {
        &self.pm_telemetry
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::acquire_platform_lifecycle;

    #[test]
    fn platform_lifecycle_rejects_a_second_instance_and_marks_unclean_startup() {
        let data_dir = std::env::temp_dir().join(format!(
            "aos-platform-lifecycle-test-{}",
            uuid::Uuid::new_v4()
        ));
        let (first, recovered) =
            acquire_platform_lifecycle(&data_dir).expect("acquire first instance lock");
        assert!(!recovered);
        assert!(data_dir.join("aos.unclean").exists());
        assert!(acquire_platform_lifecycle(&data_dir).is_err());
        drop(first);

        let (second, recovered) =
            acquire_platform_lifecycle(&data_dir).expect("reacquire instance lock");
        assert!(recovered);
        drop(second);
        let _ = std::fs::remove_dir_all(data_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::validate_startup_secret;

    #[test]
    fn production_secret_validation_rejects_public_placeholders() {
        assert!(validate_startup_secret(
            "JWT_SECRET",
            "change-me-generate-with-openssl-rand-base64-32",
            32,
            None,
            false,
        )
        .is_err());
        assert!(validate_startup_secret(
            "ENCRYPTION_KEY",
            "12345678901234567890123456789012",
            32,
            Some(32),
            false,
        )
        .is_err());
    }

    #[test]
    fn production_secret_validation_accepts_generated_values() {
        assert!(validate_startup_secret(
            "JWT_SECRET",
            "7f9e91fcf7f9a87d58a2500ef3c9083553fb76b5077d85414955b6cb91496f7e",
            32,
            None,
            false,
        )
        .is_ok());
        assert!(validate_startup_secret(
            "ENCRYPTION_KEY",
            "a4d8fd817c7b641cb4e55a74b45c744c",
            32,
            Some(32),
            false,
        )
        .is_ok());
    }

    #[tokio::test]
    async fn sqlite_baseline_contains_unified_workspace_keyset_indexes() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect SQLite");
        sqlx::migrate!("./sqlite-migrations")
            .run(&pool)
            .await
            .expect("apply SQLite baseline");

        for (table, index) in [
            ("chat_file_workspace_files", "idx_chat_workspace_keyset"),
            ("agent_context_archives", "idx_context_archive_keyset"),
            ("chat_turn_artifacts", "idx_chat_artifact_keyset"),
            ("agent_workspace_entries", "idx_workspace_shared_keyset"),
            ("agent_event_ledger", "idx_agent_event_ledger_checkpoint"),
        ] {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_schema \
                 WHERE type = 'index' AND tbl_name = ? AND name = ?",
            )
            .bind(table)
            .bind(index)
            .fetch_one(&pool)
            .await
            .expect("query SQLite index metadata");
            assert_eq!(count, 1, "missing {table}.{index}");
        }
    }
}
