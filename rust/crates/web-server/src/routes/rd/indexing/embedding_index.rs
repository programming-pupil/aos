//! Repository and task embedding indexing jobs for RD code search.

use super::*;

pub(in crate::routes::rd) fn schedule_rd_repository_embedding_index(
    state: AppState,
    tenant_id: String,
    user_id: String,
    repository_id: String,
    reason: &'static str,
) {
    if state.rd_embedding_store.is_none() || state.config_registry.is_none() {
        return;
    }
    let flight_key = format!("{tenant_id}:{repository_id}");
    if !mark_rd_embedding_index_scheduled(&flight_key) {
        tracing::debug!(
            tenant_id = %tenant_id,
            repository_id = %repository_id,
            reason = %reason,
            "RD repository embedding index already running; coalesced a pending rerun"
        );
        return;
    }
    tokio::spawn(async move {
        let mut run_reason = reason;
        loop {
            if let Err(error) = run_rd_repository_embedding_index(
                state.clone(),
                &tenant_id,
                &user_id,
                &repository_id,
                run_reason,
            )
            .await
            {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    repository_id = %repository_id,
                    reason = %run_reason,
                    error = %error,
                    "RD repository embedding index failed; Code task flow continues with lexical retrieval"
                );
            }
            if !finish_rd_embedding_index_run(&flight_key) {
                break;
            }
            run_reason = "coalesced_rerun";
        }
    });
}

fn mark_rd_embedding_index_scheduled(flight_key: &str) -> bool {
    let flights = RD_EMBEDDING_INDEX_IN_FLIGHT.get_or_init(|| ParkingMutex::new(HashMap::new()));
    let mut guard = flights.lock();
    if let Some(pending) = guard.get_mut(flight_key) {
        *pending = true;
        false
    } else {
        guard.insert(flight_key.to_string(), false);
        true
    }
}

fn finish_rd_embedding_index_run(flight_key: &str) -> bool {
    let flights = RD_EMBEDDING_INDEX_IN_FLIGHT.get_or_init(|| ParkingMutex::new(HashMap::new()));
    let mut guard = flights.lock();
    let Some(pending) = guard.get_mut(flight_key) else {
        return false;
    };
    if *pending {
        *pending = false;
        true
    } else {
        guard.remove(flight_key);
        false
    }
}

async fn run_rd_repository_embedding_index(
    state: AppState,
    tenant_id: &str,
    user_id: &str,
    repository_id: &str,
    reason: &'static str,
) -> Result<usize, AppError> {
    let Some(store) = state.rd_embedding_store.as_ref().cloned() else {
        return Ok(0);
    };
    let chunks =
        load_rd_repository_embedding_input_chunks(&state.db, tenant_id, repository_id).await?;
    if chunks.is_empty() {
        return Ok(0);
    }
    let candidates = resolve_rd_embedding_candidates(&state, tenant_id).await;
    if candidates.is_empty() {
        return Ok(0);
    }

    let mut last_error = None;
    for candidate in candidates {
        update_rd_repository_embedding_status(
            &state.db,
            tenant_id,
            repository_id,
            "indexing",
            Some(&candidate.model),
            None,
            None,
            None,
        )
        .await?;
        match index_rd_repository_with_embedding_candidate(
            &state,
            store.clone(),
            tenant_id,
            user_id,
            repository_id,
            reason,
            &candidate,
            &chunks,
        )
        .await
        {
            Ok(count) => {
                update_rd_repository_embedding_status(
                    &state.db,
                    tenant_id,
                    repository_id,
                    "ready",
                    Some(&candidate.model),
                    Some(count.total_chunks),
                    Some(&count),
                    None,
                )
                .await?;
                update_rd_file_summary_embedding_cache(
                    &state.db,
                    tenant_id,
                    repository_id,
                    &candidate.model,
                    &chunks,
                )
                .await?;
                return Ok(count.total_chunks);
            }
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    repository_id = %repository_id,
                    model = %candidate.model,
                    error = %error,
                    "RD embedding candidate failed during repository indexing; trying next candidate"
                );
                last_error = Some(error.to_string());
            }
        }
    }

    update_rd_repository_embedding_status(
        &state.db,
        tenant_id,
        repository_id,
        "failed",
        None,
        None,
        None,
        last_error.as_deref(),
    )
    .await?;
    Err(AppError::Internal(last_error.unwrap_or_else(|| {
        "RD repository embedding indexing failed".to_string()
    })))
}

async fn index_rd_repository_with_embedding_candidate(
    state: &AppState,
    store: std::sync::Arc<embedding::RdEmbeddingStore>,
    tenant_id: &str,
    user_id: &str,
    repository_id: &str,
    reason: &'static str,
    candidate: &RdEmbeddingApiKey,
    chunks: &[RdEmbeddingInputChunk],
) -> Result<RdEmbeddingIndexStats, AppError> {
    let hash_store = store.clone();
    let tenant = tenant_id.to_string();
    let repo = repository_id.to_string();
    let model = candidate.model.clone();
    let existing_hashes = tokio::task::spawn_blocking(move || {
        hash_store.repository_chunk_hashes(&tenant, &repo, &model)
    })
    .await
    .map_err(|error| {
        AppError::Internal(format!("RD embedding hash lookup worker failed: {error}"))
    })?
    .map_err(|error| AppError::Internal(format!("RD embedding hash lookup failed: {error}")))?;
    let mut reused_chunks = 0usize;
    let mut estimated_tokens_saved = 0u64;
    let chunks_to_embed = chunks
        .iter()
        .filter(|chunk| {
            let changed = existing_hashes
                .get(&chunk.chunk_id)
                .map_or(true, |hash| hash != &chunk.content_hash);
            if !changed {
                reused_chunks = reused_chunks.saturating_add(1);
                estimated_tokens_saved = estimated_tokens_saved.saturating_add(
                    u64::try_from((chunk.text.chars().count() / 4).max(1)).unwrap_or(0),
                );
            }
            changed
        })
        .cloned()
        .collect::<Vec<_>>();

    let keep_chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id.clone())
        .collect::<HashSet<_>>();

    let mut indexed_count = 0usize;
    for batch in chunks_to_embed.chunks(RD_EMBEDDING_BATCH_SIZE) {
        let texts = batch
            .iter()
            .map(|chunk| chunk.text.clone())
            .collect::<Vec<_>>();
        let input_chars = texts.iter().map(|text| text.chars().count()).sum::<usize>();
        let output = timeout(
            Duration::from_secs(RD_EMBEDDING_INDEX_BATCH_TIMEOUT_SECS),
            rd_embed_texts_with_candidate(candidate, &texts),
        )
        .await
        .map_err(|_| {
            AppError::Internal(format!(
                "RD embedding index batch timed out after {}s",
                RD_EMBEDDING_INDEX_BATCH_TIMEOUT_SECS
            ))
        })?
        .map_err(|error| AppError::Internal(format!("RD embedding index batch failed: {error}")))?;

        record_rd_embedding_usage(
            state,
            tenant_id,
            user_id,
            &format!("rd-embedding-index:{repository_id}"),
            Some(format!("{reason}:{repository_id}")),
            candidate,
            output.usage.as_ref(),
            texts.len(),
            input_chars,
        )
        .await;

        let upserts = batch
            .iter()
            .cloned()
            .zip(output.vectors.into_iter())
            .map(|(chunk, vector)| RdEmbeddingChunkUpsert {
                chunk_id: chunk.chunk_id,
                chunk_type: chunk.chunk_type,
                file_path: chunk.file_path,
                symbol_name: chunk.symbol_name,
                line_number: chunk.line_number,
                content_hash: chunk.content_hash,
                text: chunk.text,
                metadata_json: chunk.metadata_json,
                vector,
                task_id: chunk.task_id,
            })
            .collect::<Vec<_>>();
        let write_store = store.clone();
        let tenant = tenant_id.to_string();
        let repo = repository_id.to_string();
        let model = candidate.model.clone();
        let written = upserts.len();
        tokio::task::spawn_blocking(move || {
            write_store.upsert_chunks(&tenant, &repo, &model, &upserts)
        })
        .await
        .map_err(|error| AppError::Internal(format!("RD embedding write worker failed: {error}")))?
        .map_err(|error| AppError::Internal(format!("RD embedding write failed: {error}")))?;
        indexed_count = indexed_count.saturating_add(written);
    }

    let prune_store = store.clone();
    let tenant = tenant_id.to_string();
    let repo = repository_id.to_string();
    let model = candidate.model.clone();
    let pruned_chunks = tokio::task::spawn_blocking(move || {
        prune_store.prune_repository_index(&tenant, &repo, &model, &keep_chunk_ids)
    })
    .await
    .map_err(|error| AppError::Internal(format!("RD embedding prune worker failed: {error}")))?
    .map_err(|error| AppError::Internal(format!("RD embedding prune failed: {error}")))?;

    tracing::info!(
        tenant_id = %tenant_id,
        repository_id = %repository_id,
        model = %candidate.model,
        changed_chunks = indexed_count,
        reused_chunks = reused_chunks,
        pruned_chunks = pruned_chunks,
        total_chunks = chunks.len(),
        "RD repository embedding index completed"
    );
    Ok(RdEmbeddingIndexStats {
        total_chunks: chunks.len(),
        reused_chunks,
        regenerated_chunks: indexed_count,
        pruned_chunks,
        estimated_tokens_saved,
    })
}

async fn update_rd_repository_embedding_status(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    status: &str,
    model: Option<&str>,
    chunk_count: Option<usize>,
    stats: Option<&RdEmbeddingIndexStats>,
    error: Option<&str>,
) -> Result<(), AppError> {
    let count_value = chunk_count
        .and_then(|value| i64::try_from(value).ok())
        .unwrap_or(0);
    sqlx::query(
        "UPDATE rd_repository_indexes
         SET embedding_model = COALESCE(?, embedding_model),
             detail_json = JSON_SET(
                 COALESCE(detail_json, JSON_OBJECT()),
                 '$.embeddingStatus', ?,
                 '$.embeddingChunkCount', ?,
                 '$.embeddingReusedChunks', ?,
                 '$.embeddingRegeneratedChunks', ?,
                 '$.embeddingPrunedChunks', ?,
                 '$.embeddingEstimatedTokensSaved', ?,
                 '$.embeddingLastError', ?,
                 '$.embeddingUpdatedAt', strftime('%Y-%m-%dT%H:%M:%SZ', CURRENT_TIMESTAMP)
             )
         WHERE tenant_id = ? AND repository_id = ?",
    )
    .bind(model)
    .bind(status)
    .bind(count_value)
    .bind(stats.map(|stats| i64::try_from(stats.reused_chunks).unwrap_or(i64::MAX)))
    .bind(stats.map(|stats| i64::try_from(stats.regenerated_chunks).unwrap_or(i64::MAX)))
    .bind(stats.map(|stats| i64::try_from(stats.pruned_chunks).unwrap_or(i64::MAX)))
    .bind(stats.map(|stats| i64::try_from(stats.estimated_tokens_saved).unwrap_or(i64::MAX)))
    .bind(error)
    .bind(tenant_id)
    .bind(repository_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn update_rd_file_summary_embedding_cache(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    model: &str,
    chunks: &[RdEmbeddingInputChunk],
) -> Result<(), AppError> {
    for chunk in chunks
        .iter()
        .filter(|chunk| chunk.chunk_type == RdEmbeddingChunkType::FileSummary)
    {
        let Some(file_path) = chunk.file_path.as_deref() else {
            continue;
        };
        sqlx::query(
            "UPDATE rd_repository_file_summaries
             SET embedding_model = ?, embedding_content_hash = ?
             WHERE tenant_id = ? AND repository_id = ? AND file_path_hash = ?",
        )
        .bind(model)
        .bind(&chunk.content_hash)
        .bind(tenant_id)
        .bind(repository_id)
        .bind(stable_hash_hex(file_path))
        .execute(db)
        .await?;
    }
    Ok(())
}

async fn load_rd_repository_embedding_input_chunks(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
) -> Result<Vec<RdEmbeddingInputChunk>, AppError> {
    let mut chunks = Vec::new();

    let context_rows = sqlx::query(
        "SELECT scope_type, scope_key, source_hash,
                COALESCE(NULLIF(llm_summary_text, ''), summary_text) AS effective_summary_text,
                llm_model,
                CAST(detail_json AS TEXT) detail_json
         FROM rd_repository_context_summaries
         WHERE tenant_id = ? AND repository_id = ?
         ORDER BY
           CASE scope_type
             WHEN 'repository' THEN 0
             WHEN 'entrypoints' THEN 1
             WHEN 'directory' THEN 2
             ELSE 9
           END,
           updated_at DESC
         LIMIT 64",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    for row in context_rows {
        let scope_type: String = row.get("scope_type");
        let scope_key: String = row.get("scope_key");
        let source_hash: String = row.get("source_hash");
        let summary_text: String = row.get("effective_summary_text");
        let llm_model: Option<String> = row.get("llm_model");
        let effective_content_hash = rd_embedding_hash_text(&format!(
            "{}\n{}\n{}",
            source_hash,
            summary_text,
            llm_model.as_deref().unwrap_or("deterministic")
        ));
        let text = format!(
            "Repository context summary\nScope: {scope_type}/{scope_key}\nRefinedBy: {}\nSummary:\n{}",
            llm_model.as_deref().unwrap_or("deterministic"),
            truncate_text(&summary_text, 2_000)
        );
        chunks.push(RdEmbeddingInputChunk {
            chunk_id: repository_chunk_id(
                RdEmbeddingChunkType::ContextSummary,
                &scope_key,
                &source_hash,
            ),
            chunk_type: RdEmbeddingChunkType::ContextSummary,
            file_path: None,
            symbol_name: None,
            line_number: None,
            content_hash: effective_content_hash,
            text,
            metadata_json: json!({
                "scopeType": scope_type,
                "scopeKey": scope_key,
                "source": "rd_repository_context_summaries",
            }),
            task_id: None,
        });
    }

    let summary_rows = sqlx::query(
        "SELECT file_path, language, size_bytes, mtime_ms, content_hash, git_blob_sha, summary_text, summary_hash,
                CAST(symbols_json AS TEXT) AS symbols_json,
                CAST(imports_json AS TEXT) AS imports_json
         FROM rd_repository_file_summaries
         WHERE tenant_id = ? AND repository_id = ?
         ORDER BY updated_at DESC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .bind(i64::try_from(RD_FILE_SUMMARY_INDEX_LIMIT).unwrap_or(i64::MAX))
    .fetch_all(db)
    .await?;

    for row in summary_rows {
        let file_path: String = row.get("file_path");
        let language: Option<String> = row.get("language");
        let size_bytes: u64 = row.get("size_bytes");
        let mtime_ms: Option<u64> = row.get("mtime_ms");
        let content_hash: String = row.get("content_hash");
        let git_blob_sha: Option<String> = row.get("git_blob_sha");
        let summary_text: String = row.get("summary_text");
        let summary_hash = row
            .get::<Option<String>, _>("summary_hash")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| stable_hash_hex(&summary_text));
        let symbols = parse_json_string_array(row.get::<Option<String>, _>("symbols_json"));
        let imports = parse_json_string_array(row.get::<Option<String>, _>("imports_json"));
        let text = format!(
            "File: {file_path}\nLanguage: {}\nSizeBytes: {size_bytes}\nSummary:\n{}\nSymbols:\n{}\nImports:\n{}",
            language.as_deref().unwrap_or("text"),
            truncate_text(&summary_text, 1_200),
            symbols.iter().take(30).cloned().collect::<Vec<_>>().join(", "),
            imports.iter().take(30).cloned().collect::<Vec<_>>().join(", ")
        );
        let embedding_content_hash =
            rd_embedding_hash_text(&format!("{content_hash}\n{summary_hash}\n{text}"));
        chunks.push(RdEmbeddingInputChunk {
            chunk_id: repository_chunk_id(
                RdEmbeddingChunkType::FileSummary,
                &file_path,
                &summary_hash,
            ),
            chunk_type: RdEmbeddingChunkType::FileSummary,
            file_path: Some(file_path.clone()),
            symbol_name: None,
            line_number: None,
            content_hash: embedding_content_hash,
            text,
            metadata_json: json!({
                "language": language,
                "sizeBytes": size_bytes,
                "mtimeMs": mtime_ms,
                "gitBlobSha": git_blob_sha,
                "fileContentHash": content_hash,
                "summaryHash": summary_hash,
                "source": "rd_repository_file_summaries",
            }),
            task_id: None,
        });
    }

    let symbol_rows = sqlx::query(
        "SELECT file_path, language, symbol_name, symbol_kind, signature, line_number
         FROM rd_repository_symbols
         WHERE tenant_id = ? AND repository_id = ?
         ORDER BY symbol_kind ASC, symbol_name ASC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .bind(i64::try_from(RD_EMBEDDING_SYMBOL_INDEX_LIMIT).unwrap_or(i64::MAX))
    .fetch_all(db)
    .await?;
    for row in symbol_rows {
        let file_path: String = row.get("file_path");
        let language: Option<String> = row.get("language");
        let symbol_name: String = row.get("symbol_name");
        let symbol_kind: String = row.get("symbol_kind");
        let signature: Option<String> = row.get("signature");
        let line_number: u64 = row.get("line_number");
        let discriminator = format!(
            "{}:{}:{}:{}",
            symbol_kind,
            symbol_name,
            signature.as_deref().unwrap_or_default(),
            line_number
        );
        let text = format!(
            "Symbol: {symbol_kind} {symbol_name}\nFile: {file_path}\nLanguage: {}\nLine: {line_number}\nSignature: {}",
            language.as_deref().unwrap_or("text"),
            signature.as_deref().unwrap_or("")
        );
        chunks.push(RdEmbeddingInputChunk {
            chunk_id: repository_chunk_id(RdEmbeddingChunkType::Symbol, &file_path, &discriminator),
            chunk_type: RdEmbeddingChunkType::Symbol,
            file_path: Some(file_path),
            symbol_name: Some(symbol_name),
            line_number: Some(line_number),
            content_hash: rd_embedding_hash_text(&discriminator),
            text,
            metadata_json: json!({
                "language": language,
                "symbolKind": symbol_kind,
                "source": "rd_repository_symbols",
            }),
            task_id: None,
        });
    }

    let import_rows = sqlx::query(
        "SELECT file_path, language, import_path, import_kind, line_number
         FROM rd_repository_imports
         WHERE tenant_id = ? AND repository_id = ?
         ORDER BY import_path ASC, file_path ASC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .bind(i64::try_from(RD_EMBEDDING_IMPORT_INDEX_LIMIT).unwrap_or(i64::MAX))
    .fetch_all(db)
    .await?;
    for row in import_rows {
        let file_path: String = row.get("file_path");
        let language: Option<String> = row.get("language");
        let import_path: String = row.get("import_path");
        if !is_plausible_import_path(language.as_deref().unwrap_or("unknown"), &import_path) {
            continue;
        }
        let import_kind: String = row.get("import_kind");
        let line_number: u64 = row.get("line_number");
        let discriminator = format!("{import_kind}:{import_path}:{line_number}");
        let text = format!(
            "Import: {import_kind} {import_path}\nFile: {file_path}\nLanguage: {}\nLine: {line_number}",
            language.as_deref().unwrap_or("text")
        );
        chunks.push(RdEmbeddingInputChunk {
            chunk_id: repository_chunk_id(RdEmbeddingChunkType::Import, &file_path, &discriminator),
            chunk_type: RdEmbeddingChunkType::Import,
            file_path: Some(file_path),
            symbol_name: None,
            line_number: Some(line_number),
            content_hash: rd_embedding_hash_text(&discriminator),
            text,
            metadata_json: json!({
                "language": language,
                "importKind": import_kind,
                "source": "rd_repository_imports",
            }),
            task_id: None,
        });
    }

    Ok(chunks)
}

pub(in crate::routes::rd) fn schedule_rd_task_embedding_index(
    state: AppState,
    tenant_id: String,
    user_id: String,
    task_id: String,
) {
    if state.rd_embedding_store.is_none() || state.config_registry.is_none() {
        return;
    }
    tokio::spawn(async move {
        if let Err(error) = run_rd_task_embedding_index(state, &tenant_id, &user_id, &task_id).await
        {
            tracing::warn!(
                tenant_id = %tenant_id,
                task_id = %task_id,
                error = %error,
                "RD task embedding index failed; completed task result is unaffected"
            );
        }
    });
}

async fn run_rd_task_embedding_index(
    state: AppState,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
) -> Result<(), AppError> {
    let Some(store) = state.rd_embedding_store.as_ref().cloned() else {
        return Ok(());
    };
    let task = get_task_row(&state.db, tenant_id, user_id, task_id).await?;
    let Some(repository_id) = task.repository_id.as_deref() else {
        return Ok(());
    };
    let candidates = resolve_rd_embedding_candidates(&state, tenant_id).await;
    if candidates.is_empty() {
        return Ok(());
    }
    let touched_files = load_rd_task_touched_files(&state.db, tenant_id, task_id).await?;
    let chunk = build_rd_task_embedding_chunk(&task, &touched_files);
    let texts = vec![chunk.text.clone()];
    let input_chars = chunk.text.chars().count();

    let mut last_error = None;
    for candidate in candidates {
        let output = match timeout(
            Duration::from_secs(RD_EMBEDDING_INDEX_BATCH_TIMEOUT_SECS),
            rd_embed_texts_with_candidate(&candidate, &texts),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                last_error = Some(error.to_string());
                continue;
            }
            Err(_) => {
                last_error = Some(format!(
                    "RD task embedding timed out after {}s",
                    RD_EMBEDDING_INDEX_BATCH_TIMEOUT_SECS
                ));
                continue;
            }
        };
        record_rd_embedding_usage(
            &state,
            tenant_id,
            user_id,
            &format!("rd-embedding-task:{repository_id}"),
            Some(task_id.to_string()),
            &candidate,
            output.usage.as_ref(),
            1,
            input_chars,
        )
        .await;
        let Some(vector) = output.vectors.into_iter().next() else {
            continue;
        };
        let upsert = RdEmbeddingChunkUpsert {
            chunk_id: chunk.chunk_id.clone(),
            chunk_type: chunk.chunk_type,
            file_path: chunk.file_path.clone(),
            symbol_name: chunk.symbol_name.clone(),
            line_number: chunk.line_number,
            content_hash: chunk.content_hash.clone(),
            text: chunk.text.clone(),
            metadata_json: chunk.metadata_json.clone(),
            vector,
            task_id: chunk.task_id.clone(),
        };
        let write_store = store.clone();
        let tenant = tenant_id.to_string();
        let repo = repository_id.to_string();
        let model = candidate.model.clone();
        tokio::task::spawn_blocking(move || {
            write_store.upsert_chunks(&tenant, &repo, &model, &[upsert])
        })
        .await
        .map_err(|error| {
            AppError::Internal(format!("RD task embedding write worker failed: {error}"))
        })?
        .map_err(|error| AppError::Internal(format!("RD task embedding write failed: {error}")))?;
        return Ok(());
    }

    Err(AppError::Internal(
        last_error.unwrap_or_else(|| "RD task embedding failed".to_string()),
    ))
}

async fn load_rd_task_touched_files(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query(
        "SELECT DISTINCT file_path FROM rd_file_changes
         WHERE tenant_id = ? AND task_id = ?
         ORDER BY file_path ASC
         LIMIT 30",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|row| row.get("file_path")).collect())
}

fn build_rd_task_embedding_chunk(
    task: &RdTaskDto,
    touched_files: &[String],
) -> RdEmbeddingInputChunk {
    let review = task.review_md.as_deref().unwrap_or_default();
    let answer = task.answer_md.as_deref().unwrap_or_default();
    let plan = task.plan_md.as_deref().unwrap_or_default();
    let pr_title = task.pr_title.as_deref().unwrap_or_default();
    let pr_description = task.pr_description.as_deref().unwrap_or_default();
    let files = touched_files.join(", ");
    let text = format!(
        "RD Task: {}\nMode: {}\nStatus: {}\nPrompt:\n{}\nPlan:\n{}\nAnswer:\n{}\nReview:\n{}\nTouchedFiles: {}\nPR Title: {}\nPR Description:\n{}",
        task.title,
        task.mode,
        task.status,
        truncate_text(&task.prompt, 2_000),
        truncate_text(plan, 2_000),
        truncate_text(answer, 3_000),
        truncate_text(review, 3_000),
        files,
        pr_title,
        truncate_text(pr_description, 1_500)
    );
    let content_hash = rd_embedding_hash_text(&format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        task.prompt, plan, answer, review, pr_title, pr_description
    ));
    RdEmbeddingInputChunk {
        chunk_id: task_chunk_id(&task.id),
        chunk_type: RdEmbeddingChunkType::Task,
        file_path: None,
        symbol_name: None,
        line_number: None,
        content_hash,
        text,
        metadata_json: json!({
            "source": "rd_tasks",
            "taskId": task.id,
            "mode": task.mode,
            "status": task.status,
            "title": task.title,
            "touchedFiles": touched_files,
        }),
        task_id: Some(task.id.clone()),
    }
}
