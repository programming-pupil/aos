//! Schema describer - auto-generates semantic descriptions for tables/columns using the LLM.

use api::{InputContentBlock, InputMessage, MessageRequest, OutputContentBlock};
use sqlx::{Row, SqlitePool};
use std::sync::Arc;

use crate::nl2sql::embedding::{EmbeddingModel, EmbeddingStoreRegistry};
use crate::nl2sql::{ChatTenantConfig, EmbeddingTenantConfig};

const DEFAULT_EMBED_MODEL: &str = "text-embedding-3-small";

/// Validates that a SQL identifier (table or column name) contains only
/// safe characters and is within reasonable length bounds. This prevents
/// SQL injection when identifiers are interpolated into query strings.
fn validate_sql_identifier(name: &str) -> Result<String, String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!("Invalid identifier length: {}", name.len()));
    }
    let parts = name.split('.').collect::<Vec<_>>();
    if parts.len() > 2
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_alphanumeric() || c == '_'))
    {
        return Err(format!(
            "Invalid identifier contains forbidden characters: {}",
            name
        ));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod identifier_tests {
    use super::validate_sql_identifier;

    #[test]
    fn accepts_simple_and_qualified_identifiers() {
        assert_eq!(validate_sql_identifier("orders").unwrap(), "orders");
        assert_eq!(
            validate_sql_identifier("analytics.orders").unwrap(),
            "analytics.orders"
        );
    }

    #[test]
    fn rejects_identifier_injection_and_malformed_qualification() {
        for value in [
            "orders` WHERE 1=1 --",
            "orders;DROP TABLE users",
            ".orders",
            "analytics..orders",
            "catalog.analytics.orders",
        ] {
            assert!(
                validate_sql_identifier(value).is_err(),
                "accepted {value:?}"
            );
        }
    }
}

// ── Progress reporting ────────────────────────────────────────────────────────────

/// Async progress reporter for schema refresh operations.
#[async_trait::async_trait]
pub trait ProgressReporter: Send + Sync {
    /// Called periodically during a refresh to report progress.
    /// `percent` is 0-100; `processed_tables` is the number of tables completed so far.
    async fn report(&self, percent: u32, processed_tables: u32);
}

/// No-op progress reporter used when no caller-provided reporter is available.
#[derive(Clone, Copy, Default)]
pub struct NoopProgress;

#[async_trait::async_trait]
impl ProgressReporter for NoopProgress {
    async fn report(&self, _percent: u32, _processed_tables: u32) {}
}

// ── Data types ───────────────────────────────────────────────────────────────

/// Result of a schema refresh operation.
#[derive(Debug)]
pub struct RefreshResult {
    pub tables_processed: usize,
    pub columns_processed: usize,
    pub failed_tables: Vec<(String, String)>,
    pub embedding_usage: Vec<api::Usage>,
}

/// Outcome of an update-description operation.
#[derive(Debug)]
pub struct UpdateResult {
    pub updated: bool,
    pub indexed: bool,
    pub index_error: Option<String>,
}

/// Error type for column/table/datasource description updates.
#[derive(Debug)]
pub enum UpdateDescriptionError {
    NotFound,
    UpdateFailed(String),
    Database(String),
    Other(String),
}

/// Column-level schema with AI-generated and manual semantic information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    #[serde(alias = "type")]
    pub data_type: String,
    #[serde(default, alias = "nullable")]
    pub is_nullable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample_values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_description: Option<String>,
    #[serde(default)]
    pub is_indexed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Table-level schema with AI-generated and manual semantic information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TableSchema {
    pub table_name: String,
    pub columns: Vec<ColumnSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_description: Option<String>,
    #[serde(default)]
    pub is_manual: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(default)]
    pub version: i32,
}

/// Full datasource-level semantic information.
#[derive(Debug)]
pub struct DatasourceSemantics {
    pub datasource_id: String,
    pub datasource_description: String,
    pub tables: Vec<TableSchema>,
    pub embedding_version: i32,
    pub ai_description: Option<String>,
    pub user_description: Option<String>,
    pub embedding_model: Option<String>,
    pub is_indexed: bool,
    pub version: i32,
}

// ── SchemaDescriber ─────────────────────────────────────────────────────────

pub struct SchemaDescriber {
    db: SqlitePool,
    embed_store: Arc<EmbeddingStoreRegistry>,
    embed_model: String,
    chat_cfg: Option<ChatTenantConfig>,
    usage_sink: Option<Arc<dyn Fn(api::Usage) + Send + Sync>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ColumnSemanticsResult {
    pub table_name: String,
    pub column_name: String,
    pub semantic_description: String,
    pub is_indexed: bool,
    pub version: i32,
}

impl SchemaDescriber {
    pub fn new(
        db: SqlitePool,
        embed_store: Arc<EmbeddingStoreRegistry>,
        embed_cfg: Option<EmbeddingTenantConfig>,
        chat_cfg: Option<ChatTenantConfig>,
    ) -> Self {
        let embed_model = match embed_cfg {
            Some(cfg) => cfg.model,
            None => DEFAULT_EMBED_MODEL.to_owned(),
        };
        Self {
            db,
            embed_store,
            embed_model,
            chat_cfg,
            usage_sink: None,
        }
    }

    #[must_use]
    pub fn with_usage_sink(mut self, sink: Arc<dyn Fn(api::Usage) + Send + Sync>) -> Self {
        self.usage_sink = Some(sink);
        self
    }

    pub async fn refresh_datasource(
        &self,
        tenant_id: &str,
        datasource_id: &str,
    ) -> anyhow::Result<RefreshResult> {
        self.refresh_datasource_with_progress(tenant_id, datasource_id, NoopProgress)
            .await
    }

    /// Refresh semantics using pre-parsed table schemas, bypassing the DB read of
    /// `schema_info`. Used by discover-initiated refresh to avoid racing with the
    /// discover handler's UPDATE commit.
    pub async fn refresh_schema_directly<R: ProgressReporter>(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        tables: Vec<TableSchema>,
        reporter: R,
    ) -> anyhow::Result<RefreshResult> {
        tracing::info!(
            tenant_id,
            datasource_id,
            n_tables = tables.len(),
            "SchemaDescriber: refresh_schema_directly called"
        );
        if tables.is_empty() {
            return Ok(RefreshResult {
                tables_processed: 0,
                columns_processed: 0,
                failed_tables: vec![],
                embedding_usage: vec![],
            });
        }
        let datasource_description: Option<String> =
            sqlx::query_scalar("SELECT description FROM data_sources WHERE id = ?")
                .bind(datasource_id)
                .fetch_optional(&self.db)
                .await
                .ok()
                .flatten();

        let existing: std::collections::HashMap<(String, String), String> =
            sqlx::query_as::<_, (String, String, String)>(
                "SELECT table_name, column_name, COALESCE(semantic_description, '') \
                 FROM nl2sql_table_semantics WHERE datasource_id = ? AND tenant_id = ? AND deleted_at IS NULL",
            )
            .bind(datasource_id)
            .bind(tenant_id)
            .fetch_all(&self.db)
            .await
            .map(|rows| rows.into_iter().map(|(t, c, sd)| ((t, c), sd)).collect())
            .unwrap_or_default();

        let total = tables.len() as u32;
        let mut tables_processed = 0usize;
        let mut columns_processed = 0usize;
        let mut failed_tables = Vec::new();
        let mut embedding_usage = Vec::new();

        for (i, table) in tables.iter().enumerate() {
            let table_name = &table.table_name;

            // Auto-generated table description
            if let Some(ref existing_desc) = table.ai_description {
                let _ = sqlx::query(
                    concat!(
                        "INSERT INTO nl2sql_table_desc_semantics ",
                        "(datasource_id, table_name, tenant_id, ai_description, embedding_model, is_manual, version) ",
                        "VALUES (?, ?, ?, ?, ?, 0, 1) ",
                        "ON CONFLICT DO UPDATE SET ai_description = excluded.ai_description, ",
                        "embedding_model = excluded.embedding_model, version = version + 1"
                    ),
                )
                .bind(datasource_id)
                .bind(table_name)
                .bind(tenant_id)
                .bind(existing_desc)
                .bind(&self.embed_model)
                .execute(&self.db)
                .await;
            }

            let mut embed_batch: Vec<(String, String, String)> = Vec::new();

            // Parallel-generate descriptions for columns that need it
            let needs_gen: Vec<&ColumnSchema> = table
                .columns
                .iter()
                .filter(|col| {
                    existing
                        .get(&(table_name.clone(), col.name.clone()))
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                })
                .collect();
            let gen_results: std::collections::HashMap<String, String> = {
                let results = self
                    .generate_descriptions_parallel(
                        table_name,
                        &needs_gen.iter().map(|c| (*c).clone()).collect::<Vec<_>>(),
                    )
                    .await;
                results
                    .into_iter()
                    .filter_map(|(name, r)| r.ok().map(|desc| (name, desc)))
                    .collect()
            };

            for col in &table.columns {
                let col_key = (table_name.clone(), col.name.clone());
                let existing_ai = existing.get(&col_key).cloned().unwrap_or_default();

                let description = if !existing_ai.is_empty() {
                    existing_ai
                } else {
                    let gen = gen_results
                        .get(&col.name)
                        .cloned()
                        .unwrap_or_else(|| col.name.clone());
                    if gen != existing_ai {
                        self.upsert_semantics(
                            tenant_id,
                            datasource_id,
                            table_name,
                            &col.name,
                            &gen,
                            None,
                        )
                        .await?;
                    }
                    gen
                };

                embed_batch.push((
                    table_name.clone(),
                    col.name.clone(),
                    format!(
                        "Table: {}
Column: {}
Description: {}",
                        table_name, col.name, description
                    ),
                ));
                columns_processed += 1;
            }

            match self
                .embed_and_store(tenant_id, datasource_id, &embed_batch)
                .await
            {
                Ok(usage) => {
                    if let Some(u) = usage {
                        embedding_usage.push(u);
                    }
                }
                Err(e) => {
                    failed_tables.push((table_name.clone(), e.to_string()));
                    tracing::warn!(table = %table_name, error = %e, "failed to embed table ");
                }
            }

            tables_processed += 1;
            let percent = ((i + 1) as u32 * 80) / total;
            reporter.report(percent, tables_processed as u32).await;
        }

        // Refresh table-level descriptions
        for table in &tables {
            if let Err(e) = self
                .refresh_table_description(
                    tenant_id,
                    datasource_id,
                    &table.table_name,
                    &table.columns,
                )
                .await
            {
                tracing::warn!(table = %table.table_name, error = %e, "failed to refresh table description ");
            }
            match self
                .refresh_table_embedding(
                    tenant_id,
                    datasource_id,
                    &table.table_name,
                    &table.columns,
                )
                .await
            {
                Ok(Some(u)) => embedding_usage.push(u),
                Ok(None) => {}
                Err(e) => {
                    failed_tables.push((table.table_name.clone(), e.to_string()));
                    tracing::warn!(table = %table.table_name, error = %e, "failed to refresh table embedding");
                }
            }
        }

        if let Some(ref dd) = datasource_description {
            if !dd.trim().is_empty() {
                if let Err(e) = self
                    .refresh_datasource_description(tenant_id, datasource_id, dd)
                    .await
                {
                    tracing::warn!(error = %e, "failed to refresh datasource description ");
                }
            }
        }
        match self
            .refresh_datasource_embedding(
                tenant_id,
                datasource_id,
                datasource_description.as_deref(),
                &tables,
            )
            .await
        {
            Ok(Some(u)) => embedding_usage.push(u),
            Ok(None) => {}
            Err(e) => {
                failed_tables.push(("__datasource__".to_string(), e.to_string()));
                tracing::warn!(datasource_id, error = %e, "failed to refresh datasource embedding");
            }
        }

        reporter.report(95, tables_processed as u32).await;
        self.activate_complete_profiles(tenant_id, datasource_id)
            .await?;
        reporter.report(100, tables_processed as u32).await;

        tracing::info!(
            datasource_id,
            tables_processed,
            columns_processed,
            failed_count = failed_tables.len(),
            "refresh_schema_directly complete"
        );

        Ok(RefreshResult {
            tables_processed,
            columns_processed,
            failed_tables,
            embedding_usage,
        })
    }

    /// Discover the schema of a single table directly from the source DB
    /// using the provided db_type and decrypted config. This bypasses the
    /// nl2sql_semantics_refresh_tasks / schema_info table which may not be
    /// committed yet when called from the discover handler.
    pub async fn discover_single_table_schema(
        &self,
        db_type: &str,
        config: &serde_json::Value,
        table_name: &str,
    ) -> anyhow::Result<Option<TableSchema>> {
        let json = crate::nl2sql::schema_discovery::SchemaDiscovery::new()
            .discover_table(db_type, config, table_name)
            .await
            .map_err(|e| anyhow::anyhow!("discover_table failed: {e} "))?;

        let json = match json {
            Some(v) => v,
            None => return Ok(None),
        };

        let table_name_out = json
            .get("table_name")
            .and_then(|v| v.as_str())
            .unwrap_or(table_name)
            .to_owned();

        let columns: Vec<ColumnSchema> = json
            .get("columns")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|col| {
                        Some(ColumnSchema {
                            name: col.get("name")?.as_str()?.to_owned(),
                            data_type: col.get("type")?.as_str()?.to_owned(),
                            is_nullable: col.get("nullable")?.as_bool()?,
                            sample_values: vec![],
                            ai_description: None,
                            is_indexed: false,
                            comment: col
                                .get("comment")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Some(TableSchema {
            table_name: table_name_out,
            columns,
            ai_description: None,
            is_manual: false,
            embedding_model: None,
            version: 1,
        }))
    }

    pub async fn refresh_datasource_with_progress<R: ProgressReporter>(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        reporter: R,
    ) -> anyhow::Result<RefreshResult> {
        tracing::info!(
            tenant_id,
            datasource_id,
            "SchemaDescriber: starting refresh_datasource_with_progress "
        );

        // 1. Load datasource record
        let row = sqlx::query(
            "SELECT schema_info, db_type, config, description FROM data_sources WHERE id = ?",
        )
        .bind(datasource_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("datasource {datasource_id} not found "))?;

        let schema_json: Option<serde_json::Value> = row.get("schema_info");
        let _db_type: String = row.get("db_type");
        let config_json: serde_json::Value = row
            .get::<Option<serde_json::Value>, _>("config")
            .unwrap_or(serde_json::Value::Null);
        let datasource_description: Option<String> = row.get("description");

        let tables = parse_schema_json(&schema_json);

        // 2. Load existing semantics to detect which columns need (re)generation
        let existing: std::collections::HashMap<(String, String), String> =
            sqlx::query_as::<_, (String, String, String)>(
                "SELECT table_name, column_name, COALESCE(semantic_description, '') \
                 FROM nl2sql_table_semantics WHERE datasource_id = ? AND tenant_id = ? AND deleted_at IS NULL",
            )
            .bind(datasource_id)
            .bind(tenant_id)
            .fetch_all(&self.db)
            .await
            .map(|rows| rows.into_iter().map(|(t, c, sd)| ((t, c), sd)).collect())
            .unwrap_or_default();

        let total = tables.len().max(1) as u32;
        let mut tables_processed = 0usize;
        let mut columns_processed = 0usize;
        let mut failed_tables = Vec::new();
        let mut embedding_usage = Vec::new();
        let total_cols_counter = std::sync::atomic::AtomicUsize::new(0);

        for (i, table) in tables.iter().enumerate() {
            let table_name = &table.table_name;

            // Auto-generated table description
            if let Some(existing_desc) = table.ai_description.as_ref() {
                let _ = sqlx::query(
                    concat!(
                        "INSERT INTO nl2sql_table_desc_semantics ",
                        "(datasource_id, table_name, tenant_id, ai_description, embedding_model, is_manual, version) ",
                        "VALUES (?, ?, ?, ?, ?, 0, 1) ",
                        "ON CONFLICT DO UPDATE SET ai_description = excluded.ai_description, ",
                        "embedding_model = excluded.embedding_model, version = version + 1"
                    ),
                )
                .bind(datasource_id)
                .bind(table_name)
                .bind(tenant_id)
                .bind(existing_desc)
                .bind(&self.embed_model)
                .execute(&self.db)
                .await;
            }

            let mut embed_batch: Vec<(String, String, String)> = Vec::new();

            // Parallel-generate descriptions for columns that need it
            let needs_gen: Vec<&ColumnSchema> = table
                .columns
                .iter()
                .filter(|col| {
                    existing
                        .get(&(table_name.clone(), col.name.clone()))
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                })
                .collect();
            let gen_results: std::collections::HashMap<String, String> = {
                let results = self
                    .generate_descriptions_parallel(
                        table_name,
                        &needs_gen.iter().map(|c| (*c).clone()).collect::<Vec<_>>(),
                    )
                    .await;
                results
                    .into_iter()
                    .filter_map(|(name, r)| r.ok().map(|desc| (name, desc)))
                    .collect()
            };

            for col in &table.columns {
                let col_key = (table_name.clone(), col.name.clone());
                let existing_ai = existing.get(&col_key).cloned().unwrap_or_default();

                let description = if !existing_ai.is_empty() {
                    existing_ai
                } else {
                    let gen = gen_results
                        .get(&col.name)
                        .cloned()
                        .unwrap_or_else(|| col.name.clone());
                    if gen != existing_ai {
                        self.upsert_semantics(
                            tenant_id,
                            datasource_id,
                            table_name,
                            &col.name,
                            &gen,
                            None,
                        )
                        .await?;
                    }
                    gen
                };

                embed_batch.push((
                    table_name.clone(),
                    col.name.clone(),
                    format!(
                        "Table: {}
Column: {}
Description: {}",
                        table_name, col.name, description
                    ),
                ));
                columns_processed += 1;
                total_cols_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }

            match self
                .embed_and_store(tenant_id, datasource_id, &embed_batch)
                .await
            {
                Ok(usage) => {
                    if let Some(u) = usage {
                        embedding_usage.push(u);
                    }
                }
                Err(e) => {
                    failed_tables.push((table_name.clone(), e.to_string()));
                    tracing::warn!(table = %table_name, error = %e, "failed to embed table ");
                }
            }

            tables_processed += 1;
            let percent = ((i + 1) as u32 * 80) / total;
            reporter.report(percent, tables_processed as u32).await;
        }

        // Refresh table-level descriptions
        for table in &tables {
            if let Err(e) = self
                .refresh_table_description(
                    tenant_id,
                    datasource_id,
                    &table.table_name,
                    &table.columns,
                )
                .await
            {
                tracing::warn!(table = %table.table_name, error = %e, "failed to refresh table description ");
            }
            match self
                .refresh_table_embedding(
                    tenant_id,
                    datasource_id,
                    &table.table_name,
                    &table.columns,
                )
                .await
            {
                Ok(Some(u)) => embedding_usage.push(u),
                Ok(None) => {}
                Err(e) => {
                    failed_tables.push((table.table_name.clone(), e.to_string()));
                    tracing::warn!(table = %table.table_name, error = %e, "failed to refresh table embedding");
                }
            }
        }

        // Datasource-level description
        if let Some(ref dd) = datasource_description {
            if !dd.is_empty() {
                if let Err(e) = self
                    .refresh_datasource_description(tenant_id, datasource_id, dd)
                    .await
                {
                    tracing::warn!(error = %e, "failed to refresh datasource description ");
                }
            }
        }
        match self
            .refresh_datasource_embedding(
                tenant_id,
                datasource_id,
                datasource_description.as_deref(),
                &tables,
            )
            .await
        {
            Ok(Some(u)) => embedding_usage.push(u),
            Ok(None) => {}
            Err(e) => {
                failed_tables.push(("__datasource__".to_string(), e.to_string()));
                tracing::warn!(datasource_id, error = %e, "failed to refresh datasource embedding");
            }
        }

        reporter.report(95, tables_processed as u32).await;

        // Collect statistics (if enabled)
        let _ = collect_stats_for_datasource(
            &self.db,
            tenant_id,
            datasource_id,
            "",
            &config_json,
            &tables,
        )
        .await;

        self.activate_complete_profiles(tenant_id, datasource_id)
            .await?;
        reporter.report(100, tables_processed as u32).await;

        tracing::info!(
            datasource_id,
            tables_processed,
            columns_processed,
            failed_count = failed_tables.len(),
            "refresh_datasource_with_progress complete "
        );

        Ok(RefreshResult {
            tables_processed,
            columns_processed,
            failed_tables,
            embedding_usage,
        })
    }

    pub async fn refresh_tables<R: ProgressReporter>(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_names: &[String],
        reporter: R,
    ) -> anyhow::Result<RefreshResult> {
        let row = sqlx::query(
            "SELECT schema_info, db_type, config, description FROM data_sources WHERE id = ?",
        )
        .bind(datasource_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("datasource {datasource_id} not found "))?;

        let schema_json: Option<serde_json::Value> = row.get("schema_info");
        let db_type: String = row.get("db_type");
        let config_json: serde_json::Value = row
            .get::<Option<serde_json::Value>, _>("config")
            .unwrap_or(serde_json::Value::Null);
        let datasource_description: Option<String> = row.get("description");

        let all_tables = parse_schema_json(&schema_json);
        let tables: Vec<_> = all_tables
            .into_iter()
            .filter(|t| table_names.contains(&t.table_name))
            .collect();

        if tables.is_empty() {
            reporter.report(100, 0).await;
            return Ok(RefreshResult {
                tables_processed: 0,
                columns_processed: 0,
                failed_tables: vec![],
                embedding_usage: vec![],
            });
        }

        let total = tables.len();
        let mut tables_processed = 0usize;
        let mut columns_processed = 0usize;
        let mut failed_tables = Vec::new();
        let mut embedding_usage = Vec::new();

        let selected_tables: std::collections::HashSet<String> =
            tables.iter().map(|t| t.table_name.clone()).collect();

        let col_rows: Vec<(String, String, String, bool)> = sqlx::query_as(
            "SELECT table_name, column_name, COALESCE(semantic_description, '') AS semantic_description, is_indexed \
             FROM nl2sql_table_semantics \
             WHERE datasource_id = ? AND tenant_id = ? AND deleted_at IS NULL",
        )
        .bind(datasource_id)
        .bind(tenant_id)
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();
        let mut col_sem_map: std::collections::HashMap<(String, String), (String, bool)> =
            std::collections::HashMap::new();
        for (table_name, col_name, semantic_description, is_indexed) in col_rows {
            if selected_tables.contains(&table_name) {
                col_sem_map.insert((table_name, col_name), (semantic_description, is_indexed));
            }
        }

        let table_desc_rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_name, COALESCE(NULLIF(user_description, ''), NULLIF(ai_description, ''), '') AS description \
             FROM nl2sql_table_desc_semantics \
             WHERE datasource_id = ? AND deleted_at IS NULL",
        )
        .bind(datasource_id)
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();
        let mut table_desc_ready: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        for (table_name, desc) in table_desc_rows {
            if selected_tables.contains(&table_name) {
                table_desc_ready.insert(table_name, !desc.trim().is_empty());
            }
        }

        let profiles = crate::nl2sql::embedding_profiles::resolve_profiles(
            &self.db,
            tenant_id,
            Some("nl2sql"),
        )
        .await?;
        let local_store = self.embed_store.profile_store(
            tenant_id,
            &profiles.local.id,
            &profiles.local.config.model,
            profiles.local.config.base_url.clone(),
        )?;
        let indexed_keys = local_store.indexed_keys(datasource_id).unwrap_or_default();
        let table_embedding_ready: std::collections::HashSet<String> = indexed_keys
            .iter()
            .filter(|(_, c, et)| c == "__table__" && et == "table")
            .map(|(t, _, _)| t.clone())
            .collect();
        let ds_embedding_ready = indexed_keys
            .iter()
            .any(|(t, c, et)| t == "__datasource__" && c == "__datasource__" && et == "datasource");

        let ds_desc_ready: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT COALESCE(NULLIF(user_description, ''), NULLIF(ai_description, ''), '') \
             FROM nl2sql_datasource_semantics \
             WHERE datasource_id = ? AND deleted_at IS NULL \
             LIMIT 1",
        )
        .bind(datasource_id)
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()
        .flatten()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

        let mut any_semantic_changed = false;
        let mut any_embedding_changed = false;

        for (i, table) in tables.iter().enumerate() {
            let table_name = &table.table_name;
            let mut embed_batch: Vec<(String, String, String)> = Vec::new();

            let missing_desc_cols: Vec<&ColumnSchema> = table
                .columns
                .iter()
                .filter(|col| {
                    col_sem_map
                        .get(&(table_name.clone(), col.name.clone()))
                        .map(|(desc, _)| desc.trim().is_empty())
                        .unwrap_or(true)
                })
                .collect();

            let gen_results: std::collections::HashMap<String, String> =
                if missing_desc_cols.is_empty() {
                    std::collections::HashMap::new()
                } else {
                    self.generate_descriptions_parallel(
                        table_name,
                        &missing_desc_cols
                            .iter()
                            .map(|c| (*c).clone())
                            .collect::<Vec<_>>(),
                    )
                    .await
                    .into_iter()
                    .filter_map(|(name, r)| r.ok().map(|desc| (name, desc)))
                    .collect()
                };

            for col in &table.columns {
                let key = (table_name.clone(), col.name.clone());
                let (existing_desc, existing_is_indexed) = col_sem_map
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), false));
                let has_col_embedding = indexed_keys.contains(&(
                    table_name.clone(),
                    col.name.clone(),
                    "col".to_string(),
                ));

                let mut description = existing_desc.clone();
                if description.trim().is_empty() {
                    description = gen_results
                        .get(&col.name)
                        .cloned()
                        .unwrap_or_else(|| col.name.clone());
                    self.upsert_semantics(
                        tenant_id,
                        datasource_id,
                        table_name,
                        &col.name,
                        &description,
                        None,
                    )
                    .await?;
                    any_semantic_changed = true;
                    col_sem_map.insert(key.clone(), (description.clone(), true));
                }

                let needs_col_embedding = !has_col_embedding || !existing_is_indexed;
                if needs_col_embedding {
                    embed_batch.push((
                        table_name.clone(),
                        col.name.clone(),
                        format!(
                            "Table: {}\nColumn: {}\nDescription: {}",
                            table_name, col.name, description
                        ),
                    ));
                    columns_processed += 1;
                }
            }

            if !embed_batch.is_empty() {
                match self
                    .embed_and_store(tenant_id, datasource_id, &embed_batch)
                    .await
                {
                    Ok(usage) => {
                        if let Some(u) = usage {
                            embedding_usage.push(u);
                        }
                        any_embedding_changed = true;
                    }
                    Err(e) => failed_tables.push((table_name.clone(), e.to_string())),
                }
            }

            let table_has_desc = table_desc_ready.get(table_name).copied().unwrap_or(false);
            let table_has_embedding = table_embedding_ready.contains(table_name);
            let needs_table_desc = !table_has_desc;
            if needs_table_desc {
                if let Err(e) = self
                    .refresh_table_description(
                        tenant_id,
                        datasource_id,
                        &table.table_name,
                        &table.columns,
                    )
                    .await
                {
                    failed_tables.push((table_name.clone(), e.to_string()));
                } else {
                    any_semantic_changed = true;
                }
            }

            let needs_table_embedding =
                !table_has_embedding || !embed_batch.is_empty() || needs_table_desc;
            if needs_table_embedding {
                match self
                    .refresh_table_embedding(
                        tenant_id,
                        datasource_id,
                        &table.table_name,
                        &table.columns,
                    )
                    .await
                {
                    Ok(Some(u)) => {
                        embedding_usage.push(u);
                        any_embedding_changed = true;
                    }
                    Ok(None) => {}
                    Err(e) => failed_tables.push((table_name.clone(), e.to_string())),
                }
            }

            tables_processed += 1;
            let percent = ((i + 1) as u32 * 90) / total as u32;
            reporter.report(percent, tables_processed as u32).await;
        }

        let needs_ds_desc = !ds_desc_ready
            && datasource_description
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
        if needs_ds_desc {
            if let Some(ref dd) = datasource_description {
                if let Err(e) = self
                    .refresh_datasource_description(tenant_id, datasource_id, dd)
                    .await
                {
                    tracing::warn!(error = %e, "failed to refresh datasource description");
                } else {
                    any_semantic_changed = true;
                }
            }
        }

        let needs_ds_embedding =
            !ds_embedding_ready || any_embedding_changed || any_semantic_changed || needs_ds_desc;
        if needs_ds_embedding {
            if let Some(u) = self
                .refresh_datasource_embedding(
                    tenant_id,
                    datasource_id,
                    datasource_description.as_deref(),
                    &tables,
                )
                .await?
            {
                embedding_usage.push(u);
            }
        }

        let _ = collect_stats_for_datasource(
            &self.db,
            tenant_id,
            datasource_id,
            &db_type,
            &config_json,
            &tables,
        )
        .await;

        self.activate_complete_profiles(tenant_id, datasource_id)
            .await?;
        reporter.report(100, tables_processed as u32).await;
        Ok(RefreshResult {
            tables_processed,
            columns_processed,
            failed_tables,
            embedding_usage,
        })
    }

    pub async fn update_user_description(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
        user_description: &str,
    ) -> Result<UpdateResult, UpdateDescriptionError> {
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT semantic_description FROM nl2sql_table_semantics \
             WHERE datasource_id = ? AND tenant_id = ? AND table_name = ? AND column_name = ? AND deleted_at IS NULL",
        )
        .bind(datasource_id)
        .bind(tenant_id)
        .bind(&table_name)
        .bind(column_name)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| UpdateDescriptionError::UpdateFailed(e.to_string()))?;

        existing.ok_or(UpdateDescriptionError::NotFound)?;

        // Polish with LLM (best-effort)
        let polished = self.polish_description(user_description).await;
        let final_desc = if polished.is_empty() {
            user_description.to_string()
        } else {
            polished
        };

        // Write to semantic_description
        sqlx::query(
            "UPDATE nl2sql_table_semantics SET semantic_description = ? \
             WHERE datasource_id = ? AND tenant_id = ? AND table_name = ? AND column_name = ?",
        )
        .bind(&final_desc)
        .bind(datasource_id)
        .bind(tenant_id)
        .bind(table_name)
        .bind(column_name)
        .execute(&self.db)
        .await
        .map_err(|e| UpdateDescriptionError::UpdateFailed(e.to_string()))?;

        // Re-embed
        let text = format!("Table: {table_name}\nColumn: {column_name}\nDescription: {final_desc}");
        match self
            .embed_single_column(tenant_id, datasource_id, table_name, column_name, &text)
            .await
        {
            Ok(_) => Ok(UpdateResult {
                updated: true,
                indexed: true,
                index_error: None,
            }),
            Err(e) => Ok(UpdateResult {
                updated: true,
                indexed: false,
                index_error: Some(e.to_string()),
            }),
        }
    }

    pub async fn update_table_description(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        user_description: &str,
    ) -> Result<UpdateResult, UpdateDescriptionError> {
        // Polish with LLM (best-effort)
        let polished = self.polish_description(user_description).await;
        let final_desc = if polished.is_empty() {
            user_description.to_string()
        } else {
            polished
        };

        let rows_affected = sqlx::query(
            "UPDATE nl2sql_table_desc_semantics SET ai_description = ?, embedding_model = ? \
             WHERE datasource_id = ? AND table_name = ?",
        )
        .bind(&final_desc)
        .bind(&self.embed_model)
        .bind(datasource_id)
        .bind(table_name)
        .execute(&self.db)
        .await
        .map_err(|e| UpdateDescriptionError::UpdateFailed(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            return Err(UpdateDescriptionError::NotFound);
        }

        match self
            .refresh_table_column_embeddings(tenant_id, datasource_id, table_name, &final_desc)
            .await
        {
            Ok(_) => Ok(UpdateResult {
                updated: true,
                indexed: true,
                index_error: None,
            }),
            Err(e) => Ok(UpdateResult {
                updated: true,
                indexed: false,
                index_error: Some(e.to_string()),
            }),
        }
    }

    pub async fn update_datasource_description(
        &self,
        datasource_id: &str,
        user_description: &str,
    ) -> Result<UpdateResult, UpdateDescriptionError> {
        let tenant_id: Option<String> =
            sqlx::query_scalar("SELECT tenant_id FROM data_sources WHERE id = ? LIMIT 1")
                .bind(datasource_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| UpdateDescriptionError::UpdateFailed(e.to_string()))?;
        let tenant_id = tenant_id.ok_or_else(|| {
            UpdateDescriptionError::UpdateFailed("datasource not found".to_string())
        })?;
        let polished = self.polish_description(user_description).await;
        let final_desc = if polished.is_empty() {
            user_description.to_string()
        } else {
            polished
        };

        let rows_affected = sqlx::query(
            "UPDATE nl2sql_datasource_semantics SET user_description = ?, embedding_model = ? WHERE datasource_id = ?",
        )
        .bind(&final_desc)
        .bind(&self.embed_model)
        .bind(datasource_id)
        .execute(&self.db)
        .await
        .map_err(|e| UpdateDescriptionError::UpdateFailed(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            sqlx::query(
                "INSERT INTO nl2sql_datasource_semantics \
                 (tenant_id, datasource_id, user_description, embedding_model) VALUES (?, ?, ?, ?)",
            )
            .bind(&tenant_id)
            .bind(datasource_id)
            .bind(&final_desc)
            .bind(&self.embed_model)
            .execute(&self.db)
            .await
            .map_err(|e| UpdateDescriptionError::UpdateFailed(e.to_string()))?;
        }

        let tables: Vec<TableSchema> = self
            .get_table_semantics(datasource_id)
            .await
            .map_err(|e| UpdateDescriptionError::Database(e.to_string()))?;
        if let Err(e) = self
            .refresh_datasource_embedding(&tenant_id, datasource_id, Some(&final_desc), &tables)
            .await
        {
            tracing::warn!(
                datasource_id = %datasource_id,
                error = %e,
                "failed to refresh datasource embedding after update"
            );
            return Ok(UpdateResult {
                updated: true,
                indexed: false,
                index_error: Some(e.to_string()),
            });
        }

        Ok(UpdateResult {
            updated: true,
            indexed: true,
            index_error: None,
        })
    }

    pub async fn get_datasource_semantics(
        &self,
        datasource_id: &str,
    ) -> anyhow::Result<Option<DatasourceSemantics>> {
        let ds_row: Option<(String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT ds.id, dsd.user_description, COALESCE(CAST(dsd.version AS INTEGER), 0) \
             FROM data_sources ds \
             LEFT JOIN nl2sql_datasource_semantics dsd ON dsd.datasource_id = ds.id \
             WHERE ds.id = ?",
        )
        .bind(datasource_id)
        .fetch_optional(&self.db)
        .await?;

        let (datasource_id_out, user_description_out, embedding_version) =
            ds_row.unwrap_or((datasource_id.to_string(), None, None));
        let embedding_version = i32::try_from(embedding_version.unwrap_or(0)).unwrap_or(i32::MAX);

        // Load table semantics
        let table_rows: Vec<(String, Option<String>, bool, Option<i64>)> =
            sqlx::query_as(
                "SELECT tds.table_name, tds.ai_description, tds.is_manual, CAST(tds.version AS INTEGER) \
                 FROM nl2sql_table_desc_semantics tds WHERE tds.datasource_id = ? AND tds.deleted_at IS NULL",
            )
            .bind(datasource_id)
            .fetch_all(&self.db)
            .await?;

        if table_rows.is_empty() {
            return Ok(None);
        }

        // Load column semantics grouped by table
        let col_rows: Vec<(String, String, Option<String>, bool)> = sqlx::query_as(
            "SELECT table_name, column_name, semantic_description, is_indexed \
             FROM nl2sql_table_semantics WHERE datasource_id = ? AND deleted_at IS NULL ORDER BY table_name, column_name",
        )
        .bind(datasource_id)
        .fetch_all(&self.db)
        .await?;

        let mut col_map: std::collections::HashMap<String, Vec<ColumnSchema>> =
            std::collections::HashMap::new();
        for (t, c, sd, is_indexed) in col_rows {
            let sd = sd.unwrap_or_default();
            if !sd.is_empty() {
                col_map.entry(t).or_default().push(ColumnSchema {
                    name: c,
                    data_type: String::new(),
                    is_nullable: false,
                    sample_values: Vec::new(),
                    ai_description: Some(sd),
                    is_indexed,
                    comment: None,
                });
            }
        }

        let tables: Vec<TableSchema> = table_rows
            .into_iter()
            .map(|(name, ai_desc, is_manual, _version)| {
                let cols = col_map.remove(&name).unwrap_or_default();
                TableSchema {
                    table_name: name,
                    columns: cols,
                    ai_description: ai_desc,
                    is_manual,
                    embedding_model: None,
                    version: 0,
                }
            })
            .collect();

        Ok(Some(DatasourceSemantics {
            datasource_id: datasource_id_out,
            datasource_description: String::new(),
            tables,
            embedding_version,
            ai_description: None,
            user_description: user_description_out,
            embedding_model: None,
            is_indexed: false,
            version: 0,
        }))
    }

    pub async fn get_table_semantics(
        &self,
        datasource_id: &str,
    ) -> anyhow::Result<Vec<TableSchema>> {
        let table_rows: Vec<(String, Option<String>, bool, Option<i64>)> =
            sqlx::query_as(
                "SELECT tds.table_name, tds.ai_description, tds.is_manual, CAST(tds.version AS INTEGER) \
                 FROM nl2sql_table_desc_semantics tds WHERE tds.datasource_id = ? AND tds.deleted_at IS NULL",
            )
            .bind(datasource_id)
            .fetch_all(&self.db)
            .await?;

        let col_rows: Vec<(String, String, Option<String>, bool)> = sqlx::query_as(
            "SELECT table_name, column_name, semantic_description, is_indexed \
             FROM nl2sql_table_semantics WHERE datasource_id = ? AND deleted_at IS NULL ORDER BY table_name, column_name",
        )
        .bind(datasource_id)
        .fetch_all(&self.db)
        .await?;

        let mut col_map: std::collections::HashMap<String, Vec<ColumnSchema>> =
            std::collections::HashMap::new();
        for (t, c, sd, is_indexed) in col_rows {
            let sd = sd.unwrap_or_default();
            if !sd.is_empty() {
                col_map.entry(t).or_default().push(ColumnSchema {
                    name: c,
                    data_type: String::new(),
                    is_nullable: false,
                    sample_values: Vec::new(),
                    ai_description: Some(sd),
                    is_indexed,
                    comment: None,
                });
            }
        }

        Ok(table_rows
            .into_iter()
            .map(|(name, ai_desc, is_manual, _version)| {
                let cols = col_map.remove(&name).unwrap_or_default();
                TableSchema {
                    table_name: name,
                    columns: cols,
                    ai_description: ai_desc,
                    is_manual,
                    embedding_model: None,
                    version: 0,
                }
            })
            .collect())
    }

    pub async fn get_column_semantics(
        &self,
        datasource_id: &str,
    ) -> anyhow::Result<Vec<ColumnSemanticsResult>> {
        let rows: Vec<(String, String, Option<String>, bool, i64)> = sqlx::query_as(
            "SELECT table_name, column_name, semantic_description, is_indexed, COALESCE(CAST(version AS INTEGER), 0) \
             FROM nl2sql_table_semantics WHERE datasource_id = ? AND deleted_at IS NULL ORDER BY table_name, column_name",
        )
        .bind(datasource_id)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(table_name, column_name, semantic_description, is_indexed, version)| {
                    ColumnSemanticsResult {
                        table_name,
                        column_name,
                        semantic_description: semantic_description.unwrap_or_default(),
                        is_indexed,
                        version: i32::try_from(version).unwrap_or(i32::MAX),
                    }
                },
            )
            .collect())
    }

    // ── Private helpers ─────────────────────────────────────────────────────

    /// Generate descriptions for all columns in parallel, bounded to 8 simultaneous LLM calls.
    async fn generate_descriptions_parallel(
        &self,
        table_name: &str,
        columns: &[ColumnSchema],
    ) -> Vec<(String, anyhow::Result<String>)> {
        use futures_util::stream::{FuturesOrdered, StreamExt};
        const CONCURRENCY: usize = 8;
        let sem = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
        let mut futs = FuturesOrdered::new();
        for col in columns {
            let sem = Arc::clone(&sem);
            futs.push_back(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                (
                    col.name.clone(),
                    self.generate_column_description(table_name, col).await,
                )
            });
        }
        futs.collect().await
    }

    async fn generate_column_description(
        &self,
        table_name: &str,
        col: &ColumnSchema,
    ) -> anyhow::Result<String> {
        let chat_cfg = self.chat_cfg.as_ref().ok_or_else(|| {
            anyhow::anyhow!("chat config not available - set embedding/chat API keys ")
        })?;

        let samples_str = if col.sample_values.is_empty() {
            String::new()
        } else {
            format!(
                " | samples: {}",
                col.sample_values
                    .iter()
                    .take(5)
                    .map(|s| format!(r#""{}""#, s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let comment_hint = col
            .comment
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(|c| format!(" | db_comment: {c}"))
            .unwrap_or_default();

        let prompt = format!(
            r#"You are a data analyst. For the column "{table_name}.{col_name}" ({data_type}{nullable}), generate a brief semantic description (1-2 sentences max) explaining what data this column holds.

Output only the description text - no preamble, no markdown, no quotes.
Column definition: {table_name}.{col_name}: {data_type}{nullable}{comment_hint}{samples_str}"#,
            table_name = table_name,
            col_name = col.name,
            data_type = col.data_type,
            nullable = if col.is_nullable { " (nullable)" } else { "" },
            comment_hint = comment_hint,
            samples_str = samples_str,
        );

        let request = MessageRequest {
            model: chat_cfg.model.clone(),
            max_tokens: 8192,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text { text: prompt }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.3),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            chat_cfg.client.send_message(&request),
        )
        .await
        .map_err(|_| anyhow::anyhow!("LLM call timed out after 120s"))?
        .map_err(|e| anyhow::anyhow!("LLM call failed: {e}"))?;

        tracing::info!(
            table = %table_name,
            col = %col.name,
            model = %chat_cfg.model,
            content_blocks = response.content.len(),
            raw = ?response.content,
            "generate_column_description: LLM response"
        );

        let text = response
            .content
            .iter()
            .find_map(|block| match block {
                OutputContentBlock::Text { text } => {
                    let t = text.trim().to_string();
                    if t.is_empty() { None } else { Some(t) }
                }
                _ => None,
            })
            .or_else(|| {
                // Thinking-only models (e.g. GLM-Z1) may return only a Thinking block
                // when max_tokens is exhausted before the text output. Extract the last
                // non-empty sentence from the thinking content as a best-effort description.
                response.content.iter().find_map(|block| match block {
                    OutputContentBlock::Thinking { thinking, .. } => {
                        thinking.lines()
                            .rev()
                            .map(|l| l.trim())
                            .find(|l| !l.is_empty() && l.len() > 10)
                            .map(|l| l.trim_matches(|c| c == '*' || c == '#').trim().to_string())
                    }
                    _ => None,
                })
            })
            .unwrap_or_else(|| {
                tracing::warn!(table = %table_name, col = %col.name, "no usable content in LLM response, falling back to col name");
                col.name.clone()
            });

        Ok(text)
    }

    async fn polish_description(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let Some(ref chat_cfg) = self.chat_cfg else {
            return String::new();
        };
        let prompt = format!(
            "Rewrite the following column/table description in clear, concise English (1-2 sentences max). \
             Output only the rewritten text, no preamble, no quotes.\n\n{text}"
        );
        let req = MessageRequest {
            model: chat_cfg.model.clone(),
            max_tokens: 4_096.min(chat_cfg.max_output_tokens).max(1),
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text { text: prompt }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.2),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            chat_cfg.client.send_message(&req),
        )
        .await
        {
            Ok(Ok(resp)) => resp
                .content
                .iter()
                .find_map(|b| match b {
                    OutputContentBlock::Text { text } => {
                        let t = text.trim().to_string();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t)
                        }
                    }
                    _ => None,
                })
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    async fn upsert_semantics(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
        description: &str,
        _existing_ai: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            concat!(
                "INSERT INTO nl2sql_table_semantics ",
                "(datasource_id, tenant_id, table_name, column_name, semantic_description, embedding_model) ",
                "VALUES (?, ?, ?, ?, ?, ?) ",
                "ON CONFLICT DO UPDATE SET semantic_description = excluded.semantic_description, ",
                "embedding_model = excluded.embedding_model"
            ),
        )
        .bind(datasource_id)
        .bind(tenant_id)
        .bind(table_name)
        .bind(column_name)
        .bind(description)
        .bind(&self.embed_model)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn embed_and_store(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        batch: &[(String, String, String)],
    ) -> anyhow::Result<Option<api::Usage>> {
        if batch.is_empty() {
            return Ok(None);
        }

        let usage = self
            .embed_and_store_typed(tenant_id, datasource_id, "col", batch)
            .await?;

        // Mark all columns in this batch as indexed after successful vector storage
        let cols: Vec<(String, String)> = batch
            .iter()
            .map(|(t, c, _)| (t.clone(), c.clone()))
            .collect();
        let set_clause = cols
            .iter()
            .enumerate()
            .map(|(_i, _)| format!("WHEN table_name = ? AND column_name = ? THEN 1"))
            .collect::<Vec<_>>()
            .join(" ");
        let when_clauses = cols
            .iter()
            .flat_map(|(t, c)| [t.clone(), c.clone()])
            .collect::<Vec<_>>();
        let where_clause = cols
            .iter()
            .map(|_| "(table_name = ? AND column_name = ?)")
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            concat!(
                "UPDATE nl2sql_table_semantics ",
                "SET is_indexed = CASE {set_clause} ELSE is_indexed END ",
                "WHERE datasource_id = ? AND tenant_id = ? AND ({where_clause})"
            ),
            set_clause = set_clause,
            where_clause = where_clause,
        );
        // CASE and WHERE fragments contain placeholders only; all values remain bound.
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for w in &when_clauses {
            q = q.bind(w);
        }
        q = q.bind(datasource_id).bind(tenant_id);
        for (t, c) in &cols {
            q = q.bind(t).bind(c);
        }
        q.execute(&self.db).await?;

        if let (Some(sink), Some(u)) = (&self.usage_sink, usage.clone()) {
            sink(u);
        }

        Ok(usage)
    }

    async fn embed_and_store_typed(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        embed_type: &str,
        batch: &[(String, String, String)],
    ) -> anyhow::Result<Option<api::Usage>> {
        if batch.is_empty() {
            return Ok(None);
        }

        let profiles = crate::nl2sql::embedding_profiles::resolve_profiles(
            &self.db,
            tenant_id,
            Some("nl2sql"),
        )
        .await?;
        crate::nl2sql::embedding_profiles::ensure_datasource_profiles(
            &self.db,
            tenant_id,
            datasource_id,
            &profiles,
        )
        .await?;
        let texts: Vec<String> = batch.iter().map(|(_, _, text)| text.clone()).collect();

        // The bundled local profile is the durable baseline and must succeed.
        let local_model = EmbeddingModel::new_with_dimensions(
            &profiles.local.config.model,
            profiles.local.config.base_url.clone(),
            None,
            profiles.local.config.dimensions,
        );
        let (local_vectors, _) = local_model.embed_batch_with_usage(&texts).await?;
        validate_embedding_batch(
            &local_vectors,
            batch.len(),
            profiles.local.config.effective_dimensions(),
            "local",
        )?;
        let local_store = self.embed_store.profile_store(
            tenant_id,
            &profiles.local.id,
            &profiles.local.config.model,
            profiles.local.config.base_url.clone(),
        )?;
        for (index, (table_name, column_name, _)) in batch.iter().enumerate() {
            local_store.upsert_typed(
                datasource_id,
                table_name,
                column_name,
                embed_type,
                &local_vectors[index],
                &profiles.local.config.model,
            )?;
        }
        crate::nl2sql::embedding_profiles::record_profile_success(&self.db, &profiles.local.id)
            .await?;

        // API indexing is best-effort. A failure never discards the local
        // vectors; the reindex worker will backfill the API profile later.
        let mut usage = None;
        if let Some(api_profile) = &profiles.api {
            let circuit_allows = crate::nl2sql::embedding_profiles::circuit_allows_request(
                &self.db,
                &api_profile.id,
            )
            .await
            .unwrap_or(true);
            if circuit_allows {
                let api_model = EmbeddingModel::new_with_dimensions(
                    &api_profile.config.model,
                    api_profile.config.base_url.clone(),
                    Some(api_profile.config.api_key.clone()),
                    api_profile.config.dimensions,
                );
                let api_result =
                    api_model
                        .embed_batch_with_usage(&texts)
                        .await
                        .and_then(|(vectors, usage)| {
                            validate_embedding_batch(
                                &vectors,
                                batch.len(),
                                api_profile.config.effective_dimensions(),
                                "API",
                            )?;
                            Ok((vectors, usage))
                        });
                match api_result {
                    Ok((api_vectors, api_usage)) => {
                        let api_store = self.embed_store.profile_store(
                            tenant_id,
                            &api_profile.id,
                            &api_profile.config.model,
                            api_profile.config.base_url.clone(),
                        )?;
                        for (index, (table_name, column_name, _)) in batch.iter().enumerate() {
                            api_store.upsert_typed(
                                datasource_id,
                                table_name,
                                column_name,
                                embed_type,
                                &api_vectors[index],
                                &api_profile.config.model,
                            )?;
                        }
                        crate::nl2sql::embedding_profiles::record_profile_success(
                            &self.db,
                            &api_profile.id,
                        )
                        .await?;
                        let _ =
                            crate::nl2sql::embedding_failover::resolve_embedding_fallback_alert(
                                &self.db,
                                tenant_id,
                                "nl2sql",
                                &api_profile.config,
                            )
                            .await;
                        usage = api_usage;
                    }
                    Err(error) => {
                        crate::nl2sql::embedding_profiles::record_profile_failure(
                            &self.db,
                            &api_profile.id,
                            &error.to_string(),
                        )
                        .await?;
                        crate::nl2sql::embedding_profiles::enqueue_reindex(
                            &self.db,
                            tenant_id,
                            datasource_id,
                            api_profile,
                        )
                        .await?;
                        let _ = crate::nl2sql::embedding_failover::record_embedding_fallback_alert(
                            &self.db,
                            tenant_id,
                            "nl2sql",
                            &api_profile.config,
                            &error.to_string(),
                        )
                        .await;
                        tracing::warn!(
                            tenant_id,
                            datasource_id,
                            profile_id = %api_profile.id,
                            error = %error,
                            "API embedding failed; local profile stored and API backfill queued"
                        );
                    }
                }
            } else {
                crate::nl2sql::embedding_profiles::enqueue_reindex(
                    &self.db,
                    tenant_id,
                    datasource_id,
                    api_profile,
                )
                .await?;
            }
        }

        if let (Some(sink), Some(u)) = (&self.usage_sink, usage.clone()) {
            sink(u);
        }
        Ok(usage)
    }

    async fn activate_complete_profiles(
        &self,
        tenant_id: &str,
        datasource_id: &str,
    ) -> anyhow::Result<()> {
        let profiles = crate::nl2sql::embedding_profiles::resolve_profiles(
            &self.db,
            tenant_id,
            Some("nl2sql"),
        )
        .await?;
        crate::nl2sql::embedding_profiles::ensure_datasource_profiles(
            &self.db,
            tenant_id,
            datasource_id,
            &profiles,
        )
        .await?;

        let local_store = self.embed_store.profile_store(
            tenant_id,
            &profiles.local.id,
            &profiles.local.config.model,
            profiles.local.config.base_url.clone(),
        )?;
        let local_count = local_store.indexed_keys(datasource_id)?.len();
        crate::nl2sql::embedding_profiles::activate_profile(
            &self.db,
            tenant_id,
            datasource_id,
            &profiles.local,
            local_count,
            local_count,
        )
        .await?;

        if let Some(api_profile) = &profiles.api {
            let api_store = self.embed_store.profile_store(
                tenant_id,
                &api_profile.id,
                &api_profile.config.model,
                api_profile.config.base_url.clone(),
            )?;
            let api_count = api_store.indexed_keys(datasource_id)?.len();
            if api_count == local_count {
                crate::nl2sql::embedding_profiles::activate_profile(
                    &self.db,
                    tenant_id,
                    datasource_id,
                    api_profile,
                    api_count,
                    local_count,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn embed_single_column(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        column_name: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let usage = self
            .embed_and_store_typed(
                tenant_id,
                datasource_id,
                "col",
                &[(
                    table_name.to_string(),
                    column_name.to_string(),
                    text.to_string(),
                )],
            )
            .await?;
        sqlx::query(concat!(
            "UPDATE nl2sql_table_semantics SET is_indexed = 1 ",
            "WHERE datasource_id = ? AND tenant_id = ? AND table_name = ? AND column_name = ?"
        ))
        .bind(datasource_id)
        .bind(tenant_id)
        .bind(table_name)
        .bind(column_name)
        .execute(&self.db)
        .await?;
        if let (Some(sink), Some(u)) = (&self.usage_sink, usage) {
            sink(u);
        }
        Ok(())
    }

    async fn refresh_table_description(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        columns: &[ColumnSchema],
    ) -> anyhow::Result<()> {
        let col_list = columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{}. {}: {} {}",
                    i + 1,
                    c.name,
                    c.data_type,
                    if c.is_nullable { "NULL" } else { "NOT NULL" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "You are a data analyst. For the table \\\"{table_name}\\\", generate a brief semantic description (1-2 sentences max) explaining what this table represents.\n\nColumns:\n{col_list}\n\nOutput only the description text - no quotes, no markdown.",
            table_name = table_name,
            col_list = col_list,
        );

        let chat_cfg = match self.chat_cfg.as_ref() {
            Some(cfg) => cfg,
            None => return Ok(()),
        };

        let request = MessageRequest {
            model: chat_cfg.model.clone(),
            max_tokens: 8192,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text { text: prompt }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.3),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };

        let response = chat_cfg.client.send_message(&request).await?;
        let text = response
            .content
            .iter()
            .find_map(|b| match b {
                OutputContentBlock::Text { text } => Some(text.trim().to_string()),
                _ => None,
            })
            .unwrap_or_default();

        sqlx::query(
            concat!(
                "INSERT INTO nl2sql_table_desc_semantics ",
                "(datasource_id, table_name, tenant_id, ai_description, embedding_model, is_manual, version) ",
                "VALUES (?, ?, ?, ?, ?, 0, 1) ",
                "ON CONFLICT DO UPDATE SET ai_description = excluded.ai_description, ",
                "embedding_model = excluded.embedding_model, version = version + 1"
            ),
        )
        .bind(datasource_id)
        .bind(table_name)
        .bind(tenant_id)
        .bind(&text)
        .bind(&self.embed_model)
        .execute(&self.db)
        .await?;

        Ok(())
    }

    async fn refresh_table_column_embeddings(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        _user_description: &str,
    ) -> anyhow::Result<()> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name, COALESCE(semantic_description, '') \
             FROM nl2sql_table_semantics WHERE datasource_id = ? AND table_name = ? AND deleted_at IS NULL",
        )
        .bind(datasource_id)
        .bind(table_name)
        .fetch_all(&self.db)
        .await?;

        for (col_name, desc) in rows {
            if desc.is_empty() {
                continue;
            }
            let text = format!(
                "Table: {}
Column: {}
Description: {}",
                table_name, col_name, desc
            );
            self.embed_single_column(tenant_id, datasource_id, table_name, &col_name, &text)
                .await?;
        }

        Ok(())
    }

    async fn refresh_datasource_description(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        user_description: &str,
    ) -> anyhow::Result<()> {
        let prompt = format!(
            "You are a data analyst. This database contains tables for: {}.\n\nGenerate a single-sentence description of what business domain or use case this database serves.\n\nOutput only the description - no quotes, no markdown.",
            user_description,
        );

        let chat_cfg = match self.chat_cfg.as_ref() {
            Some(cfg) => cfg,
            None => return Ok(()),
        };

        let request = MessageRequest {
            model: chat_cfg.model.clone(),
            max_tokens: 128,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text { text: prompt }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.3),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };

        let response = chat_cfg.client.send_message(&request).await?;
        let text = response
            .content
            .iter()
            .find_map(|b| match b {
                OutputContentBlock::Text { text } => Some(text.trim().to_string()),
                _ => None,
            })
            .unwrap_or_else(|| user_description.to_string());

        sqlx::query(concat!(
            "INSERT INTO nl2sql_datasource_semantics ",
            "(datasource_id, tenant_id, ai_description, embedding_model, version) ",
            "VALUES (?, ?, ?, ?, 1) ",
            "ON CONFLICT DO UPDATE SET ai_description = excluded.ai_description, ",
            "embedding_model = excluded.embedding_model, version = version + 1"
        ))
        .bind(datasource_id)
        .bind(tenant_id)
        .bind(&text)
        .bind(&self.embed_model)
        .execute(&self.db)
        .await?;

        Ok(())
    }

    async fn refresh_table_embedding(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        table_name: &str,
        columns: &[ColumnSchema],
    ) -> anyhow::Result<Option<api::Usage>> {
        let desc_row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT user_description, ai_description \
             FROM nl2sql_table_desc_semantics \
             WHERE datasource_id = ? AND table_name = ? AND deleted_at IS NULL \
             LIMIT 1",
        )
        .bind(datasource_id)
        .bind(table_name)
        .fetch_optional(&self.db)
        .await?;

        let preferred_desc = desc_row.and_then(|(user_desc, ai_desc)| {
            user_desc
                .filter(|s| !s.trim().is_empty())
                .or_else(|| ai_desc.filter(|s| !s.trim().is_empty()))
        });
        let text = Self::build_table_embedding_text(table_name, preferred_desc.as_deref(), columns);
        let batch = vec![(table_name.to_string(), "__table__".to_string(), text)];
        self.embed_and_store_typed(tenant_id, datasource_id, "table", &batch)
            .await
    }

    async fn refresh_datasource_embedding(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        fallback_description: Option<&str>,
        fallback_tables: &[TableSchema],
    ) -> anyhow::Result<Option<api::Usage>> {
        let ds_meta: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT d.name, dsd.user_description, dsd.ai_description \
             FROM data_sources d \
             LEFT JOIN nl2sql_datasource_semantics dsd ON dsd.datasource_id = d.id \
             WHERE d.id = ? \
             LIMIT 1",
        )
        .bind(datasource_id)
        .fetch_optional(&self.db)
        .await?;

        let (ds_name, user_desc, ai_desc) = ds_meta.unwrap_or((None, None, None));
        let ds_name = ds_name.unwrap_or_else(|| datasource_id.to_string());
        let description = user_desc
            .filter(|s| !s.trim().is_empty())
            .or_else(|| ai_desc.filter(|s| !s.trim().is_empty()))
            .or_else(|| {
                fallback_description
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            });

        let mut table_summaries: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_name, COALESCE(NULLIF(user_description, ''), NULLIF(ai_description, ''), '') AS table_desc \
             FROM nl2sql_table_desc_semantics \
             WHERE datasource_id = ? AND deleted_at IS NULL \
             ORDER BY table_name",
        )
        .bind(datasource_id)
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();
        table_summaries.retain(|(_, d)| !d.trim().is_empty());

        let text = Self::build_datasource_embedding_text(
            &ds_name,
            description.as_deref(),
            &table_summaries,
            fallback_tables,
        );
        let batch = vec![(
            "__datasource__".to_string(),
            "__datasource__".to_string(),
            text,
        )];
        self.embed_and_store_typed(tenant_id, datasource_id, "datasource", &batch)
            .await
    }

    fn build_table_embedding_text(
        table_name: &str,
        description: Option<&str>,
        columns: &[ColumnSchema],
    ) -> String {
        let mut out = String::new();
        out.push_str("Table: ");
        out.push_str(table_name);
        out.push('\n');
        if let Some(desc) = description {
            out.push_str("Description: ");
            out.push_str(desc.trim());
            out.push('\n');
        }
        if !columns.is_empty() {
            out.push_str("Columns:\n");
            for col in columns.iter().take(80) {
                out.push_str("- ");
                out.push_str(&col.name);
                out.push_str(" (");
                out.push_str(&col.data_type);
                out.push(')');
                if let Some(ai_desc) = col.ai_description.as_ref().filter(|s| !s.trim().is_empty())
                {
                    out.push_str(": ");
                    out.push_str(ai_desc.trim());
                }
                out.push('\n');
            }
        }
        out
    }

    fn build_datasource_embedding_text(
        datasource_name: &str,
        description: Option<&str>,
        table_summaries: &[(String, String)],
        fallback_tables: &[TableSchema],
    ) -> String {
        let mut out = String::new();
        out.push_str("Datasource: ");
        out.push_str(datasource_name);
        out.push('\n');
        if let Some(desc) = description {
            out.push_str("Description: ");
            out.push_str(desc.trim());
            out.push('\n');
        }

        if !table_summaries.is_empty() {
            out.push_str("Tables:\n");
            for (table, table_desc) in table_summaries.iter().take(60) {
                out.push_str("- ");
                out.push_str(table);
                out.push_str(": ");
                out.push_str(table_desc.trim());
                out.push('\n');
            }
        } else if !fallback_tables.is_empty() {
            out.push_str("Tables:\n");
            for table in fallback_tables.iter().take(60) {
                out.push_str("- ");
                out.push_str(&table.table_name);
                if let Some(desc) = table
                    .ai_description
                    .as_ref()
                    .filter(|s| !s.trim().is_empty())
                {
                    out.push_str(": ");
                    out.push_str(desc.trim());
                }
                out.push('\n');
            }
        }
        out
    }
}

fn validate_embedding_batch(
    vectors: &[Vec<f32>],
    expected_count: usize,
    expected_dimensions: usize,
    label: &str,
) -> anyhow::Result<()> {
    if vectors.len() != expected_count {
        anyhow::bail!(
            "{label} embedding returned {} vectors for {expected_count} inputs",
            vectors.len()
        );
    }
    if let Some(vector) = vectors
        .iter()
        .find(|vector| vector.len() != expected_dimensions)
    {
        anyhow::bail!(
            "{label} embedding returned {} dimensions; expected {expected_dimensions}",
            vector.len()
        );
    }
    Ok(())
}

/// Rebuild one immutable profile from descriptions already stored in AOS.
/// This path never calls the chat LLM and is used by the shadow-index worker.
pub async fn rebuild_profile_from_existing_semantics(
    db: &SqlitePool,
    registry: &EmbeddingStoreRegistry,
    tenant_id: &str,
    datasource_id: &str,
    profile: &crate::nl2sql::embedding_profiles::ResolvedProfile,
) -> anyhow::Result<(usize, usize)> {
    let datasource: Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
    )> = sqlx::query_as(
        "SELECT d.name, d.description, dsd.user_description, dsd.ai_description, d.schema_info \
         FROM data_sources d \
         LEFT JOIN nl2sql_datasource_semantics dsd \
           ON dsd.datasource_id = d.id AND dsd.deleted_at IS NULL \
         WHERE d.id = ? AND d.tenant_id = ? AND d.deleted_at IS NULL LIMIT 1",
    )
    .bind(datasource_id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await?;
    let (datasource_name, datasource_description, user_description, ai_description, schema_info) =
        datasource.ok_or_else(|| anyhow::anyhow!("datasource {datasource_id} not found"))?;

    let column_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT table_name, column_name, COALESCE(semantic_description, '') \
         FROM nl2sql_table_semantics \
         WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL \
           AND COALESCE(semantic_description, '') <> '' \
         ORDER BY table_name, column_name",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await?;
    let table_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name, \
                COALESCE(NULLIF(user_description, ''), NULLIF(ai_description, ''), '') \
         FROM nl2sql_table_desc_semantics \
         WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL \
         ORDER BY table_name",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await?;

    let mut columns_by_table: std::collections::HashMap<String, Vec<ColumnSchema>> =
        std::collections::HashMap::new();
    let mut items: Vec<(String, String, String, String)> = Vec::new();
    for (table_name, column_name, description) in column_rows {
        columns_by_table
            .entry(table_name.clone())
            .or_default()
            .push(ColumnSchema {
                name: column_name.clone(),
                data_type: String::new(),
                is_nullable: false,
                sample_values: Vec::new(),
                ai_description: Some(description.clone()),
                is_indexed: true,
                comment: None,
            });
        items.push((
            table_name.clone(),
            column_name.clone(),
            "col".to_string(),
            format!("Table: {table_name}\nColumn: {column_name}\nDescription: {description}"),
        ));
    }
    for (table_name, description) in &table_rows {
        let columns = columns_by_table
            .get(table_name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        items.push((
            table_name.clone(),
            "__table__".to_string(),
            "table".to_string(),
            SchemaDescriber::build_table_embedding_text(
                table_name,
                (!description.trim().is_empty()).then_some(description.as_str()),
                columns,
            ),
        ));
    }
    if !table_rows.is_empty() || !items.is_empty() {
        let effective_description = user_description
            .filter(|value| !value.trim().is_empty())
            .or_else(|| ai_description.filter(|value| !value.trim().is_empty()))
            .or_else(|| datasource_description.filter(|value| !value.trim().is_empty()));
        items.push((
            "__datasource__".to_string(),
            "__datasource__".to_string(),
            "datasource".to_string(),
            SchemaDescriber::build_datasource_embedding_text(
                &datasource_name,
                effective_description.as_deref(),
                &table_rows,
                &[],
            ),
        ));
    }

    let current_profiles =
        crate::nl2sql::embedding_profiles::resolve_profiles(db, tenant_id, Some("nl2sql")).await?;
    let local_store = registry.profile_store(
        tenant_id,
        &current_profiles.local.id,
        &current_profiles.local.config.model,
        current_profiles.local.config.base_url.clone(),
    )?;
    let local_keys = local_store.indexed_keys(datasource_id)?;
    if profile.config.profile_kind == crate::nl2sql::EmbeddingProfileKind::Api {
        if local_keys.is_empty() {
            let schema_tables = parse_schema_json(&schema_info).len();
            if schema_tables > 0 {
                anyhow::bail!(
                    "local embedding baseline is not ready for datasource {datasource_id}"
                );
            }
        }
        items.retain(|(table, column, kind, _)| {
            local_keys.contains(&(table.clone(), column.clone(), kind.clone()))
        });
        if items.len() != local_keys.len() {
            anyhow::bail!(
                "stored semantics cover {} of {} local schema vectors for datasource {datasource_id}",
                items.len(),
                local_keys.len()
            );
        }
    }

    let model = EmbeddingModel::new_with_dimensions(
        &profile.config.model,
        profile.config.base_url.clone(),
        (profile.config.profile_kind == crate::nl2sql::EmbeddingProfileKind::Api)
            .then(|| profile.config.api_key.clone()),
        profile.config.dimensions,
    );
    let expected_dimensions = profile.config.effective_dimensions();
    let mut stored_rows = Vec::with_capacity(items.len());
    for batch in items.chunks(64) {
        let texts: Vec<String> = batch.iter().map(|(_, _, _, text)| text.clone()).collect();
        let vectors = model.embed_batch(&texts).await?;
        if vectors.len() != batch.len() {
            anyhow::bail!(
                "embedding profile {} returned {} vectors for {} schema inputs",
                profile.id,
                vectors.len(),
                batch.len()
            );
        }
        for ((table, column, kind, _), vector) in batch.iter().zip(vectors) {
            if vector.len() != expected_dimensions {
                anyhow::bail!(
                    "embedding profile {} returned {} dimensions; expected {}",
                    profile.id,
                    vector.len(),
                    expected_dimensions
                );
            }
            stored_rows.push((
                table.clone(),
                column.clone(),
                kind.clone(),
                vector,
                profile.config.model.clone(),
            ));
        }
    }

    let store = registry.profile_store(
        tenant_id,
        &profile.id,
        &profile.config.model,
        profile.config.base_url.clone(),
    )?;
    store.replace_datasource_embeddings(datasource_id, &stored_rows)?;
    crate::nl2sql::embedding_profiles::record_profile_success(db, &profile.id).await?;
    Ok((stored_rows.len(), items.len()))
}

// ── Schema parsing ─────────────────────────────────────────────────────────────

fn parse_schema_json(schema_json: &Option<serde_json::Value>) -> Vec<TableSchema> {
    match schema_json {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect(),
        Some(serde_json::Value::Object(obj)) => obj
            .get("tables")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// ── Statistics collection ────────────────────────────────────────────────────────

/// Whether to collect table/column statistics during refresh (NL2SQL_COLLECT_STATS=true).
fn should_collect_stats() -> bool {
    std::env::var("NL2SQL_COLLECT_STATS")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false)
}

/// Collect row-count and size statistics for every table in a datasource.
/// Writes results to `nl2sql_table_stats` and `nl2sql_column_stats`.
async fn collect_stats_for_datasource(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    db_type: &str,
    config_json: &serde_json::Value,
    tables: &[TableSchema],
) {
    if !should_collect_stats() {
        return;
    }

    let resolved = match resolve_source_db_url(db_type, config_json) {
        Some(r) => r,
        None => {
            tracing::warn!(db_type = %db_type, "collect_stats: unsupported db_type or missing config");
            return;
        }
    };

    match resolved.1.as_str() {
        "mysql" | "tidb" => {
            collect_mysql_stats(db, tenant_id, datasource_id, &resolved.0, tables).await;
        }
        "postgres" => {
            collect_postgres_stats(db, tenant_id, datasource_id, &resolved.0, tables).await;
        }
        "clickhouse" => {
            collect_clickhouse_stats(db, tenant_id, datasource_id, &resolved.0, tables).await;
        }
        _ => {
            tracing::debug!(db_type = %db_type, "collect_stats: not implemented for this db_type");
        }
    }
}

fn resolve_source_db_url(
    db_type: &str,
    config: &serde_json::Value,
) -> Option<(String, String, u16, String, String, String)> {
    match db_type {
        "mysql" | "tidb" => {
            let host = config.get("host")?.as_str()?;
            let port = config.get("port")?.as_i64()?.to_string();
            let user = config.get("username")?.as_str()?;
            let pass = config.get("password")?.as_str()?;
            let database = config.get("database")?.as_str()?;
            let port: u16 = port.parse().unwrap_or(3306);
            Some((
                format!("mysql://{user}:{pass}@{host}:{port}/{database}"),
                host.to_owned(),
                port,
                database.to_owned(),
                user.to_owned(),
                pass.to_owned(),
            ))
        }
        "postgres" => {
            let host = config.get("host")?.as_str()?;
            let port = config.get("port")?.as_i64()?.to_string();
            let user = config.get("username")?.as_str()?;
            let pass = config.get("password")?.as_str()?;
            let database = config.get("database")?.as_str()?;
            let port: u16 = port.parse().unwrap_or(5432);
            Some((
                format!("postgres://{user}:{pass}@{host}:{port}/{database}"),
                host.to_owned(),
                port,
                database.to_owned(),
                user.to_owned(),
                pass.to_owned(),
            ))
        }
        "clickhouse" => {
            let host = config.get("host")?.as_str()?;
            let port = config.get("port")?.as_i64()?.to_string();
            let user = config.get("username")?.as_str()?;
            let pass = config.get("password")?.as_str()?;
            let database = config.get("database")?.as_str()?;
            let port: u16 = port.parse().unwrap_or(8123);
            Some((
                format!("clickhouse://{user}:{pass}@{host}:{port}/{database}"),
                host.to_owned(),
                port,
                database.to_owned(),
                user.to_owned(),
                pass.to_owned(),
            ))
        }
        _ => None,
    }
}

async fn collect_mysql_stats(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    url: &str,
    tables: &[TableSchema],
) {
    let pool = match sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "collect_mysql_stats: failed to connect");
            return;
        }
    };

    for table in tables {
        let table_name = &table.table_name;

        // Validate before interpolation to prevent SQL injection.
        let Ok(table_name) = validate_sql_identifier(table_name) else {
            tracing::warn!(table_name = %table_name, "collect_mysql_stats: skipping invalid table name");
            continue;
        };
        let table_name_only = table_name.split('.').last().unwrap_or(&table_name);
        let schema_name = table_name
            .split_once('.')
            .map(|(schema, _)| schema)
            .unwrap_or("")
            .to_string();
        let quoted_table_name = table_name
            .split('.')
            .map(|part| format!("`{part}`"))
            .collect::<Vec<_>>()
            .join(".");

        let row_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {quoted_table_name}"
        )))
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

        let size_bytes: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(data_length + index_length), 0) FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        )
        .bind(&schema_name)
        .bind(table_name_only)
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

        sqlx::query(
            concat!(
                "INSERT INTO nl2sql_table_stats (tenant_id, datasource_id, table_name, row_count, size_bytes, last_analyzed) ",
                "VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ",
                "ON CONFLICT DO UPDATE SET tenant_id = excluded.tenant_id, row_count = excluded.row_count, ",
                "size_bytes = excluded.size_bytes, last_analyzed = CURRENT_TIMESTAMP"
            ),
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(&table_name)
        .bind(row_count)
        .bind(size_bytes)
        .execute(db)
        .await
        .ok();

        for col in &table.columns {
            let col_name = &col.name;
            let col_type = col.data_type.to_lowercase();
            let is_numeric = col_type.contains("int")
                || col_type.contains("float")
                || col_type.contains("double")
                || col_type.contains("decimal")
                || col_type.contains("numeric");

            let Ok(col_name) = validate_sql_identifier(col_name) else {
                tracing::warn!(col_name = %col_name, table_name = %table_name,
                    "collect_mysql_stats: skipping invalid column name");
                continue;
            };

            let null_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT SUM(ISNULL(`{col_name}`)) FROM {quoted_table_name}"
            )))
            .fetch_one(&pool)
            .await
            .unwrap_or(0);

            let distinct_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(DISTINCT `{col_name}`) FROM {quoted_table_name}"
            )))
            .fetch_one(&pool)
            .await
            .unwrap_or(0);

            let total_rows = row_count.max(1);
            let null_pct = null_count as f64 / total_rows as f64 * 100.0;

            let (min_val, max_val, avg_val) = if is_numeric {
                let min_v: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT CAST(MIN(`{col_name}`) AS CHAR) FROM {quoted_table_name}"
                )))
                .fetch_one(&pool)
                .await
                .ok()
                .flatten();
                let max_v: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT CAST(MAX(`{col_name}`) AS CHAR) FROM {quoted_table_name}"
                )))
                .fetch_one(&pool)
                .await
                .ok()
                .flatten();
                let avg_v: Option<f64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT AVG(`{col_name}`) FROM {quoted_table_name}"
                )))
                .fetch_one(&pool)
                .await
                .ok()
                .flatten();
                (min_v, max_v, avg_v)
            } else {
                (None, None, None)
            };

            let samples: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT DISTINCT CAST(`{col_name}` AS CHAR) FROM {quoted_table_name} WHERE `{col_name}` IS NOT NULL LIMIT 5"
            )))
            .fetch_all(&pool)
            .await
            .unwrap_or_default();

            let samples_json = serde_json::to_string(&samples).unwrap_or_else(|_| "[]".to_string());

            sqlx::query(
                concat!(
                    "INSERT INTO nl2sql_column_stats (tenant_id, datasource_id, table_name, column_name, ",
                    "row_count, null_count, distinct_count, null_pct, min_value, max_value, avg_value, ",
                    "sample_values, last_analyzed) ",
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ",
                    "ON CONFLICT DO UPDATE SET tenant_id = excluded.tenant_id, row_count = excluded.row_count, ",
                    "null_count = excluded.null_count, distinct_count = excluded.distinct_count, ",
                    "null_pct = excluded.null_pct, min_value = excluded.min_value, ",
                    "max_value = excluded.max_value, avg_value = excluded.avg_value, ",
                    "sample_values = excluded.sample_values, last_analyzed = CURRENT_TIMESTAMP"
                ),
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(&table_name)
            .bind(&col_name)
            .bind(row_count)
            .bind(null_count)
            .bind(distinct_count)
            .bind(null_pct)
            .bind(&min_val)
            .bind(&max_val)
            .bind(avg_val)
            .bind(&samples_json)
            .execute(db)
            .await
            .ok();
        }
    }

    pool.close().await;
}

async fn collect_postgres_stats(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    url: &str,
    tables: &[TableSchema],
) {
    let pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "collect_postgres_stats: failed to connect");
            return;
        }
    };

    for table in tables {
        let table_name = &table.table_name;
        let Ok(table_name) = validate_sql_identifier(table_name) else {
            tracing::warn!(table_name = %table_name,
                "collect_postgres_stats: skipping invalid table name");
            continue;
        };
        let (schema_str, name_str) = if table_name.contains('.') {
            let parts: Vec<&str> = table_name.splitn(2, '.').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            ("public".to_string(), table_name.clone())
        };

        let row_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM \"{schema_str}\".\"{name_str}\""
        )))
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

        let size_bytes: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT pg_total_relation_size('\"{schema_str}\".\"{name_str}\"')"
        )))
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

        sqlx::query(
            concat!(
                "INSERT INTO nl2sql_table_stats (tenant_id, datasource_id, table_name, row_count, size_bytes, last_analyzed) ",
                "VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ",
                "ON CONFLICT DO UPDATE SET tenant_id = excluded.tenant_id, row_count = excluded.row_count, ",
                "size_bytes = excluded.size_bytes, last_analyzed = CURRENT_TIMESTAMP"
            ),
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(&table_name)
        .bind(row_count)
        .bind(size_bytes)
        .execute(db)
        .await
        .ok();

        for col in &table.columns {
            let col_name = &col.name;
            let col_type = col.data_type.to_lowercase();
            let is_numeric = col_type.contains("int")
                || col_type.contains("float")
                || col_type.contains("double")
                || col_type.contains("decimal")
                || col_type.contains("numeric");

            let Ok(col_name) = validate_sql_identifier(col_name) else {
                tracing::warn!(col_name = %col_name, table_name = %table_name,
                    "collect_postgres_stats: skipping invalid column name");
                continue;
            };

            let null_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(*) FROM \"{schema_str}\".\"{name_str}\" WHERE \"{col_name}\" IS NULL"
            )))
            .fetch_one(&pool)
            .await
            .unwrap_or(0);

            let distinct_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(DISTINCT \"{col_name}\") FROM \"{schema_str}\".\"{name_str}\""
            )))
            .fetch_one(&pool)
            .await
            .unwrap_or(0);

            let total_rows = row_count.max(1);
            let null_pct = null_count as f64 / total_rows as f64 * 100.0;

            let (min_val, max_val, avg_val) = if is_numeric {
                let min_v: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT MIN(\"{col_name}\")::TEXT FROM \"{schema_str}\".\"{name_str}\""
                )))
                .fetch_one(&pool)
                .await
                .ok()
                .flatten();
                let max_v: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT MAX(\"{col_name}\")::TEXT FROM \"{schema_str}\".\"{name_str}\""
                )))
                .fetch_one(&pool)
                .await
                .ok()
                .flatten();
                let avg_v: Option<f64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT AVG(\"{col_name}\") FROM \"{schema_str}\".\"{name_str}\""
                )))
                .fetch_one(&pool)
                .await
                .ok()
                .flatten();
                (min_v, max_v, avg_v)
            } else {
                (None, None, None)
            };

            let samples: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT DISTINCT \"{col_name}\"::TEXT FROM \"{schema_str}\".\"{name_str}\" WHERE \"{col_name}\" IS NOT NULL LIMIT 5"
            )))
            .fetch_all(&pool)
            .await
            .unwrap_or_default();

            let samples_json = serde_json::to_string(&samples).unwrap_or_else(|_| "[]".to_string());

            sqlx::query(
                concat!(
                    "INSERT INTO nl2sql_column_stats (tenant_id, datasource_id, table_name, column_name, ",
                    "row_count, null_count, distinct_count, null_pct, min_value, max_value, avg_value, ",
                    "sample_values, last_analyzed) ",
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ",
                    "ON CONFLICT DO UPDATE SET tenant_id = excluded.tenant_id, row_count = excluded.row_count, ",
                    "null_count = excluded.null_count, distinct_count = excluded.distinct_count, ",
                    "null_pct = excluded.null_pct, min_value = excluded.min_value, ",
                    "max_value = excluded.max_value, avg_value = excluded.avg_value, ",
                    "sample_values = excluded.sample_values, last_analyzed = CURRENT_TIMESTAMP"
                ),
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(&table_name)
            .bind(&col_name)
            .bind(row_count)
            .bind(null_count)
            .bind(distinct_count)
            .bind(null_pct)
            .bind(&min_val)
            .bind(&max_val)
            .bind(avg_val)
            .bind(&samples_json)
            .execute(db)
            .await
            .ok();
        }
    }

    pool.close().await;
}

async fn collect_clickhouse_stats(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    url: &str,
    tables: &[TableSchema],
) {
    use clickhouse::Client;
    let client = Client::default().with_url(url);

    for table in tables {
        let Ok(table_name) = validate_sql_identifier(&table.table_name) else {
            tracing::warn!(table_name = %table.table_name,
                "collect_clickhouse_stats: skipping invalid table name");
            continue;
        };
        let quoted_table_name = table_name
            .split('.')
            .map(|part| format!("`{part}`"))
            .collect::<Vec<_>>()
            .join(".");
        let database = url.split('/').nth(3).unwrap_or("default");
        let table_name_only = table_name.split('.').last().unwrap_or(table_name.as_str());

        let row_count: i64 = client
            .query(&format!("SELECT count() FROM {quoted_table_name}"))
            .fetch_all::<i64>()
            .await
            .map(|r| r[0])
            .unwrap_or(0);

        let size_bytes: i64 = client
            .query("SELECT total_bytes FROM system.tables WHERE database = ? AND name = ?")
            .bind(database)
            .bind(table_name_only)
            .fetch_all::<i64>()
            .await
            .map(|r| r[0])
            .unwrap_or(0);

        sqlx::query(
            concat!(
                "INSERT INTO nl2sql_table_stats (tenant_id, datasource_id, table_name, row_count, size_bytes, last_analyzed) ",
                "VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ",
                "ON CONFLICT DO UPDATE SET tenant_id = excluded.tenant_id, row_count = excluded.row_count, ",
                "size_bytes = excluded.size_bytes, last_analyzed = CURRENT_TIMESTAMP"
            ),
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(&table_name)
        .bind(row_count)
        .bind(size_bytes)
        .execute(db)
        .await
        .ok();

        for col in &table.columns {
            let Ok(col_name) = validate_sql_identifier(&col.name) else {
                tracing::warn!(col_name = %col.name, table_name = %table_name,
                    "collect_clickhouse_stats: skipping invalid column name");
                continue;
            };
            let quoted_col_name = format!("`{col_name}`");
            let col_type = col.data_type.to_lowercase();
            let is_numeric = col_type.contains("int")
                || col_type.contains("float")
                || col_type.contains("double")
                || col_type.contains("decimal");

            let null_count: i64 = client
                .query(&format!(
                    "SELECT count() FROM {quoted_table_name} WHERE {quoted_col_name} IS NULL"
                ))
                .fetch_all::<i64>()
                .await
                .map(|r| r[0])
                .unwrap_or(0);

            let distinct_count: i64 = client
                .query(&format!(
                    "SELECT count(distinct {quoted_col_name}) FROM {quoted_table_name}"
                ))
                .fetch_all::<i64>()
                .await
                .map(|r| r[0])
                .unwrap_or(0);

            let total_rows = row_count.max(1);
            let null_pct = null_count as f64 / total_rows as f64 * 100.0;

            let (min_val, max_val, avg_val) = if is_numeric {
                let min_v: Option<String> = client
                    .query(&format!(
                        "SELECT min({quoted_col_name}) FROM {quoted_table_name}"
                    ))
                    .fetch_all::<String>()
                    .await
                    .ok()
                    .and_then(|mut r| r.pop());
                let max_v: Option<String> = client
                    .query(&format!(
                        "SELECT max({quoted_col_name}) FROM {quoted_table_name}"
                    ))
                    .fetch_all::<String>()
                    .await
                    .ok()
                    .and_then(|mut r| r.pop());
                let avg_v: Option<f64> = client
                    .query(&format!(
                        "SELECT avg({quoted_col_name}) FROM {quoted_table_name}"
                    ))
                    .fetch_all::<f64>()
                    .await
                    .ok()
                    .and_then(|mut r| r.pop());
                (min_v, max_v, avg_v)
            } else {
                (None, None, None)
            };

            let samples: Vec<String> = client
                .query(&format!(
                    "SELECT DISTINCT {quoted_col_name} FROM {quoted_table_name} WHERE isNotNull({quoted_col_name}) LIMIT 5"
                ))
                .fetch_all::<String>()
                .await
                .unwrap_or_default();

            let samples_json = serde_json::to_string(&samples).unwrap_or_else(|_| "[]".to_string());

            sqlx::query(
                concat!(
                    "INSERT INTO nl2sql_column_stats (tenant_id, datasource_id, table_name, column_name, ",
                    "row_count, null_count, distinct_count, null_pct, min_value, max_value, avg_value, ",
                    "sample_values, last_analyzed) ",
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ",
                    "ON CONFLICT DO UPDATE SET tenant_id = excluded.tenant_id, row_count = excluded.row_count, ",
                    "null_count = excluded.null_count, distinct_count = excluded.distinct_count, ",
                    "null_pct = excluded.null_pct, min_value = excluded.min_value, ",
                    "max_value = excluded.max_value, avg_value = excluded.avg_value, ",
                    "sample_values = excluded.sample_values, last_analyzed = CURRENT_TIMESTAMP"
                ),
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(&table_name)
            .bind(col_name)
            .bind(row_count)
            .bind(null_count)
            .bind(distinct_count)
            .bind(null_pct)
            .bind(&min_val)
            .bind(&max_val)
            .bind(avg_val)
            .bind(&samples_json)
            .execute(db)
            .await
            .ok();
        }
    }
}
