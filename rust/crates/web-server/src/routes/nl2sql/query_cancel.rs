//! Query cancellation support via driver-native KILL commands.
//!
//! For MySQL/PostgreSQL: captures the thread/process ID before executing,
//! then issues `KILL <id>` / `pg_cancel_backend(<pid>)` on cancellation.
//! For ClickHouse: uses `KILL QUERY WHERE query_id = ?`.
//! For Trino: uses `DELETE /v1/query/{id}`.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;

/// Manages a query's lifecycle: execution and cancellation.
pub struct QueryExecutor {
    pub query_id: String,
}

impl QueryExecutor {
    pub fn new() -> Self {
        Self {
            query_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Cancels the running query using the driver-native KILL command.
    #[allow(dead_code)]
    pub async fn cancel(&self, config_json: &serde_json::Value) -> Result<(), CancelError> {
        let db_type = config_json
            .get("db_type")
            .and_then(|v| v.as_str())
            .unwrap_or("mysql");

        match db_type {
            "mysql" | "tidb" => self.cancel_mysql(config_json).await,
            "postgres" => self.cancel_postgres(config_json).await,
            "clickhouse" => self.cancel_clickhouse(config_json).await,
            "trino" => self.cancel_trino(config_json).await,
            _ => Err(CancelError::UnsupportedDbType(db_type.to_string())),
        }
    }

    async fn cancel_mysql(&self, config_json: &serde_json::Value) -> Result<(), CancelError> {
        #[derive(serde::Deserialize)]
        struct MySqlCfg {
            host: String,
            port: u16,
            database: String,
            username: String,
            password: String,
            thread_id: Option<u64>,
        }
        let cfg: MySqlCfg = serde_json::from_value(config_json.clone())
            .map_err(|e| CancelError::ConfigError(e.to_string()))?;

        if let Some(tid) = cfg.thread_id {
            let url = crate::routes::data_sources::build_mysql_url_parts(
                &cfg.username,
                &cfg.password,
                &cfg.host,
                cfg.port,
                &cfg.database,
            );
            let pool: sqlx::MySqlPool = sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(&url)
                .await
                .map_err(|e| CancelError::ConnectionFailed(e.to_string()))?;
            let mut conn = pool
                .acquire()
                .await
                .map_err(|e| CancelError::ConnectionFailed(e.to_string()))?;
            sqlx::query(&format!("KILL {tid}"))
                .execute(&mut *conn)
                .await
                .map_err(|e| CancelError::KillFailed(e.to_string()))?;
            tracing::info!(thread_id = tid, "MySQL query cancelled via KILL");
        }
        Ok(())
    }

    async fn cancel_postgres(&self, config_json: &serde_json::Value) -> Result<(), CancelError> {
        #[derive(serde::Deserialize)]
        struct PgCfg {
            host: String,
            port: u16,
            database: String,
            username: String,
            password: String,
            pid: Option<i32>,
        }
        let cfg: PgCfg = serde_json::from_value(config_json.clone())
            .map_err(|e| CancelError::ConfigError(e.to_string()))?;

        if let Some(pid) = cfg.pid {
            let url = crate::routes::data_sources::build_postgres_url_parts(
                &cfg.username,
                &cfg.password,
                &cfg.host,
                cfg.port,
                &cfg.database,
            );
            let pool: sqlx::PgPool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(&url)
                .await
                .map_err(|e| CancelError::ConnectionFailed(e.to_string()))?;
            let mut conn = pool
                .acquire()
                .await
                .map_err(|e| CancelError::ConnectionFailed(e.to_string()))?;
            sqlx::query(&format!("SELECT pg_cancel_backend({pid})"))
                .execute(&mut *conn)
                .await
                .map_err(|e| CancelError::KillFailed(e.to_string()))?;
            tracing::info!(
                pid = pid,
                "PostgreSQL query cancelled via pg_cancel_backend"
            );
        }
        Ok(())
    }

    async fn cancel_clickhouse(&self, config_json: &serde_json::Value) -> Result<(), CancelError> {
        #[derive(serde::Deserialize)]
        struct ChCfg {
            host: String,
            port: u16,
            database: String,
            username: String,
            password: String,
        }
        let cfg: ChCfg = serde_json::from_value(config_json.clone())
            .map_err(|e| CancelError::ConfigError(e.to_string()))?;
        let url = crate::routes::data_sources::build_mysql_url_parts(
            &cfg.username,
            &cfg.password,
            &cfg.host,
            cfg.port,
            &cfg.database,
        );
        let pool: sqlx::MySqlPool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(&url)
            .await
            .map_err(|e| CancelError::ConnectionFailed(e.to_string()))?;
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| CancelError::ConnectionFailed(e.to_string()))?;
        sqlx::query(&format!("KILL QUERY WHERE query_id = '{}'", self.query_id))
            .execute(&mut *conn)
            .await
            .map_err(|e| CancelError::KillFailed(e.to_string()))?;
        tracing::info!(query_id = %self.query_id, "ClickHouse query cancelled");
        Ok(())
    }

    async fn cancel_trino(&self, config_json: &serde_json::Value) -> Result<(), CancelError> {
        #[derive(serde::Deserialize)]
        struct TrinoCfg {
            host: String,
            port: u16,
            username: Option<String>,
            password: Option<String>,
            ssl: Option<bool>,
            basic_auth: Option<bool>,
            auth_token: Option<String>,
        }
        let cfg: TrinoCfg = serde_json::from_value(config_json.clone())
            .map_err(|e| CancelError::ConfigError(e.to_string()))?;
        let normalized_host = nl2sql_domain::datasource_config::normalize_host_input(&cfg.host);
        let port = normalized_host.port.unwrap_or(cfg.port);
        let secure = cfg.ssl.or(normalized_host.secure).unwrap_or(port == 443);
        let scheme = if secure { "https" } else { "http" };
        let url = format!(
            "{}://{}:{}/v1/query/{}",
            scheme, normalized_host.host, port, self.query_id
        );
        let client = reqwest::Client::new();
        let req = if let Some(ref token) = cfg.auth_token {
            client
                .delete(&url)
                .header("Authorization", format!("Bearer {token}"))
        } else if cfg
            .basic_auth
            .unwrap_or_else(|| cfg.password.as_deref().is_some_and(|p| !p.is_empty()))
        {
            client.delete(&url).basic_auth(
                cfg.username.as_deref().unwrap_or_default(),
                cfg.password.clone(),
            )
        } else {
            client.delete(&url)
        };
        req.send()
            .await
            .map_err(|e| CancelError::KillFailed(e.to_string()))?;
        tracing::info!(query_id = %self.query_id, "Trino query cancelled");
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ActiveQueryHandle {
    pub tenant_id: String,
    pub data_source_id: String,
    pub db_type: String,
    pub query_id: String,
    pub mysql_thread_id: Option<u64>,
    pub postgres_pid: Option<i32>,
}

fn active_queries() -> &'static Mutex<HashMap<String, ActiveQueryHandle>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, ActiveQueryHandle>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_active_query(handle: ActiveQueryHandle) {
    active_queries()
        .lock()
        .insert(handle.query_id.clone(), handle);
}

pub fn unregister_active_query(query_id: &str) {
    active_queries().lock().remove(query_id);
}

pub fn active_query(query_id: &str) -> Option<ActiveQueryHandle> {
    active_queries().lock().get(query_id).cloned()
}

#[derive(Debug)]
pub enum CancelError {
    ConnectionFailed(String),
    KillFailed(String),
    ConfigError(String),
    UnsupportedDbType(String),
}

impl std::fmt::Display for CancelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancelError::ConnectionFailed(e) => write!(f, "connection failed: {}", e),
            CancelError::KillFailed(e) => write!(f, "kill command failed: {}", e),
            CancelError::ConfigError(e) => write!(f, "config error: {}", e),
            CancelError::UnsupportedDbType(t) => write!(f, "unsupported DB type: {}", t),
        }
    }
}

impl std::error::Error for CancelError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_query_registry_round_trips_and_unregisters() {
        let query_id = format!("test-query-{}", uuid::Uuid::new_v4());
        let handle = ActiveQueryHandle {
            tenant_id: "tenant-a".to_string(),
            data_source_id: "ds-1".to_string(),
            db_type: "mysql".to_string(),
            query_id: query_id.clone(),
            mysql_thread_id: Some(42),
            postgres_pid: None,
        };

        register_active_query(handle.clone());

        let registered = active_query(&query_id).expect("active query should be registered");
        assert_eq!(registered.tenant_id, handle.tenant_id);
        assert_eq!(registered.data_source_id, handle.data_source_id);
        assert_eq!(registered.db_type, handle.db_type);
        assert_eq!(registered.mysql_thread_id, Some(42));
        assert_eq!(registered.postgres_pid, None);

        unregister_active_query(&query_id);
        assert!(active_query(&query_id).is_none());
    }
}
