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
    for candidate in candidates {
        let model = candidate.model.clone();
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
                continue;
            }
        };
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
        let model = candidate.model.clone();
        let hits = tokio::task::spawn_blocking(move || {
            search_store.search_repository(&tenant_id, &repo_id, &model, &query_vector, top_k)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_else(Vec::new);
        if !hits.is_empty() {
            return hits;
        }
    }

    Vec::new()
}

pub(in crate::routes::rd) async fn resolve_rd_embedding_candidates(
    state: &AppState,
    tenant_id: &str,
) -> Vec<RdEmbeddingApiKey> {
    let Some(registry) = state.config_registry.as_ref() else {
        return Vec::new();
    };
    let entries = match registry
        .resolve_api_keys_by_model_type(tenant_id, Some(RD_SCENARIO), "embedding")
        .await
    {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                error = %error,
                "failed to resolve RD embedding api_keys"
            );
            return Vec::new();
        }
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let model = entry
                .model
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "text-embedding-3-small".to_string());
            (!entry.key.trim().is_empty()).then_some(RdEmbeddingApiKey {
                id: entry.id,
                provider: entry.provider,
                #[cfg(feature = "nl2sql")]
                base_url: entry.base_url,
                model,
                #[cfg(feature = "nl2sql")]
                api_key: entry.key,
            })
        })
        .collect()
}

pub(in crate::routes::rd) async fn rd_embed_texts_with_candidate(
    candidate: &RdEmbeddingApiKey,
    texts: &[String],
) -> anyhow::Result<RdEmbeddingBatchOutput> {
    #[cfg(not(feature = "nl2sql"))]
    {
        let _ = (candidate, texts);
        anyhow::bail!("RD semantic embedding requires the nl2sql feature");
    }
    #[cfg(feature = "nl2sql")]
    {
        let model = crate::nl2sql::embedding::EmbeddingModel::new(
            &candidate.model,
            candidate.base_url.clone(),
            Some(candidate.api_key.clone()),
        );
        let (vectors, usage) = model.embed_batch_with_usage(texts).await?;
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
        api_key_id: Some(candidate.id.clone()),
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
