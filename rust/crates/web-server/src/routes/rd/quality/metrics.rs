//! Quality snapshot metric loaders and JSON accumulators.

use super::*;

#[derive(Debug, Clone, Default)]
pub(in crate::routes::rd) struct RdQualityEmbeddingMetrics {
    pub(in crate::routes::rd) store_enabled: bool,
    pub(in crate::routes::rd) model: Option<String>,
    pub(in crate::routes::rd) total_count: u64,
    pub(in crate::routes::rd) context_summary_count: u64,
    pub(in crate::routes::rd) file_summary_count: u64,
    pub(in crate::routes::rd) symbol_count: u64,
    pub(in crate::routes::rd) import_count: u64,
    pub(in crate::routes::rd) task_count: u64,
}

#[derive(Debug, Clone, Default)]
pub(in crate::routes::rd) struct RdQualitySummaryCacheMetrics {
    pub(in crate::routes::rd) repository_summary_count: u64,
    pub(in crate::routes::rd) directory_summary_count: u64,
    pub(in crate::routes::rd) file_summary_count: u64,
    pub(in crate::routes::rd) llm_summary_count: u64,
    pub(in crate::routes::rd) stale_summary_count: u64,
}

#[derive(Debug, Clone, Default)]
pub(in crate::routes::rd) struct RdQualityIndexCacheMetrics {
    pub(in crate::routes::rd) embedding_reused_chunk_count: u64,
    pub(in crate::routes::rd) embedding_regenerated_chunk_count: u64,
    pub(in crate::routes::rd) embedding_pruned_chunk_count: u64,
    pub(in crate::routes::rd) file_summary_reused_count: u64,
    pub(in crate::routes::rd) file_summary_regenerated_count: u64,
    pub(in crate::routes::rd) estimated_tokens_saved: u64,
}

#[derive(Debug, Clone, Default)]
pub(in crate::routes::rd) struct RdQualityTokenMetrics {
    pub(in crate::routes::rd) input_tokens: u64,
    pub(in crate::routes::rd) output_tokens: u64,
    pub(in crate::routes::rd) cache_creation_tokens: u64,
    pub(in crate::routes::rd) cache_read_tokens: u64,
    pub(in crate::routes::rd) total_tokens: u64,
}

impl RdQualityTokenMetrics {
    pub(in crate::routes::rd) fn add(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

#[derive(Debug, Clone, Default)]
pub(in crate::routes::rd) struct RdQualityObservabilityMetrics {
    pub(in crate::routes::rd) retrieval_evidence_event_count: u64,
    pub(in crate::routes::rd) cache_usage_event_count: u64,
    pub(in crate::routes::rd) retrieval_selected_file_count: u64,
    pub(in crate::routes::rd) embedding_hit_count: u64,
    pub(in crate::routes::rd) summary_hit_count: u64,
    pub(in crate::routes::rd) symbol_hit_count: u64,
    pub(in crate::routes::rd) import_hit_count: u64,
    pub(in crate::routes::rd) dependency_graph_hit_count: u64,
    pub(in crate::routes::rd) task_memory_hit_count: u64,
    pub(in crate::routes::rd) read_file_count: u64,
    pub(in crate::routes::rd) grep_search_count: u64,
    pub(in crate::routes::rd) glob_search_count: u64,
    pub(in crate::routes::rd) repeated_tool_target_count: u64,
    pub(in crate::routes::rd) tokens: RdQualityTokenMetrics,
    pub(in crate::routes::rd) runtime_token_metrics: RdQualityTokenMetrics,
    pub(in crate::routes::rd) context_planner_tokens: u64,
    pub(in crate::routes::rd) runtime_tokens: u64,
}

#[derive(Debug, Clone, Default)]
struct RdQualityTaskTokenStageState {
    context_plan: Option<RdQualityTokenMetrics>,
    runtime_usage: Option<RdQualityTokenMetrics>,
    runtime: Option<RdQualityTokenMetrics>,
    summary: Option<RdQualityTokenMetrics>,
}

impl RdQualityTaskTokenStageState {
    fn add_selected_to(self, metrics: &mut RdQualityObservabilityMetrics) {
        if let Some(tokens) = self.context_plan {
            metrics.context_planner_tokens = metrics
                .context_planner_tokens
                .saturating_add(tokens.total_tokens);
            metrics.tokens.add(&tokens);
        }

        let runtime_tokens = self.runtime_usage.or(self.runtime).or(self.summary);
        if let Some(tokens) = runtime_tokens {
            metrics.runtime_tokens = metrics.runtime_tokens.saturating_add(tokens.total_tokens);
            metrics.runtime_token_metrics.add(&tokens);
            metrics.tokens.add(&tokens);
        }
    }
}

pub(in crate::routes::rd) async fn load_rd_quality_embedding_metrics(
    state: &AppState,
    claims: &Claims,
    repository_id: Option<&str>,
) -> RdQualityEmbeddingMetrics {
    let tenant_id = &claims.tenant_id;
    let mut metrics = RdQualityEmbeddingMetrics {
        store_enabled: state.rd_embedding_store.is_some(),
        model: match load_rd_quality_embedding_model(&state.db, tenant_id, repository_id).await {
            Ok(model) => model,
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    repository_id = ?repository_id,
                    "failed to load RD embedding model for quality summary: {}",
                    error
                );
                None
            }
        },
        ..Default::default()
    };

    let Some(store) = state.rd_embedding_store.as_ref().cloned() else {
        return metrics;
    };
    let tenant = tenant_id.to_string();
    let repo = repository_id.map(ToOwned::to_owned);
    let repositories = if claims.has_tenant_wide_monitoring_scope() || repo.is_some() {
        vec![repo]
    } else {
        match sqlx::query_scalar::<_, String>(
            "SELECT id FROM gitlab_projects WHERE tenant_id = ? AND user_id = ?",
        )
        .bind(tenant_id)
        .bind(&claims.sub)
        .fetch_all(&state.db)
        .await
        {
            Ok(rows) => rows.into_iter().map(Some).collect(),
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    user_id = %claims.sub,
                    "failed to resolve RD repositories for quality embedding metrics: {}",
                    error
                );
                Vec::new()
            }
        }
    };
    let counts = tokio::task::spawn_blocking(move || {
        let mut combined = HashMap::<String, usize>::new();
        for repository_id in repositories {
            let Ok(counts) = store.chunk_counts_by_type(&tenant, repository_id.as_deref(), None)
            else {
                continue;
            };
            for (kind, count) in counts {
                *combined.entry(kind).or_default() += count;
            }
        }
        combined
    })
    .await
    .ok()
    .unwrap_or_default();

    metrics.context_summary_count = usize_to_u64(*counts.get("context_summary").unwrap_or(&0));
    metrics.file_summary_count = usize_to_u64(*counts.get("file_summary").unwrap_or(&0));
    metrics.symbol_count = usize_to_u64(*counts.get("symbol").unwrap_or(&0));
    metrics.import_count = usize_to_u64(*counts.get("import").unwrap_or(&0));
    metrics.task_count = usize_to_u64(*counts.get("task").unwrap_or(&0));
    metrics.total_count = metrics
        .context_summary_count
        .saturating_add(metrics.file_summary_count)
        .saturating_add(metrics.symbol_count)
        .saturating_add(metrics.import_count)
        .saturating_add(metrics.task_count);
    metrics
}

pub(in crate::routes::rd) async fn load_rd_quality_embedding_model(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    let rows = if let Some(repository_id) = repository_id {
        sqlx::query(
            "SELECT embedding_model \
             FROM rd_repository_indexes \
             WHERE tenant_id = ? AND repository_id = ? AND embedding_model IS NOT NULL AND embedding_model <> '' \
             ORDER BY last_indexed_at DESC, updated_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(repository_id)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query(
            "SELECT embedding_model \
             FROM rd_repository_indexes \
             WHERE tenant_id = ? AND embedding_model IS NOT NULL AND embedding_model <> '' \
             ORDER BY last_indexed_at DESC, updated_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .fetch_all(db)
        .await?
    };
    Ok(rows
        .first()
        .and_then(|row| row.get::<Option<String>, _>("embedding_model")))
}

pub(in crate::routes::rd) async fn load_rd_quality_summary_metrics(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: Option<&str>,
    repository_id: Option<&str>,
) -> Result<RdQualitySummaryCacheMetrics, sqlx::Error> {
    let mut metrics = RdQualitySummaryCacheMetrics::default();
    let mut context_sql = String::from(
        "SELECT scope_type, \
            COUNT(*) AS summary_count, \
            CAST(COALESCE(SUM(CASE WHEN llm_summary_text IS NOT NULL AND llm_summary_text <> '' THEN 1 ELSE 0 END), 0) AS INTEGER) AS llm_summary_count, \
            CAST(COALESCE(SUM(CASE WHEN llm_summary_text IS NULL OR llm_summary_text = '' THEN 1 ELSE 0 END), 0) AS INTEGER) AS pending_llm_count \
         FROM rd_repository_context_summaries s \
         INNER JOIN gitlab_projects p ON p.tenant_id = s.tenant_id AND p.id = s.repository_id \
         WHERE s.tenant_id = ?",
    );
    if user_id.is_some() {
        context_sql.push_str(" AND p.user_id = ?");
    }
    if repository_id.is_some() {
        context_sql.push_str(" AND repository_id = ?");
    }
    context_sql.push_str(" GROUP BY scope_type");
    let mut context_query = sqlx::query(&context_sql).bind(tenant_id);
    if let Some(user_id) = user_id {
        context_query = context_query.bind(user_id);
    }
    if let Some(repository_id) = repository_id {
        context_query = context_query.bind(repository_id);
    }
    let context_rows = context_query.fetch_all(db).await?;
    for row in context_rows {
        let scope_type: String = row.get("scope_type");
        let count = nonnegative_i64_to_u64(row.get("summary_count"));
        let llm_count = nonnegative_i64_to_u64(row.get("llm_summary_count"));
        let pending_llm_count = nonnegative_i64_to_u64(row.get("pending_llm_count"));
        match scope_type.as_str() {
            "repository" | "entrypoints" => {
                metrics.repository_summary_count =
                    metrics.repository_summary_count.saturating_add(count);
            }
            "directory" => {
                metrics.directory_summary_count =
                    metrics.directory_summary_count.saturating_add(count);
            }
            _ => {}
        }
        metrics.llm_summary_count = metrics.llm_summary_count.saturating_add(llm_count);
        // Deterministic summaries are still valid; this counts scopes waiting for
        // optional LLM refinement so operators can see cache freshness work left.
        metrics.stale_summary_count = metrics
            .stale_summary_count
            .saturating_add(pending_llm_count);
    }

    let mut file_sql = String::from(
        "SELECT COUNT(*) AS file_summary_count \
         FROM rd_repository_file_summaries s \
         INNER JOIN gitlab_projects p ON p.tenant_id = s.tenant_id AND p.id = s.repository_id \
         WHERE s.tenant_id = ?",
    );
    if user_id.is_some() {
        file_sql.push_str(" AND p.user_id = ?");
    }
    if repository_id.is_some() {
        file_sql.push_str(" AND repository_id = ?");
    }
    let mut file_query = sqlx::query(&file_sql).bind(tenant_id);
    if let Some(user_id) = user_id {
        file_query = file_query.bind(user_id);
    }
    if let Some(repository_id) = repository_id {
        file_query = file_query.bind(repository_id);
    }
    let file_row = file_query.fetch_one(db).await?;
    metrics.file_summary_count = nonnegative_i64_to_u64(file_row.get("file_summary_count"));
    Ok(metrics)
}

pub(in crate::routes::rd) async fn load_rd_quality_index_cache_metrics(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: Option<&str>,
    repository_id: Option<&str>,
) -> Result<RdQualityIndexCacheMetrics, sqlx::Error> {
    let mut sql = String::from(
        "SELECT i.detail_json FROM rd_repository_indexes i
         INNER JOIN gitlab_projects p ON p.tenant_id = i.tenant_id AND p.id = i.repository_id
         WHERE i.tenant_id = ?",
    );
    if user_id.is_some() {
        sql.push_str(" AND p.user_id = ?");
    }
    if repository_id.is_some() {
        sql.push_str(" AND repository_id = ?");
    }
    let mut query = sqlx::query(&sql).bind(tenant_id);
    if let Some(user_id) = user_id {
        query = query.bind(user_id);
    }
    if let Some(repository_id) = repository_id {
        query = query.bind(repository_id);
    }
    let rows = query.fetch_all(db).await?;
    let mut metrics = RdQualityIndexCacheMetrics::default();
    for row in rows {
        let detail: Option<Value> = row.get("detail_json");
        let Some(detail) = detail else {
            continue;
        };
        metrics.embedding_reused_chunk_count = metrics
            .embedding_reused_chunk_count
            .saturating_add(rd_json_u64(&detail, &["embeddingReusedChunks"]).unwrap_or(0));
        metrics.embedding_regenerated_chunk_count = metrics
            .embedding_regenerated_chunk_count
            .saturating_add(rd_json_u64(&detail, &["embeddingRegeneratedChunks"]).unwrap_or(0));
        metrics.embedding_pruned_chunk_count = metrics
            .embedding_pruned_chunk_count
            .saturating_add(rd_json_u64(&detail, &["embeddingPrunedChunks"]).unwrap_or(0));
        metrics.file_summary_reused_count = metrics
            .file_summary_reused_count
            .saturating_add(rd_json_u64(&detail, &["fileSummaryReusedCount"]).unwrap_or(0));
        metrics.file_summary_regenerated_count = metrics
            .file_summary_regenerated_count
            .saturating_add(rd_json_u64(&detail, &["fileSummaryRegeneratedCount"]).unwrap_or(0));
        metrics.estimated_tokens_saved = metrics
            .estimated_tokens_saved
            .saturating_add(rd_json_u64(&detail, &["embeddingEstimatedTokensSaved"]).unwrap_or(0));
    }
    Ok(metrics)
}

pub(in crate::routes::rd) async fn load_rd_quality_observability_metrics(
    db: &SqlitePool,
    claims: &Claims,
    days: u32,
    repository_id: Option<&str>,
) -> Result<RdQualityObservabilityMetrics, sqlx::Error> {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let mut sql = String::from(
        "SELECT e.task_id, e.stage, e.detail_json \
         FROM rd_task_events e \
         INNER JOIN rd_tasks t ON t.id = e.task_id AND t.tenant_id = e.tenant_id \
         WHERE e.tenant_id = ? AND (? = 1 OR t.user_id = ?) \
           AND e.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?)) \
           AND e.stage IN ('context_cache_usage','context_retrieval_evidence','runtime_tool_governance','runtime_usage','runtime','summary','context_plan_llm')",
    );
    if repository_id.is_some() {
        sql.push_str(" AND t.repository_id = ?");
    }
    sql.push_str(" LIMIT 5000");

    let mut query = sqlx::query(&sql)
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days));
    if let Some(repository_id) = repository_id {
        query = query.bind(repository_id);
    }

    let rows = query.fetch_all(db).await?;
    let mut metrics = RdQualityObservabilityMetrics::default();
    let mut cache_tasks = HashSet::new();
    let mut retrieval_details_by_task: HashMap<String, Vec<Value>> = HashMap::new();
    let mut token_stages_by_task: HashMap<String, RdQualityTaskTokenStageState> = HashMap::new();

    for row in rows {
        let task_id: String = row.get("task_id");
        let stage: String = row.get("stage");
        let detail: Option<Value> = row.get("detail_json");
        let Some(detail) = detail else {
            continue;
        };
        match stage.as_str() {
            "context_cache_usage" => {
                metrics.cache_usage_event_count = metrics.cache_usage_event_count.saturating_add(1);
                cache_tasks.insert(task_id);
                accumulate_rd_quality_cache_usage(&mut metrics, &detail);
            }
            "context_retrieval_evidence" => {
                metrics.retrieval_evidence_event_count =
                    metrics.retrieval_evidence_event_count.saturating_add(1);
                retrieval_details_by_task
                    .entry(task_id)
                    .or_default()
                    .push(detail);
            }
            "runtime_tool_governance" => {
                accumulate_rd_quality_tool_governance(&mut metrics, &detail);
            }
            "runtime_usage" => {
                if let Some(tokens) = rd_quality_token_metrics_from_detail(&detail) {
                    token_stages_by_task
                        .entry(task_id)
                        .or_default()
                        .runtime_usage = Some(tokens);
                }
            }
            "runtime" => {
                if let Some(tokens) = rd_quality_token_metrics_from_detail(&detail) {
                    token_stages_by_task.entry(task_id).or_default().runtime = Some(tokens);
                }
            }
            "summary" => {
                if let Some(tokens) = rd_quality_token_metrics_from_detail(&detail) {
                    token_stages_by_task.entry(task_id).or_default().summary = Some(tokens);
                }
            }
            "context_plan_llm" => {
                if let Some(tokens) = rd_quality_token_metrics_from_detail(&detail) {
                    token_stages_by_task
                        .entry(task_id)
                        .or_default()
                        .context_plan = Some(tokens);
                }
            }
            _ => {}
        }
    }

    for (task_id, details) in retrieval_details_by_task {
        if cache_tasks.contains(&task_id) {
            continue;
        }
        for detail in details {
            accumulate_rd_quality_retrieval_evidence(&mut metrics, &detail);
        }
    }

    for task_tokens in token_stages_by_task.into_values() {
        task_tokens.add_selected_to(&mut metrics);
    }

    Ok(metrics)
}

fn accumulate_rd_quality_cache_usage(metrics: &mut RdQualityObservabilityMetrics, detail: &Value) {
    metrics.retrieval_selected_file_count = metrics
        .retrieval_selected_file_count
        .saturating_add(rd_json_u64(detail, &["selectedFiles", "fileCount"]).unwrap_or(0));
    metrics.embedding_hit_count = metrics
        .embedding_hit_count
        .saturating_add(rd_json_u64(detail, &["embeddingHits", "embeddingHitCount"]).unwrap_or(0));
    metrics.summary_hit_count = metrics
        .summary_hit_count
        .saturating_add(rd_json_u64(detail, &["summaryHits", "summaryHitCount"]).unwrap_or(0));
    metrics.symbol_hit_count = metrics
        .symbol_hit_count
        .saturating_add(rd_json_u64(detail, &["symbolHits", "symbolHitCount"]).unwrap_or(0));
    metrics.import_hit_count = metrics
        .import_hit_count
        .saturating_add(rd_json_u64(detail, &["importHits", "importHitCount"]).unwrap_or(0));
    metrics.dependency_graph_hit_count = metrics.dependency_graph_hit_count.saturating_add(
        rd_json_u64(detail, &["dependencyGraphHits", "dependencyGraphHitCount"]).unwrap_or(0),
    );
    metrics.task_memory_hit_count = metrics.task_memory_hit_count.saturating_add(
        rd_json_u64(detail, &["taskMemoryHits", "taskMemoryHitCount"]).unwrap_or(0),
    );

    if metrics.embedding_hit_count == 0
        || metrics.summary_hit_count == 0
        || metrics.symbol_hit_count == 0
        || metrics.import_hit_count == 0
        || metrics.dependency_graph_hit_count == 0
        || metrics.task_memory_hit_count == 0
    {
        if let Some(sources) = rd_json_string_array_from_value(detail, &["cacheSources", "sources"])
        {
            accumulate_rd_quality_source_hit(metrics, &sources.into_iter().collect());
        }
    }
}

fn accumulate_rd_quality_retrieval_evidence(
    metrics: &mut RdQualityObservabilityMetrics,
    detail: &Value,
) {
    let mut counted_files = 0u64;
    if let Some(files) = detail.get("files").and_then(Value::as_array) {
        for file in files {
            let Some(sources) = rd_json_string_array_from_value(file, &["sources"]) else {
                continue;
            };
            counted_files = counted_files.saturating_add(1);
            let sources = sources.into_iter().collect::<BTreeSet<_>>();
            accumulate_rd_quality_source_hit(metrics, &sources);
        }
    }
    if counted_files == 0 {
        counted_files = rd_json_u64(detail, &["fileCount"]).unwrap_or(0);
        if let Some(sources) = rd_json_string_array_from_value(detail, &["sources"]) {
            accumulate_rd_quality_source_hit(metrics, &sources.into_iter().collect());
        }
    }
    metrics.retrieval_selected_file_count = metrics
        .retrieval_selected_file_count
        .saturating_add(counted_files);
}

pub(in crate::routes::rd) fn accumulate_rd_quality_source_hit(
    metrics: &mut RdQualityObservabilityMetrics,
    sources: &BTreeSet<String>,
) {
    let has_embedding = sources.iter().any(|source| source.starts_with("embedding"));
    if has_embedding {
        metrics.embedding_hit_count = metrics.embedding_hit_count.saturating_add(1);
    }
    if sources.contains("file_summary")
        || sources.contains("embedding_summary")
        || sources.contains("embedding_context")
    {
        metrics.summary_hit_count = metrics.summary_hit_count.saturating_add(1);
    }
    if sources.contains("symbol_index") || sources.contains("embedding_symbol") {
        metrics.symbol_hit_count = metrics.symbol_hit_count.saturating_add(1);
    }
    if sources.contains("import_index") || sources.contains("embedding_import") {
        metrics.import_hit_count = metrics.import_hit_count.saturating_add(1);
    }
    if sources.contains("dependency_graph") {
        metrics.dependency_graph_hit_count = metrics.dependency_graph_hit_count.saturating_add(1);
    }
    if sources.contains("embedding_task") || sources.contains("task_memory") {
        metrics.task_memory_hit_count = metrics.task_memory_hit_count.saturating_add(1);
    }
}

fn accumulate_rd_quality_tool_governance(
    metrics: &mut RdQualityObservabilityMetrics,
    detail: &Value,
) {
    metrics.read_file_count = metrics
        .read_file_count
        .saturating_add(rd_json_u64(detail, &["readFileCount"]).unwrap_or(0));
    metrics.grep_search_count = metrics
        .grep_search_count
        .saturating_add(rd_json_u64(detail, &["grepSearchCount"]).unwrap_or(0));
    metrics.glob_search_count = metrics
        .glob_search_count
        .saturating_add(rd_json_u64(detail, &["globSearchCount"]).unwrap_or(0));
    metrics.repeated_tool_target_count =
        metrics
            .repeated_tool_target_count
            .saturating_add(rd_repeated_json_entries_count(
                detail.get("repeatedTargets"),
            ));
}

pub(in crate::routes::rd) async fn load_rd_quality_embedding_token_metrics(
    db: &SqlitePool,
    tenant_id: &str,
    tenant_wide: bool,
    user_id: &str,
    days: u32,
    repository_id: Option<&str>,
) -> Result<RdQualityTokenMetrics, sqlx::Error> {
    let mut sql = String::from(
        "SELECT \
           CAST(COALESCE(SUM(input_tokens), 0) AS INTEGER) AS input_tokens, \
           CAST(COALESCE(SUM(output_tokens), 0) AS INTEGER) AS output_tokens, \
           CAST(COALESCE(SUM(cache_creation_tokens), 0) AS INTEGER) AS cache_creation_tokens, \
           CAST(COALESCE(SUM(cache_read_tokens), 0) AS INTEGER) AS cache_read_tokens, \
           CAST(COALESCE(SUM(total_tokens), 0) AS INTEGER) AS total_tokens \
         FROM token_usage \
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?) \
           AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?)) \
           AND (provider LIKE 'rd_embedding:%' OR session_id LIKE 'rd-embedding-%')",
    );
    if repository_id.is_some() {
        sql.push_str(" AND (session_id LIKE ? OR request_id LIKE ?)");
    }
    let mut query = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(user_id)
        .bind(i64::from(days));
    if let Some(repository_id) = repository_id {
        let like = format!("%{repository_id}%");
        query = query.bind(like.clone()).bind(like);
    }
    let row = query.fetch_one(db).await?;
    Ok(RdQualityTokenMetrics {
        input_tokens: nonnegative_i64_to_u64(row.get("input_tokens")),
        output_tokens: nonnegative_i64_to_u64(row.get("output_tokens")),
        cache_creation_tokens: nonnegative_i64_to_u64(row.get("cache_creation_tokens")),
        cache_read_tokens: nonnegative_i64_to_u64(row.get("cache_read_tokens")),
        total_tokens: nonnegative_i64_to_u64(row.get("total_tokens")),
    })
}

pub(in crate::routes::rd) async fn load_rd_quality_runtime_token_metrics(
    db: &SqlitePool,
    tenant_id: &str,
    tenant_wide: bool,
    user_id: &str,
    days: u32,
    repository_id: Option<&str>,
) -> Result<RdQualityTokenMetrics, sqlx::Error> {
    let mut sql = String::from(
        "SELECT \
           CAST(COALESCE(SUM(d.input_tokens), 0) AS INTEGER) AS input_tokens, \
           CAST(COALESCE(SUM(d.output_tokens), 0) AS INTEGER) AS output_tokens, \
           CAST(COALESCE(SUM(d.cache_creation_tokens), 0) AS INTEGER) AS cache_creation_tokens, \
           CAST(COALESCE(SUM(d.cache_read_tokens), 0) AS INTEGER) AS cache_read_tokens, \
           CAST(COALESCE(SUM(d.total_tokens), 0) AS INTEGER) AS total_tokens \
         FROM ( \
           SELECT DISTINCT \
                  tu.id, \
                  tu.input_tokens, \
                  tu.output_tokens, \
                  tu.cache_creation_tokens, \
                  tu.cache_read_tokens, \
                  tu.total_tokens \
           FROM token_usage tu \
           INNER JOIN rd_tasks t \
             ON t.tenant_id = tu.tenant_id \
            AND t.user_id = tu.user_id \
            AND t.runtime_session_id = tu.session_id \
           WHERE tu.tenant_id = ? AND (? = 1 OR tu.user_id = ?) \
             AND tu.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?)) \
             AND tu.created_at >= t.started_at \
             AND tu.created_at <= COALESCE(t.completed_at, datetime(t.started_at, '+12 hours')) \
             AND tu.session_id IS NOT NULL \
             AND tu.session_id <> '' \
             AND (tu.provider NOT LIKE 'rd_embedding:%') \
             AND (tu.session_id NOT LIKE 'rd-embedding-%')",
    );
    if repository_id.is_some() {
        sql.push_str(" AND t.repository_id = ?");
    }
    sql.push_str(") d");
    let mut query = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(user_id)
        .bind(i64::from(days));
    if let Some(repository_id) = repository_id {
        query = query.bind(repository_id);
    }
    let row = query.fetch_one(db).await?;
    Ok(RdQualityTokenMetrics {
        input_tokens: nonnegative_i64_to_u64(row.get("input_tokens")),
        output_tokens: nonnegative_i64_to_u64(row.get("output_tokens")),
        cache_creation_tokens: nonnegative_i64_to_u64(row.get("cache_creation_tokens")),
        cache_read_tokens: nonnegative_i64_to_u64(row.get("cache_read_tokens")),
        total_tokens: nonnegative_i64_to_u64(row.get("total_tokens")),
    })
}

fn rd_quality_token_metrics_from_detail(detail: &Value) -> Option<RdQualityTokenMetrics> {
    let usage = detail.get("usage").filter(|value| value.is_object());
    let input_tokens = usage
        .and_then(|value| rd_json_u64(value, &["inputTokens", "input_tokens"]))
        .or_else(|| rd_json_u64(detail, &["inputTokens", "input_tokens"]))
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|value| rd_json_u64(value, &["outputTokens", "output_tokens"]))
        .or_else(|| rd_json_u64(detail, &["outputTokens", "output_tokens"]))
        .unwrap_or(0);
    let cache_creation_tokens = usage
        .and_then(|value| {
            rd_json_u64(
                value,
                &[
                    "cacheCreationTokens",
                    "cache_creation_tokens",
                    "cacheCreationInputTokens",
                    "cache_creation_input_tokens",
                ],
            )
        })
        .or_else(|| {
            rd_json_u64(
                detail,
                &[
                    "cacheCreationTokens",
                    "cache_creation_tokens",
                    "cacheCreationInputTokens",
                    "cache_creation_input_tokens",
                ],
            )
        })
        .unwrap_or(0);
    let cache_read_tokens = usage
        .and_then(|value| {
            rd_json_u64(
                value,
                &[
                    "cacheReadTokens",
                    "cache_read_tokens",
                    "cacheReadInputTokens",
                    "cache_read_input_tokens",
                ],
            )
        })
        .or_else(|| {
            rd_json_u64(
                detail,
                &[
                    "cacheReadTokens",
                    "cache_read_tokens",
                    "cacheReadInputTokens",
                    "cache_read_input_tokens",
                ],
            )
        })
        .unwrap_or(0);
    let mut total_tokens = usage
        .and_then(|value| rd_json_u64(value, &["totalTokens", "total_tokens"]))
        .or_else(|| rd_json_u64(detail, &["totalTokens", "total_tokens"]))
        .unwrap_or(0);
    if total_tokens == 0 {
        total_tokens = input_tokens
            .saturating_add(output_tokens)
            .saturating_add(cache_creation_tokens)
            .saturating_add(cache_read_tokens);
    }
    (total_tokens > 0).then_some(RdQualityTokenMetrics {
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        total_tokens,
    })
}

fn rd_json_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        let Some(candidate) = value.get(*key) else {
            continue;
        };
        if let Some(number) = candidate.as_u64() {
            return Some(number);
        }
        if let Some(number) = candidate.as_i64() {
            return Some(nonnegative_i64_to_u64(number));
        }
        if let Some(number) = candidate.as_f64() {
            return Some(number.max(0.0).round() as u64);
        }
        if let Some(raw) = candidate.as_str() {
            if let Ok(number) = raw.trim().parse::<u64>() {
                return Some(number);
            }
        }
    }
    None
}

fn rd_json_string_array_from_value(value: &Value, keys: &[&str]) -> Option<Vec<String>> {
    for key in keys {
        let Some(array) = value.get(*key).and_then(Value::as_array) else {
            continue;
        };
        let values = array
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        return Some(values);
    }
    None
}

fn rd_repeated_json_entries_count(value: Option<&Value>) -> u64 {
    let Some(Value::Array(items)) = value else {
        return 0;
    };
    items.iter().fold(0u64, |sum, item| {
        let count = rd_json_u64(item, &["count"]).unwrap_or(1);
        sum.saturating_add(count.saturating_sub(1).max(1))
    })
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
