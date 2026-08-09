//! Repository retrieval evidence and cache observability events.

use super::retrieval::{RdRepositoryRetrievalContext, RdRepositoryRetrievalEvidenceFile};
use super::*;

pub(super) async fn maybe_record_rd_retrieval_evidence(
    state: &AppState,
    claims: &Claims,
    task_id: Option<&str>,
    repository_id: &str,
    context_profile: RdContextProfile,
    retrieval: &RdRepositoryRetrievalContext,
) {
    let Some(task_id) = task_id else {
        return;
    };

    let files = retrieval
        .evidence
        .iter()
        .take(64)
        .map(RdRepositoryRetrievalEvidenceFile::to_json)
        .collect::<Vec<_>>();
    let mut source_set = retrieval
        .evidence
        .iter()
        .flat_map(|file| file.sources.iter().cloned())
        .collect::<BTreeSet<_>>();
    source_set.extend(retrieval.observability_sources.iter().cloned());
    let sources = source_set.into_iter().collect::<Vec<_>>();

    let index_cache_metrics = match load_rd_quality_index_cache_metrics(
        &state.db,
        &claims.tenant_id,
        Some(&claims.sub),
        Some(repository_id),
    )
    .await
    {
        Ok(metrics) => metrics,
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                task_id = %task_id,
                repository_id = %repository_id,
                "failed to load RD index cache metrics for task cache report: {}",
                error
            );
            RdQualityIndexCacheMetrics::default()
        }
    };
    let embedding_model =
        match load_rd_quality_embedding_model(&state.db, &claims.tenant_id, Some(repository_id))
            .await
        {
            Ok(model) => model,
            Err(error) => {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    task_id = %task_id,
                    repository_id = %repository_id,
                    "failed to load RD embedding model for task cache report: {}",
                    error
                );
                None
            }
        };
    let cache_usage_detail = rd_context_cache_usage_detail(
        repository_id,
        context_profile,
        retrieval,
        &sources,
        &files,
        embedding_model,
        &index_cache_metrics,
    );
    if !retrieval.evidence.is_empty()
        || retrieval.task_memory_hit_count > 0
        || retrieval.context_summary_hit_count > 0
    {
        let mut evidence_detail = cache_usage_detail.clone();
        if let Some(map) = evidence_detail.as_object_mut() {
            map.insert("fileCount".to_string(), json!(files.len()));
            map.insert("files".to_string(), json!(files.clone()));
        }
        if let Err(error) = record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "context_retrieval_evidence",
            "completed",
            "已生成结构化召回证据",
            evidence_detail,
        )
        .await
        {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                task_id = %task_id,
                repository_id = %repository_id,
                "failed to record RD retrieval evidence event: {}",
                error
            );
        }
    }
    if let Err(error) = record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "context_cache_usage",
        "completed",
        "上下文缓存与召回命中已记录",
        cache_usage_detail,
    )
    .await
    {
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            task_id = %task_id,
            repository_id = %repository_id,
            "failed to record RD context cache usage event: {}",
            error
        );
    }
}

fn rd_context_cache_usage_detail(
    repository_id: &str,
    context_profile: RdContextProfile,
    retrieval: &RdRepositoryRetrievalContext,
    sources: &[String],
    files: &[Value],
    embedding_model: Option<String>,
    index_cache_metrics: &RdQualityIndexCacheMetrics,
) -> Value {
    let mut counts = RdQualityObservabilityMetrics::default();
    for evidence in &retrieval.evidence {
        accumulate_rd_quality_source_hit(&mut counts, &evidence.sources);
    }
    counts.task_memory_hit_count = counts
        .task_memory_hit_count
        .saturating_add(retrieval.task_memory_hit_count);
    counts.summary_hit_count = counts
        .summary_hit_count
        .saturating_add(retrieval.context_summary_hit_count);
    if retrieval
        .observability_sources
        .iter()
        .any(|source| source.starts_with("embedding"))
    {
        counts.embedding_hit_count = counts.embedding_hit_count.saturating_add(
            retrieval
                .task_memory_hit_count
                .saturating_add(retrieval.context_summary_hit_count)
                .max(1),
        );
    }
    let lexical_hits = counts
        .summary_hit_count
        .saturating_add(counts.symbol_hit_count)
        .saturating_add(counts.import_hit_count)
        .saturating_add(counts.dependency_graph_hit_count);
    let retrieval_reused_chunks = counts
        .embedding_hit_count
        .saturating_add(counts.summary_hit_count)
        .saturating_add(counts.symbol_hit_count)
        .saturating_add(counts.import_hit_count)
        .saturating_add(counts.dependency_graph_hit_count)
        .saturating_add(counts.task_memory_hit_count);
    let cache_reused_chunks = index_cache_metrics
        .embedding_reused_chunk_count
        .saturating_add(index_cache_metrics.file_summary_reused_count)
        .max(retrieval_reused_chunks);
    let cache_regenerated_chunks = index_cache_metrics
        .embedding_regenerated_chunk_count
        .saturating_add(index_cache_metrics.file_summary_regenerated_count);
    let cache_miss_reasons = rd_context_cache_miss_reasons(
        files.len(),
        sources,
        &counts,
        lexical_hits,
        cache_reused_chunks,
        cache_regenerated_chunks,
        embedding_model.as_deref(),
    );
    let retrieval_estimated_tokens_saved = files
        .len()
        .saturating_mul(1_800)
        .saturating_add(
            usize::try_from(counts.task_memory_hit_count)
                .unwrap_or(usize::MAX)
                .saturating_mul(1_200),
        )
        .saturating_add(
            usize::try_from(counts.summary_hit_count)
                .unwrap_or(usize::MAX)
                .saturating_mul(1_000),
        );
    let estimated_tokens_saved = index_cache_metrics
        .estimated_tokens_saved
        .max(u64::try_from(retrieval_estimated_tokens_saved).unwrap_or(u64::MAX));
    json!({
        "repositoryId": repository_id,
        "contextProfile": context_profile.as_str(),
        "contextProfileName": context_profile.display_name(),
        "strategy": retrieval.strategy.clone(),
        "terms": retrieval.terms.clone(),
        "selectedFiles": files.len(),
        "embeddingEnabled": sources.iter().any(|source| source.starts_with("embedding")),
        "embeddingModel": embedding_model,
        "embeddingHits": counts.embedding_hit_count,
        "lexicalHits": lexical_hits,
        "summaryHits": counts.summary_hit_count,
        "symbolHits": counts.symbol_hit_count,
        "importHits": counts.import_hit_count,
        "dependencyGraphHits": counts.dependency_graph_hit_count,
        "taskMemoryHits": counts.task_memory_hit_count,
        "mergedCandidates": retrieval.evidence.len(),
        "staleFiles": 0,
        "cacheSources": sources,
        "cacheMissReasons": cache_miss_reasons,
        "files": files.iter().take(24).cloned().collect::<Vec<_>>(),
        "readFileCount": 0,
        "readFileCountAtPlanTime": 0,
        "cacheReusedChunks": cache_reused_chunks,
        "cacheRegeneratedChunks": cache_regenerated_chunks,
        "estimatedTokensSaved": estimated_tokens_saved,
        "effectFirst": true,
        "realFileVerificationRequired": true,
        "note": "缓存、embedding 和索引用于定位候选上下文；修改代码、Review 结论和行级判断仍必须读取真实文件核对。",
    })
}

fn rd_context_cache_miss_reasons(
    selected_files: usize,
    sources: &[String],
    counts: &RdQualityObservabilityMetrics,
    lexical_hits: u64,
    cache_reused_chunks: u64,
    cache_regenerated_chunks: u64,
    embedding_model: Option<&str>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if selected_files == 0 {
        reasons.push(
            "本轮没有产生结构化召回候选；通常是仓库索引尚未建立、问题过短/过泛，或当前仓库没有可匹配摘要/符号。".to_string(),
        );
    }
    if !sources.iter().any(|source| source.starts_with("embedding")) {
        reasons.push(if embedding_model.is_some() {
            "未命中 embedding：语义相似度未达到阈值，或 embedding 索引仍在构建/刚被文件变更刷新。".to_string()
        } else {
            "未命中 embedding：当前租户未配置 rd 场景的 embedding 模型，已降级使用词法、摘要、symbol/import 索引。".to_string()
        });
    }
    if lexical_hits == 0 {
        reasons.push(
            "未命中词法/symbol/import/依赖图索引：问题关键词与当前索引项重合度低，runtime 需要通过少量真实文件继续核对。".to_string(),
        );
    }
    if counts.summary_hit_count == 0 {
        reasons.push(
            "未命中文件/目录摘要：可能是摘要尚未构建，或用户问题没有匹配到摘要文本。".to_string(),
        );
    }
    if counts.task_memory_hit_count == 0 {
        reasons.push("未命中历史任务记忆：当前问题没有找到相似已完成代码任务。".to_string());
    }
    if cache_reused_chunks == 0 && cache_regenerated_chunks > 0 {
        reasons.push(
            "本轮索引以重建 chunk 为主：可能是首次构建、切换 embedding 模型，或文件内容发生变化。"
                .to_string(),
        );
    }
    reasons.into_iter().take(6).collect()
}
