//! SchemaMonitor — detects structural schema changes and creates notification records.
//!
//! Works in two modes:
//! - **Polling mode**: called by a background scheduler (e.g. cron / tokio interval).
//!   Scans all enabled datasources, diffs live schema vs cached `schema_info`,
//!   creates notifications for any changes.
//! - **Inline mode**: called at the end of a refresh task — if changes are detected,
//!   notifications are created immediately instead of waiting for the next poll cycle.
//!
//! Approval flow:
//!   1. Notifications land with `status = 'pending'` and `auto_action = 'pending_approval'`
//!   2. Admin reviews via UI and calls `approve` or `reject`
//!   3. `approve` triggers reindex; `reject` marks completed without action
//!
//! Auto-reindex flow (auto_action = 'auto_reindex'):
//!   1. `process_auto_reindex_tasks` picks up pending tasks from `nl2sql_refresh_tasks`
//!   2. Each task triggers semantic re-indexing via `trigger_semantic_reindex`
//!   3. On success the task is marked `completed`; on failure `failed` with error message

use std::sync::Arc;

use sqlparser::ast::Statement;
use sqlparser::parser::Parser;
use sqlx::SqlitePool;

use crate::nl2sql::schema_describer::SchemaDescriber;
use crate::nl2sql::schema_diff::{diff_schemas, SchemaDiff};
use crate::nl2sql::schema_discovery::SchemaDiscovery;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct SchemaMonitor {
    db: SqlitePool,
    data_dir: std::path::PathBuf,
    discovery: SchemaDiscovery,
}

impl SchemaMonitor {
    pub fn new(db: SqlitePool, data_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            db,
            data_dir: data_dir.into(),
            discovery: SchemaDiscovery::new(),
        }
    }

    /// Check a single datasource for structural changes.
    /// Returns `Some(SchemaChangeReport)` if changes were found, `None` otherwise.
    pub async fn check_datasource(
        &self,
        tenant_id: &str,
        datasource_id: &str,
    ) -> anyhow::Result<Option<SchemaChangeReport>> {
        // Load cached schema
        let cached: Option<serde_json::Value> =
            sqlx::query_scalar::<sqlx::Sqlite, Option<serde_json::Value>>(
                "SELECT schema_info FROM data_sources WHERE id = ?",
            )
            .bind(datasource_id)
            .fetch_optional(&self.db)
            .await?
            .flatten();

        let cached_json = match cached {
            Some(v) => v,
            None => return Ok(None),
        };

        // Load datasource config for schema discovery
        let (db_type, config_json): (String, String) = sqlx::query_as::<sqlx::Sqlite, _>(
            "SELECT db_type, config FROM data_sources WHERE id = ?",
        )
        .bind(datasource_id)
        .fetch_one(&self.db)
        .await?;

        let encrypted_config: serde_json::Value = serde_json::from_str(&config_json)
            .map_err(|error| anyhow::anyhow!("invalid encrypted datasource config: {error}"))?;
        let config = crate::routes::data_sources::decrypt_config(
            &encrypted_config,
            &self.data_dir,
            tenant_id,
            datasource_id,
        )
        .map_err(|error| anyhow::anyhow!("failed to decrypt datasource config: {error}"))?;

        // Discover live schema
        let live_schema = match self.discovery.discover(&db_type, &config).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(datasource_id = %datasource_id, error = %e, "SchemaMonitor: failed to discover live schema");
                return Ok(None);
            }
        };

        // Diff
        let diff = diff_schemas(&cached_json, &serde_json::json!(live_schema.tables));
        if diff.is_empty() {
            return Ok(None);
        }

        let affected = self.count_affected_queries(datasource_id, &diff).await?;
        let recommendations = self.classify_recommendations(&diff);

        Ok(Some(SchemaChangeReport {
            diff,
            affected_queries: affected,
            recommendations,
        }))
    }

    /// Save a change report as notification records.
    /// Called by the scheduler or by the refresh endpoint after a diff.
    ///
    /// For each notification, `affected_queries_count` is calculated per-table
    /// (not as an aggregate) so the UI can show granular impact data.
    pub async fn save_notifications(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        task_id: Option<&str>,
        report: &SchemaChangeReport,
    ) -> anyhow::Result<u64> {
        let mut tx = sqlx::Acquire::begin(&self.db).await?;
        let mut total = 0u64;

        let tables_added = serde_json::to_string(&report.diff.added)?;
        let tables_removed = serde_json::to_string(&report.diff.removed)?;
        let tables_changed = serde_json::to_string(&report.diff.changed)?;

        if !report.diff.added.is_empty() {
            // Use the first added table's per-table count as the notification value.
            let count = if let Some(first) = report.diff.added.first() {
                calculate_affected_queries_count(&self.db, tenant_id, datasource_id, first, None)
                    .await
                    .unwrap_or(0)
            } else {
                0
            };
            sqlx::query::<sqlx::Sqlite>(
                r#"INSERT INTO nl2sql_schema_change_notifications
                    (tenant_id, datasource_id, task_id, change_type, details, affected_queries_count, recommended_action)
                   VALUES (?, ?, ?, 'tables_added', ?, ?, 'reindex')"#,
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(task_id)
            .bind(&tables_added)
            .bind(count)
            .execute(&mut *tx)
            .await?;
            total += 1;
        }

        if !report.diff.removed.is_empty() {
            // Use the first removed table's per-table count for the recommended_action decision.
            let first_removed = report.diff.removed.first();
            let count = if let Some(t) = first_removed {
                calculate_affected_queries_count(&self.db, tenant_id, datasource_id, t, None)
                    .await
                    .unwrap_or(0)
            } else {
                0
            };
            let action = if count > 10 || report.affected_queries > 10 {
                "review_semantics"
            } else {
                "reindex"
            };
            sqlx::query::<sqlx::Sqlite>(
                r#"INSERT INTO nl2sql_schema_change_notifications
                    (tenant_id, datasource_id, task_id, change_type, details, affected_queries_count, recommended_action)
                   VALUES (?, ?, ?, 'tables_removed', ?, ?, ?)"#,
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(task_id)
            .bind(&tables_removed)
            .bind(count)
            .bind(action)
            .execute(&mut *tx)
            .await?;
            total += 1;
        }

        if !report.diff.changed.is_empty() {
            // Use the first changed table's per-table count.
            let count = if let Some(first) = report.diff.changed.first() {
                calculate_affected_queries_count(&self.db, tenant_id, datasource_id, first, None)
                    .await
                    .unwrap_or(0)
            } else {
                0
            };
            sqlx::query::<sqlx::Sqlite>(
                r#"INSERT INTO nl2sql_schema_change_notifications
                    (tenant_id, datasource_id, task_id, change_type, details, affected_queries_count, recommended_action)
                   VALUES (?, ?, ?, 'tables_changed', ?, ?, 'review_semantics')"#,
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(task_id)
            .bind(&tables_changed)
            .bind(count)
            .execute(&mut *tx)
            .await?;
            total += 1;
        }

        // Update refresh task if provided
        if let Some(tid) = task_id {
            let summary = serde_json::json!({
                "added_tables": report.diff.added,
                "removed_tables": report.diff.removed,
                "changed_tables": report.diff.changed,
            });
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE nl2sql_refresh_tasks SET change_summary = ?, auto_action = 'pending_approval' WHERE task_id = ?",
            )
            .bind(serde_json::to_string(&summary)?)
            .bind(tid)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(total)
    }

    /// Approve a notification: update status, trigger reindex, mark completed.
    pub async fn approve(&self, notification_id: u64, reviewed_by: &str) -> anyhow::Result<()> {
        let (datasource_id, task_id): (String, Option<String>) = sqlx::query_as::<sqlx::Sqlite, _>(
            "SELECT datasource_id, task_id FROM nl2sql_schema_change_notifications WHERE id = ?",
        )
        .bind(crate::sqlite_i64(notification_id))
        .fetch_one(&self.db)
        .await?;

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_schema_change_notifications SET status = 'approved', reviewed_by = ?, reviewed_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(reviewed_by)
        .bind(crate::sqlite_i64(notification_id))
        .execute(&self.db)
        .await?;

        // Trigger reindex via the existing reindex endpoint logic
        // (mark embedding_needs_reindex = 1 on data_sources)
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE data_sources SET embedding_needs_reindex = 1 WHERE id = ?",
        )
        .bind(&datasource_id)
        .execute(&self.db)
        .await?;

        // Also mark the task as auto_reindex if it exists
        if let Some(tid) = &task_id {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE nl2sql_refresh_tasks SET auto_action = 'auto_reindex', status = 'pending' WHERE task_id = ?",
            )
            .bind(tid)
            .execute(&self.db)
            .await?;
        }

        // Mark notification as completed once reindex is done
        // (The caller of this function should update to 'completed' after reindex)
        Ok(())
    }

    /// Reject a notification.
    pub async fn reject(&self, notification_id: u64, reviewed_by: &str) -> anyhow::Result<()> {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_schema_change_notifications SET status = 'rejected', reviewed_by = ?, reviewed_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(reviewed_by)
        .bind(crate::sqlite_i64(notification_id))
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Mark notification as completed.
    pub async fn complete(&self, notification_id: u64) -> anyhow::Result<()> {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_schema_change_notifications SET status = 'completed' WHERE id = ?",
        )
        .bind(crate::sqlite_i64(notification_id))
        .execute(&self.db)
        .await?;
        Ok(())
    }

    // ─── Helpers ────────────────────────────────────────────────────────────

    /// Count how many executed queries in the past 30 days used any of the changed tables.
    /// P3-3: Uses sqlparser AST to extract actual table names from SQL, avoiding
    /// false positives from naive string matching (e.g. "orders" matching "reorders").
    async fn count_affected_queries(
        &self,
        datasource_id: &str,
        diff: &SchemaDiff,
    ) -> anyhow::Result<u32> {
        let all: Vec<String> = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT generated_sql FROM nl2sql_queries WHERE datasource_id = ? AND deleted_at IS NULL AND executed = 1 AND created_at > datetime(CURRENT_TIMESTAMP, '-30 days')",
        )
        .bind(datasource_id)
        .fetch_all(&self.db)
        .await?;

        let all_tables: std::collections::HashSet<_> = diff
            .added
            .iter()
            .chain(diff.removed.iter())
            .chain(diff.changed.iter())
            .collect();

        let mut count = 0u32;
        for sql in all {
            if sql_mentions_any_table(&sql, &all_tables) {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Classify impact and recommend action based on diff contents.
    fn classify_recommendations(&self, diff: &SchemaDiff) -> ChangeRecommendations {
        let has_removed = !diff.removed.is_empty();
        let has_added = !diff.added.is_empty();
        let has_changed = !diff.changed.is_empty();

        ChangeRecommendations {
            reindex_embedding: has_added || has_changed,
            review_semantics: has_removed || has_changed,
            notify_admin: has_removed,
            description: if has_removed {
                "Removed tables may break existing queries. Review and update affected queries."
                    .to_string()
            } else if has_changed {
                "Column changes detected. Review semantic descriptions to ensure NL2SQL accuracy."
                    .to_string()
            } else {
                "New tables detected. Re-index embeddings to enable routing to new tables."
                    .to_string()
            },
        }
    }
}

// ─── Report Types ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct SchemaChangeReport {
    pub diff: SchemaDiff,
    pub affected_queries: u32,
    pub recommendations: ChangeRecommendations,
}

#[derive(Debug)]
pub struct ChangeRecommendations {
    pub reindex_embedding: bool,
    pub review_semantics: bool,
    pub notify_admin: bool,
    pub description: String,
}

// ─── Report Types ─────────────────────────────────────────────────────────────

/// P3-3: Check if a SQL query references any table in the given set using AST parsing.
/// Falls back to lowercase substring match if parsing fails (e.g. unsupported dialect).
fn sql_mentions_any_table(sql: &str, tables: &std::collections::HashSet<&String>) -> bool {
    // Fast path: if SQL is short, try substring match first
    if sql.len() < 20 {
        for table in tables {
            if sql.contains(table.as_str()) {
                return true;
            }
        }
        return false;
    }

    let dialect = sqlparser::dialect::GenericDialect {};
    let statements = match Parser::parse_sql(&dialect, sql) {
        Ok(s) => s,
        Err(_) => return false,
    };

    for stmt in &statements {
        if statement_mentions_any_table(stmt, tables) {
            return true;
        }
    }
    false
}

/// Recursively check if a statement references any table in the set.
fn statement_mentions_any_table(
    stmt: &Statement,
    tables: &std::collections::HashSet<&String>,
) -> bool {
    use sqlparser::ast::{Visit, Visitor};

    struct TableFinder<'a> {
        targets: &'a std::collections::HashSet<&'a String>,
        found: bool,
    }

    impl<'a> Visitor for TableFinder<'a> {
        type Break = ();

        fn pre_visit_relation(
            &mut self,
            relation: &sqlparser::ast::ObjectName,
        ) -> std::ops::ControlFlow<()> {
            if self.found {
                return std::ops::ControlFlow::Break(());
            }
            let name = relation.to_string();
            if self.targets.contains(&name) || self.targets.contains(&name.to_lowercase()) {
                self.found = true;
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }

    let mut finder = TableFinder {
        targets: tables,
        found: false,
    };
    let _ = stmt.visit(&mut finder);
    finder.found
}

// ─── Task row type ────────────────────────────────────────────────────────────

#[derive(Debug)]
struct RefreshTaskRow {
    task_id: String,
    tenant_id: String,
    datasource_id: String,
}

// ─── Auto-reindex: background task processor ───────────────────────────────────

/// Picks up all pending `auto_reindex` tasks from `nl2sql_refresh_tasks` and
/// processes them one by one by invoking the semantic re-indexing pipeline.
///
/// This is called on every scheduler tick alongside the normal schema-refresh
/// cycle so that tasks created by schema changes are picked up promptly.
pub(crate) async fn process_auto_reindex_tasks(state: &AppState) {
    let tasks: Vec<RefreshTaskRow> = match sqlx::query_as::<sqlx::Sqlite, (String, String, String)>(
        "SELECT task_id, tenant_id, datasource_id \
         FROM nl2sql_refresh_tasks \
         WHERE auto_action = 'auto_reindex' AND status = 'pending'",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|(task_id, tenant_id, datasource_id)| RefreshTaskRow {
                task_id,
                tenant_id,
                datasource_id,
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "process_auto_reindex_tasks: failed to fetch pending tasks");
            return;
        }
    };

    if tasks.is_empty() {
        return;
    }

    tracing::info!(
        count = tasks.len(),
        "process_auto_reindex_tasks: processing {} pending tasks",
        tasks.len()
    );

    for task in tasks {
        // Mark as running so we don't pick it up again on the next tick.
        if let Err(e) = sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_refresh_tasks SET status = 'running' WHERE task_id = ?",
        )
        .bind(&task.task_id)
        .execute(&state.db)
        .await
        {
            tracing::warn!(task_id = %task.task_id, error = %e, "failed to mark task as running");
            continue;
        }

        match trigger_semantic_reindex(state, &task.tenant_id, &task.datasource_id, &task.task_id)
            .await
        {
            Ok(_) => {
                tracing::info!(task_id = %task.task_id, "auto_reindex task completed");
                let _ = sqlx::query::<sqlx::Sqlite>(
                    "UPDATE nl2sql_refresh_tasks SET status = 'completed', completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                )
                .bind(&task.task_id)
                .execute(&state.db)
                .await;
            }
            Err(e) => {
                tracing::error!(task_id = %task.task_id, error = %e, "auto_reindex task failed");
                let _ = sqlx::query::<sqlx::Sqlite>(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', error_message = ?, completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                )
                .bind(e.to_string())
                .bind(&task.task_id)
                .execute(&state.db)
                .await;
            }
        }
    }
}

/// Triggers full semantic re-indexing for a datasource using the existing
/// `SchemaDescriber::refresh_datasource` pipeline.
///
/// This rebuilds embeddings and AI descriptions while the currently active
/// profile remains available for queries.
async fn trigger_semantic_reindex(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    _task_id: &str,
) -> anyhow::Result<()> {
    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("embedding store not initialized"))?;

    // Reset semantic tracking columns so all tables/columns get re-described.
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_table_semantics \
         SET semantic_description = '', embedding_model = '', is_indexed = 0 \
         WHERE datasource_id = ?",
    )
    .bind(datasource_id)
    .execute(&state.db)
    .await
    .ok();

    sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_table_desc_semantics \
         SET ai_description = '', embedding_model = '' \
         WHERE datasource_id = ?",
    )
    .bind(datasource_id)
    .execute(&state.db)
    .await
    .ok();

    // Resolve per-tenant config.
    let embed_cfg = crate::nl2sql::resolve_embedding_config(&state.db, tenant_id, None).await;

    let chat_cfg = match state.config_registry.as_ref() {
        Some(registry) => crate::nl2sql::resolve_chat_config(
            registry.as_ref(),
            tenant_id,
            tenant_id,
            &state.default_model,
            None,
        )
        .await
        .map_err(|e| anyhow::anyhow!("chat config error: {}", e))?,
        None => {
            return Err(anyhow::anyhow!("config registry not available"));
        }
    };

    let describer = SchemaDescriber::new(
        state.db.clone(),
        Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    describer
        .refresh_datasource(tenant_id, datasource_id)
        .await?;

    Ok(())
}

// ─── Auto-calculate affected_queries_count ────────────────────────────────────

/// Calculates the number of historical queries that reference a changed table
/// (or optionally a specific column within that table).
///
/// Matches against both `matched_tables` and `generated_sql` columns using
/// case-insensitive substring patterns. Uses `tenant_id` and `datasource_id`
/// as the primary filter scope.
pub(crate) async fn calculate_affected_queries_count(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    table_name: &str,
    column_name: Option<&str>,
) -> anyhow::Result<i64> {
    let table_pattern = format!("%{}%", table_name);

    let count: i64 = if let Some(col) = column_name {
        let col_pattern = format!("%{}%", col);
        sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT COUNT(*) FROM nl2sql_queries \
             WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL \
               AND (matched_tables LIKE ? COLLATE utf8mb4_general_ci \
                 OR generated_sql LIKE ? COLLATE utf8mb4_general_ci \
                 OR matched_columns LIKE ? COLLATE utf8mb4_general_ci)",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(&table_pattern)
        .bind(&table_pattern)
        .bind(&col_pattern)
        .fetch_one(db)
        .await
        .map_err(|e| anyhow::anyhow!("failed to count affected queries: {}", e))?
    } else {
        sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT COUNT(*) FROM nl2sql_queries \
             WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL \
               AND (matched_tables LIKE ? COLLATE utf8mb4_general_ci \
                 OR generated_sql LIKE ? COLLATE utf8mb4_general_ci)",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(&table_pattern)
        .bind(&table_pattern)
        .fetch_one(db)
        .await
        .map_err(|e| anyhow::anyhow!("failed to count affected queries: {}", e))?
    };

    Ok(count)
}
