//! Semantic retrieval and RD embedding usage helpers.

use super::*;

pub(super) fn rd_semantic_hit_metadata_hint(metadata: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(language) = metadata.get("language").and_then(Value::as_str) {
        if !language.trim().is_empty() {
            parts.push(format!("lang={language}"));
        }
    }
    if let Some(size_bytes) = metadata.get("sizeBytes").and_then(Value::as_u64) {
        parts.push(format!("bytes={size_bytes}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

pub(super) async fn rd_semantic_repository_search(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    prompt: &str,
    top_k: usize,
) -> Vec<RdEmbeddingSearchHit> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Vec::new();
    }
    let Some(store) = state.rd_embedding_store.as_ref().cloned() else {
        return Vec::new();
    };
    let candidates = resolve_rd_embedding_candidates(state, &claims.tenant_id).await;
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut backfill_scheduled = false;
    let mut api_failure: Option<(RdEmbeddingApiKey, String)> = None;
    for candidate in candidates {
        let model = candidate.vector_space_id.clone();
        let tenant_id = claims.tenant_id.clone();
        let repo_id = repository_id.to_string();
        let count_store = store.clone();
        let chunk_count = tokio::task::spawn_blocking(move || {
            count_store.repository_chunk_count(&tenant_id, &repo_id, &model)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(0);
        if chunk_count == 0 {
            if !backfill_scheduled {
                schedule_rd_repository_embedding_index(
                    state.clone(),
                    claims.tenant_id.clone(),
                    claims.sub.clone(),
                    repository_id.to_string(),
                    "lazy_backfill",
                );
                backfill_scheduled = true;
            }
            continue;
        }

        let query_texts = vec![truncate_text(prompt, 6_000)];
        let embed_future = rd_embed_texts_with_candidate(&candidate, &query_texts);
        let embedding_result = match timeout(
            Duration::from_secs(RD_EMBEDDING_QUERY_TIMEOUT_SECS),
            embed_future,
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    repository_id = %repository_id,
                    model = %candidate.model,
                    error = %error,
                    "RD semantic query embedding failed; falling back to lexical retrieval"
                );
                if !candidate.is_local {
                    api_failure = Some((candidate.clone(), error.to_string()));
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    repository_id = %repository_id,
                    model = %candidate.model,
                    timeout_secs = RD_EMBEDDING_QUERY_TIMEOUT_SECS,
                    "RD semantic query embedding timed out; falling back to lexical retrieval"
                );
                if !candidate.is_local {
                    api_failure = Some((
                        candidate.clone(),
                        format!(
                            "RD semantic query embedding timed out after {RD_EMBEDDING_QUERY_TIMEOUT_SECS}s"
                        ),
                    ));
                }
                continue;
            }
        };
        if candidate.is_local {
            if let Some((failed, error)) = api_failure.as_ref() {
                if let Err(alert_error) =
                    crate::nl2sql::embedding_failover::record_embedding_fallback_alert_for_profile(
                        &state.db,
                        &claims.tenant_id,
                        RD_SCENARIO,
                        &failed.vector_space_id,
                        &failed.provider,
                        &failed.model,
                        error,
                    )
                    .await
                {
                    tracing::warn!(tenant_id = %claims.tenant_id, error = %alert_error, "failed to persist RD embedding fallback alert");
                }
            }
        } else if let Err(error) =
            crate::nl2sql::embedding_failover::resolve_embedding_fallback_alert_for_profile(
                &state.db,
                &claims.tenant_id,
                RD_SCENARIO,
                &candidate.vector_space_id,
            )
            .await
        {
            tracing::warn!(tenant_id = %claims.tenant_id, error = %error, "failed to resolve RD embedding fallback alert");
        }
        record_rd_embedding_usage(
            state,
            &claims.tenant_id,
            &claims.sub,
            &format!("rd-embedding-query:{repository_id}"),
            Some(repository_id.to_string()),
            &candidate,
            embedding_result.usage.as_ref(),
            1,
            prompt.chars().count(),
        )
        .await;
        let Some(query_vector) = embedding_result.vectors.into_iter().next() else {
            continue;
        };
        let search_store = store.clone();
        let tenant_id = claims.tenant_id.clone();
        let repo_id = repository_id.to_string();
        let model = candidate.vector_space_id.clone();
        let hits = tokio::task::spawn_blocking(move || {
            search_store.search_repository(
                &tenant_id,
                &repo_id,
                &model,
                &query_vector,
                top_k.saturating_mul(2).max(top_k),
            )
        })
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_else(Vec::new);
        return fuse_rd_semantic_hits(vec![hits], top_k);
    }

    Vec::new()
}

fn fuse_rd_semantic_hits(
    ranked_hit_sets: Vec<Vec<RdEmbeddingSearchHit>>,
    top_k: usize,
) -> Vec<RdEmbeddingSearchHit> {
    const RRF_K: f32 = 60.0;
    let mut fused = HashMap::<String, (RdEmbeddingSearchHit, f32, usize)>::new();
    for hits in ranked_hit_sets {
        for (rank, hit) in hits.into_iter().enumerate() {
            let rank_score = 1.0 / (RRF_K + rank as f32 + 1.0);
            let semantic_tiebreaker = hit.score.clamp(0.0, 1.0) * 0.001;
            let contribution = rank_score + semantic_tiebreaker;
            fused
                .entry(hit.chunk_id.clone())
                .and_modify(|(best, score, profile_count)| {
                    *score += contribution;
                    *profile_count = profile_count.saturating_add(1);
                    if hit.score > best.score {
                        *best = hit.clone();
                    }
                })
                .or_insert((hit, contribution, 1));
        }
    }
    let mut hits = fused.into_values().collect::<Vec<_>>();
    hits.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(top_k);
    let max_fusion_score = hits
        .first()
        .map_or(1.0, |(_, score, _)| *score)
        .max(f32::EPSILON);
    hits.into_iter()
        .map(|(mut hit, fusion_score, profile_count)| {
            let rank_confidence = (fusion_score / max_fusion_score).clamp(0.0, 1.0);
            let consensus_boost = (profile_count.saturating_sub(1) as f32 * 0.04).min(0.08);
            hit.score =
                (hit.score.clamp(0.0, 1.0) * 0.85 + rank_confidence * 0.15 + consensus_boost)
                    .clamp(0.0, 1.0);
            hit
        })
        .collect()
}

pub(in crate::routes::rd) async fn resolve_rd_embedding_candidates(
    state: &AppState,
    tenant_id: &str,
) -> Vec<RdEmbeddingApiKey> {
    #[cfg(not(feature = "nl2sql"))]
    {
        let _ = (state, tenant_id);
        return Vec::new();
    }
    #[cfg(feature = "nl2sql")]
    {
        let profiles =
            crate::nl2sql::resolve_embedding_profiles(&state.db, tenant_id, Some(RD_SCENARIO))
                .await;
        let mut configs = Vec::with_capacity(2);
        if let Some(api) = profiles.api {
            configs.push(api);
        }
        configs.push(profiles.local);
        configs
            .into_iter()
            .map(|config| {
                let is_local = config.profile_kind == crate::nl2sql::EmbeddingProfileKind::Local;
                RdEmbeddingApiKey {
                    id: config.key_id.clone(),
                    provider: config.provider.clone(),
                    base_url: config.base_url.clone(),
                    model: config.model.clone(),
                    vector_space_id: config.profile_id(tenant_id),
                    dimensions: config.dimensions,
                    is_local,
                    api_key: config.api_key,
                }
            })
            .collect()
    }
}

async fn rd_embed_texts_with_candidate(
    candidate: &RdEmbeddingApiKey,
    texts: &[String],
) -> anyhow::Result<RdEmbeddingBatchOutput> {
    rd_embed_texts_with_candidate_priority(candidate, texts, false).await
}

pub(in crate::routes::rd) async fn rd_embed_texts_with_candidate_background(
    candidate: &RdEmbeddingApiKey,
    texts: &[String],
) -> anyhow::Result<RdEmbeddingBatchOutput> {
    rd_embed_texts_with_candidate_priority(candidate, texts, true).await
}

async fn rd_embed_texts_with_candidate_priority(
    candidate: &RdEmbeddingApiKey,
    texts: &[String],
    background: bool,
) -> anyhow::Result<RdEmbeddingBatchOutput> {
    #[cfg(not(feature = "nl2sql"))]
    {
        let _ = (candidate, texts);
        anyhow::bail!("RD semantic embedding requires the nl2sql feature");
    }
    #[cfg(feature = "nl2sql")]
    {
        let model = crate::nl2sql::embedding::EmbeddingModel::new_with_dimensions(
            &candidate.model,
            candidate.base_url.clone(),
            Some(candidate.api_key.clone()),
            candidate.dimensions,
        );
        let (vectors, usage) = if background {
            model.embed_batch_with_usage_background(texts).await?
        } else {
            model.embed_batch_with_usage(texts).await?
        };
        if vectors.len() != texts.len() {
            anyhow::bail!(
                "embedding response length mismatch: got {}, expected {}",
                vectors.len(),
                texts.len()
            );
        }
        Ok(RdEmbeddingBatchOutput { vectors, usage })
    }
}

pub(in crate::routes::rd) async fn record_rd_embedding_usage(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    request_id: Option<String>,
    candidate: &RdEmbeddingApiKey,
    usage: Option<&api::Usage>,
    text_count: usize,
    input_chars: usize,
) {
    let Some(writer) = state.usage_writer.as_ref() else {
        return;
    };
    let estimated_input_tokens =
        u32::try_from((input_chars / 4).max(text_count)).unwrap_or(u32::MAX);
    let input_tokens = usage
        .map(|usage| usage.input_tokens)
        .filter(|tokens| *tokens > 0)
        .unwrap_or(estimated_input_tokens);
    let output_tokens = usage.map_or(0, |usage| usage.output_tokens);
    let cache_creation_tokens = usage.map_or(0, |usage| usage.cache_creation_input_tokens);
    let cache_read_tokens = usage.map_or(0, |usage| usage.cache_read_input_tokens);
    let total_tokens = input_tokens
        .saturating_add(output_tokens)
        .saturating_add(cache_creation_tokens)
        .saturating_add(cache_read_tokens);
    let record = crate::routes::chat::TokenUsageRecord {
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        request_id,
        model: candidate.model.clone(),
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        total_tokens,
        estimated_cost_usd: 0.0,
        api_key_id: candidate.id.clone(),
        provider: format!("rd_embedding:{}", candidate.provider),
        created_at: Utc::now(),
    };
    if let Err(error) = writer.write(&record).await {
        tracing::warn!(
            tenant_id = %tenant_id,
            model = %candidate.model,
            error = %error,
            "failed to record RD embedding token usage"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::fuse_rd_semantic_hits;
    use crate::routes::rd::embedding::{RdEmbeddingChunkType, RdEmbeddingSearchHit};

    fn hit(id: &str, score: f32) -> RdEmbeddingSearchHit {
        RdEmbeddingSearchHit {
            chunk_id: id.to_string(),
            chunk_type: RdEmbeddingChunkType::FileSummary,
            file_path: Some(format!("src/{id}.rs")),
            symbol_name: None,
            line_number: None,
            score,
            text: id.to_string(),
            metadata_json: serde_json::json!({}),
            task_id: None,
        }
    }

    #[test]
    fn local_and_remote_rankings_are_fused_without_duplicate_chunks() {
        let fused = fuse_rd_semantic_hits(
            vec![
                vec![hit("shared", 0.72), hit("local-only", 0.91)],
                vec![hit("shared", 0.88), hit("remote-only", 0.80)],
            ],
            3,
        );

        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].chunk_id, "shared");
        assert_eq!(fused[0].text, "shared");
        assert!(fused[0].score > 0.85);
    }
}
