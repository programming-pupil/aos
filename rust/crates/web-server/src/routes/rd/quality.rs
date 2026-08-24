//! Quality snapshot and observability aggregation for AOS Code Studio.

use super::*;

mod metrics;
pub(super) use metrics::{
    accumulate_rd_quality_source_hit, load_rd_quality_embedding_model,
    load_rd_quality_index_cache_metrics, RdQualityIndexCacheMetrics, RdQualityObservabilityMetrics,
};
use metrics::{
    load_rd_quality_embedding_metrics, load_rd_quality_embedding_token_metrics,
    load_rd_quality_observability_metrics, load_rd_quality_runtime_token_metrics,
    load_rd_quality_summary_metrics, RdQualitySummaryCacheMetrics, RdQualityTokenMetrics,
};

#[derive(Debug, Deserialize)]
pub(super) struct RdQualityQuery {
    days: Option<u32>,
    repository_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdFailedCommandDto {
    command: String,
    failure_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdQualitySummaryDto {
    days: u32,
    repository_id: Option<String>,
    task_count: u64,
    completed_count: u64,
    failed_count: u64,
    running_count: u64,
    waiting_approval_count: u64,
    cancelled_count: u64,
    success_rate: f64,
    avg_task_duration_ms: Option<f64>,
    diff_count: u64,
    pending_diff_count: u64,
    applied_diff_count: u64,
    test_run_count: u64,
    passed_test_count: u64,
    failed_test_count: u64,
    test_pass_rate: f64,
    candidate_worktree_started_count: u64,
    candidate_worktree_diff_count: u64,
    candidate_worktree_no_diff_count: u64,
    candidate_worktree_failed_count: u64,
    candidate_context_synced_count: u64,
    candidate_context_sync_failed_count: u64,
    candidate_diff_bytes: u64,
    diff_check_passed_count: u64,
    diff_check_failed_count: u64,
    diff_check_skipped_count: u64,
    diff_check_pass_rate: f64,
    diff_repair_passed_count: u64,
    diff_repair_failed_count: u64,
    review_agent_run_count: u64,
    review_findings_count: u64,
    review_file_ref_count: u64,
    review_line_ref_count: u64,
    embedding_store_enabled: bool,
    embedding_model: Option<String>,
    embedding_chunk_count: u64,
    embedding_context_summary_chunk_count: u64,
    embedding_file_summary_chunk_count: u64,
    embedding_symbol_chunk_count: u64,
    embedding_import_chunk_count: u64,
    embedding_task_chunk_count: u64,
    repository_summary_count: u64,
    directory_summary_count: u64,
    file_summary_count: u64,
    llm_summary_count: u64,
    stale_summary_count: u64,
    retrieval_evidence_event_count: u64,
    cache_usage_event_count: u64,
    retrieval_selected_file_count: u64,
    cache_hit_rate: f64,
    embedding_hit_count: u64,
    summary_hit_count: u64,
    symbol_hit_count: u64,
    import_hit_count: u64,
    dependency_graph_hit_count: u64,
    task_memory_hit_count: u64,
    read_file_count: u64,
    grep_search_count: u64,
    glob_search_count: u64,
    repeated_tool_target_count: u64,
    cache_reused_chunk_count: u64,
    cache_regenerated_chunk_count: u64,
    embedding_reused_chunk_count: u64,
    embedding_regenerated_chunk_count: u64,
    embedding_pruned_chunk_count: u64,
    file_summary_reused_count: u64,
    file_summary_regenerated_count: u64,
    estimated_tokens_saved: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    total_tokens: u64,
    embedding_tokens: u64,
    context_planner_tokens: u64,
    runtime_tokens: u64,
    top_failed_commands: Vec<RdFailedCommandDto>,
    generated_at: String,
}

pub(super) async fn get_quality_summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<RdQualityQuery>,
) -> Result<Json<RdQualitySummaryDto>, AppError> {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let days = query.days.unwrap_or(30).clamp(1, 365);
    let repository_id = query
        .repository_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if let Some(repository_id) = repository_id.as_deref() {
        let accessible = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM gitlab_projects
             WHERE tenant_id = ? AND id = ? AND (? = 1 OR user_id = ?)",
        )
        .bind(&claims.tenant_id)
        .bind(repository_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .fetch_one(&state.db)
        .await?;
        if accessible == 0 {
            return Err(AppError::NotFound("repository not found".to_string()));
        }
    }

    let mut task_where =
        "tenant_id = ? AND (? = 1 OR user_id = ?) AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))"
            .to_string();
    if repository_id.is_some() {
        task_where.push_str(" AND repository_id = ?");
    }
    let task_sql = format!(
        "SELECT \
         COUNT(*) AS task_count, \
         CAST(COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS completed_count, \
         CAST(COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS failed_count, \
         CAST(COALESCE(SUM(CASE WHEN status IN ('queued','running') THEN 1 ELSE 0 END), 0) AS INTEGER) AS running_count, \
         CAST(COALESCE(SUM(CASE WHEN status = 'waiting_approval' THEN 1 ELSE 0 END), 0) AS INTEGER) AS waiting_approval_count, \
         CAST(COALESCE(SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END), 0) AS INTEGER) AS cancelled_count, \
         CAST(AVG(CASE WHEN started_at IS NOT NULL AND completed_at IS NOT NULL THEN ((julianday(completed_at) - julianday(started_at)) * 86400000000) / 1000 ELSE NULL END) AS DOUBLE) AS avg_task_duration_ms \
         FROM rd_tasks WHERE {task_where}"
    );
    let mut task_query = sqlx::query(sqlx::AssertSqlSafe(task_sql))
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days));
    if let Some(repo_id) = &repository_id {
        task_query = task_query.bind(repo_id);
    }
    let task_row = task_query.fetch_one(&state.db).await?;

    let mut change_where =
        "c.tenant_id = ? AND (? = 1 OR t.user_id = ?) AND c.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))"
            .to_string();
    if repository_id.is_some() {
        change_where.push_str(" AND c.repository_id = ?");
    }
    let change_sql = format!(
        "SELECT \
         COUNT(*) AS diff_count, \
         CAST(COALESCE(SUM(CASE WHEN c.applied = 0 THEN 1 ELSE 0 END), 0) AS INTEGER) AS pending_diff_count, \
         CAST(COALESCE(SUM(CASE WHEN c.applied = 1 THEN 1 ELSE 0 END), 0) AS INTEGER) AS applied_diff_count \
         FROM rd_file_changes c \
         INNER JOIN rd_tasks t ON t.id = c.task_id AND t.tenant_id = c.tenant_id \
         WHERE {change_where}"
    );
    let mut change_query = sqlx::query(sqlx::AssertSqlSafe(change_sql))
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days));
    if let Some(repo_id) = &repository_id {
        change_query = change_query.bind(repo_id);
    }
    let change_row = change_query.fetch_one(&state.db).await?;

    let mut test_where =
        "r.tenant_id = ? AND (? = 1 OR t.user_id = ?) AND r.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))"
            .to_string();
    if repository_id.is_some() {
        test_where.push_str(" AND r.repository_id = ?");
    }
    let test_sql = format!(
        "SELECT \
         COUNT(*) AS test_run_count, \
         CAST(COALESCE(SUM(CASE WHEN r.status = 'passed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS passed_test_count, \
         CAST(COALESCE(SUM(CASE WHEN r.status IN ('failed','timeout') THEN 1 ELSE 0 END), 0) AS INTEGER) AS failed_test_count \
         FROM rd_test_runs r \
         INNER JOIN rd_tasks t ON t.id = r.task_id AND t.tenant_id = r.tenant_id \
         WHERE {test_where}"
    );
    let mut test_query = sqlx::query(sqlx::AssertSqlSafe(test_sql))
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days));
    if let Some(repo_id) = &repository_id {
        test_query = test_query.bind(repo_id);
    }
    let test_row = test_query.fetch_one(&state.db).await?;

    let top_failed_sql = format!(
        "SELECT r.command, COUNT(*) AS failure_count \
         FROM rd_test_runs r \
         INNER JOIN rd_tasks t ON t.id = r.task_id AND t.tenant_id = r.tenant_id \
         WHERE {test_where} AND r.status IN ('failed','timeout') \
         GROUP BY r.command \
         ORDER BY failure_count DESC, MAX(r.created_at) DESC \
         LIMIT 5"
    );
    let mut top_failed_query = sqlx::query(sqlx::AssertSqlSafe(top_failed_sql))
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days));
    if let Some(repo_id) = &repository_id {
        top_failed_query = top_failed_query.bind(repo_id);
    }
    let top_failed_rows = top_failed_query.fetch_all(&state.db).await?;

    let mut metric_where =
        "m.tenant_id = ? AND (? = 1 OR t.user_id = ?) AND m.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))"
            .to_string();
    if repository_id.is_some() {
        metric_where.push_str(" AND m.repository_id = ?");
    }
    let metric_sql = format!(
        "SELECT m.metric_name, CAST(COALESCE(SUM(m.metric_value), 0) AS DOUBLE) AS metric_value \
         FROM rd_quality_metrics m \
         INNER JOIN rd_tasks t ON t.id = m.task_id AND t.tenant_id = m.tenant_id \
         WHERE {metric_where} \
           AND m.metric_name IN (\
             'candidate_worktree_started',\
             'candidate_worktree_diff_generated',\
             'candidate_worktree_no_diff',\
             'candidate_worktree_failed',\
             'candidate_context_synced',\
             'candidate_context_sync_failed',\
             'candidate_worktree_diff_bytes',\
             'diff_check_passed',\
             'diff_check_failed',\
             'diff_check_skipped',\
             'diff_repair_passed',\
             'diff_repair_failed',\
             'review_agent_run',\
             'review_findings_count',\
             'review_file_ref_count',\
             'review_line_ref_count'\
           ) \
         GROUP BY m.metric_name"
    );
    let mut metric_query = sqlx::query(sqlx::AssertSqlSafe(metric_sql))
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days));
    if let Some(repo_id) = &repository_id {
        metric_query = metric_query.bind(repo_id);
    }
    let metric_rows = metric_query.fetch_all(&state.db).await?;
    let metric_totals = metric_rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("metric_name"),
                row.get::<f64, _>("metric_value"),
            )
        })
        .collect::<HashMap<_, _>>();

    let task_count = nonnegative_i64_to_u64(task_row.get("task_count"));
    let completed_count = nonnegative_i64_to_u64(task_row.get("completed_count"));
    let test_run_count = nonnegative_i64_to_u64(test_row.get("test_run_count"));
    let passed_test_count = nonnegative_i64_to_u64(test_row.get("passed_test_count"));
    let diff_check_passed_count = metric_count(&metric_totals, "diff_check_passed");
    let diff_check_failed_count = metric_count(&metric_totals, "diff_check_failed");
    let embedding_metrics =
        load_rd_quality_embedding_metrics(&state, &claims, repository_id.as_deref()).await;
    let index_cache_metrics = match load_rd_quality_index_cache_metrics(
        &state.db,
        &claims.tenant_id,
        (!tenant_wide).then_some(claims.sub.as_str()),
        repository_id.as_deref(),
    )
    .await
    {
        Ok(metrics) => metrics,
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                repository_id = ?repository_id,
                "failed to load RD quality index cache metrics: {}",
                error
            );
            RdQualityIndexCacheMetrics::default()
        }
    };
    let summary_metrics = match load_rd_quality_summary_metrics(
        &state.db,
        &claims.tenant_id,
        (!tenant_wide).then_some(claims.sub.as_str()),
        repository_id.as_deref(),
    )
    .await
    {
        Ok(metrics) => metrics,
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                repository_id = ?repository_id,
                "failed to load RD quality summary cache metrics: {}",
                error
            );
            RdQualitySummaryCacheMetrics::default()
        }
    };
    let mut observability_metrics = match load_rd_quality_observability_metrics(
        &state.db,
        &claims,
        days,
        repository_id.as_deref(),
    )
    .await
    {
        Ok(metrics) => metrics,
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                repository_id = ?repository_id,
                "failed to load RD quality cache/token/tool observability metrics: {}",
                error
            );
            RdQualityObservabilityMetrics::default()
        }
    };
    let embedding_token_metrics = match load_rd_quality_embedding_token_metrics(
        &state.db,
        &claims.tenant_id,
        tenant_wide,
        &claims.sub,
        days,
        repository_id.as_deref(),
    )
    .await
    {
        Ok(metrics) => metrics,
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                repository_id = ?repository_id,
                "failed to load RD embedding token usage metrics: {}",
                error
            );
            RdQualityTokenMetrics::default()
        }
    };
    let runtime_billing_token_metrics = match load_rd_quality_runtime_token_metrics(
        &state.db,
        &claims.tenant_id,
        tenant_wide,
        &claims.sub,
        days,
        repository_id.as_deref(),
    )
    .await
    {
        Ok(metrics) => metrics,
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                repository_id = ?repository_id,
                "failed to load RD runtime billing token usage metrics: {}",
                error
            );
            RdQualityTokenMetrics::default()
        }
    };
    if runtime_billing_token_metrics.total_tokens > 0 {
        observability_metrics.tokens.input_tokens = observability_metrics
            .tokens
            .input_tokens
            .saturating_sub(observability_metrics.runtime_token_metrics.input_tokens);
        observability_metrics.tokens.output_tokens = observability_metrics
            .tokens
            .output_tokens
            .saturating_sub(observability_metrics.runtime_token_metrics.output_tokens);
        observability_metrics.tokens.cache_creation_tokens = observability_metrics
            .tokens
            .cache_creation_tokens
            .saturating_sub(
                observability_metrics
                    .runtime_token_metrics
                    .cache_creation_tokens,
            );
        observability_metrics.tokens.cache_read_tokens = observability_metrics
            .tokens
            .cache_read_tokens
            .saturating_sub(
                observability_metrics
                    .runtime_token_metrics
                    .cache_read_tokens,
            );
        observability_metrics.tokens.total_tokens = observability_metrics
            .tokens
            .total_tokens
            .saturating_sub(observability_metrics.runtime_token_metrics.total_tokens);
        observability_metrics.runtime_tokens = runtime_billing_token_metrics.total_tokens;
        observability_metrics
            .tokens
            .add(&runtime_billing_token_metrics);
    }
    observability_metrics.tokens.add(&embedding_token_metrics);

    Ok(Json(RdQualitySummaryDto {
        days,
        repository_id,
        task_count,
        completed_count,
        failed_count: nonnegative_i64_to_u64(task_row.get("failed_count")),
        running_count: nonnegative_i64_to_u64(task_row.get("running_count")),
        waiting_approval_count: nonnegative_i64_to_u64(task_row.get("waiting_approval_count")),
        cancelled_count: nonnegative_i64_to_u64(task_row.get("cancelled_count")),
        success_rate: ratio(completed_count, task_count),
        avg_task_duration_ms: task_row.get("avg_task_duration_ms"),
        diff_count: nonnegative_i64_to_u64(change_row.get("diff_count")),
        pending_diff_count: nonnegative_i64_to_u64(change_row.get("pending_diff_count")),
        applied_diff_count: nonnegative_i64_to_u64(change_row.get("applied_diff_count")),
        test_run_count,
        passed_test_count,
        failed_test_count: nonnegative_i64_to_u64(test_row.get("failed_test_count")),
        test_pass_rate: ratio(passed_test_count, test_run_count),
        candidate_worktree_started_count: metric_count(
            &metric_totals,
            "candidate_worktree_started",
        ),
        candidate_worktree_diff_count: metric_count(
            &metric_totals,
            "candidate_worktree_diff_generated",
        ),
        candidate_worktree_no_diff_count: metric_count(
            &metric_totals,
            "candidate_worktree_no_diff",
        ),
        candidate_worktree_failed_count: metric_count(&metric_totals, "candidate_worktree_failed"),
        candidate_context_synced_count: metric_count(&metric_totals, "candidate_context_synced"),
        candidate_context_sync_failed_count: metric_count(
            &metric_totals,
            "candidate_context_sync_failed",
        ),
        candidate_diff_bytes: metric_count(&metric_totals, "candidate_worktree_diff_bytes"),
        diff_check_passed_count,
        diff_check_failed_count,
        diff_check_skipped_count: metric_count(&metric_totals, "diff_check_skipped"),
        diff_check_pass_rate: ratio(
            diff_check_passed_count,
            diff_check_passed_count + diff_check_failed_count,
        ),
        diff_repair_passed_count: metric_count(&metric_totals, "diff_repair_passed"),
        diff_repair_failed_count: metric_count(&metric_totals, "diff_repair_failed"),
        review_agent_run_count: metric_count(&metric_totals, "review_agent_run"),
        review_findings_count: metric_count(&metric_totals, "review_findings_count"),
        review_file_ref_count: metric_count(&metric_totals, "review_file_ref_count"),
        review_line_ref_count: metric_count(&metric_totals, "review_line_ref_count"),
        embedding_store_enabled: embedding_metrics.store_enabled,
        embedding_model: embedding_metrics.model,
        embedding_chunk_count: embedding_metrics.total_count,
        embedding_context_summary_chunk_count: embedding_metrics.context_summary_count,
        embedding_file_summary_chunk_count: embedding_metrics.file_summary_count,
        embedding_symbol_chunk_count: embedding_metrics.symbol_count,
        embedding_import_chunk_count: embedding_metrics.import_count,
        embedding_task_chunk_count: embedding_metrics.task_count,
        repository_summary_count: summary_metrics.repository_summary_count,
        directory_summary_count: summary_metrics.directory_summary_count,
        file_summary_count: summary_metrics.file_summary_count,
        llm_summary_count: summary_metrics.llm_summary_count,
        stale_summary_count: summary_metrics.stale_summary_count,
        retrieval_evidence_event_count: observability_metrics.retrieval_evidence_event_count,
        cache_usage_event_count: observability_metrics.cache_usage_event_count,
        retrieval_selected_file_count: observability_metrics.retrieval_selected_file_count,
        cache_hit_rate: ratio(
            observability_metrics
                .embedding_hit_count
                .saturating_add(observability_metrics.summary_hit_count)
                .saturating_add(observability_metrics.symbol_hit_count)
                .saturating_add(observability_metrics.import_hit_count)
                .saturating_add(observability_metrics.dependency_graph_hit_count)
                .saturating_add(observability_metrics.task_memory_hit_count),
            observability_metrics.retrieval_selected_file_count.max(1),
        )
        .min(1.0),
        embedding_hit_count: observability_metrics.embedding_hit_count,
        summary_hit_count: observability_metrics.summary_hit_count,
        symbol_hit_count: observability_metrics.symbol_hit_count,
        import_hit_count: observability_metrics.import_hit_count,
        dependency_graph_hit_count: observability_metrics.dependency_graph_hit_count,
        task_memory_hit_count: observability_metrics.task_memory_hit_count,
        read_file_count: observability_metrics.read_file_count,
        grep_search_count: observability_metrics.grep_search_count,
        glob_search_count: observability_metrics.glob_search_count,
        repeated_tool_target_count: observability_metrics.repeated_tool_target_count,
        cache_reused_chunk_count: index_cache_metrics
            .embedding_reused_chunk_count
            .saturating_add(index_cache_metrics.file_summary_reused_count),
        cache_regenerated_chunk_count: index_cache_metrics
            .embedding_regenerated_chunk_count
            .saturating_add(index_cache_metrics.file_summary_regenerated_count),
        embedding_reused_chunk_count: index_cache_metrics.embedding_reused_chunk_count,
        embedding_regenerated_chunk_count: index_cache_metrics.embedding_regenerated_chunk_count,
        embedding_pruned_chunk_count: index_cache_metrics.embedding_pruned_chunk_count,
        file_summary_reused_count: index_cache_metrics.file_summary_reused_count,
        file_summary_regenerated_count: index_cache_metrics.file_summary_regenerated_count,
        estimated_tokens_saved: index_cache_metrics.estimated_tokens_saved,
        input_tokens: observability_metrics.tokens.input_tokens,
        output_tokens: observability_metrics.tokens.output_tokens,
        cache_creation_tokens: observability_metrics.tokens.cache_creation_tokens,
        cache_read_tokens: observability_metrics.tokens.cache_read_tokens,
        total_tokens: observability_metrics.tokens.total_tokens,
        embedding_tokens: embedding_token_metrics.total_tokens,
        context_planner_tokens: observability_metrics.context_planner_tokens,
        runtime_tokens: observability_metrics.runtime_tokens,
        top_failed_commands: top_failed_rows
            .iter()
            .map(|row| RdFailedCommandDto {
                command: row.get("command"),
                failure_count: nonnegative_i64_to_u64(row.get("failure_count")),
            })
            .collect(),
        generated_at: chrono::Utc::now().to_rfc3339(),
    }))
}
