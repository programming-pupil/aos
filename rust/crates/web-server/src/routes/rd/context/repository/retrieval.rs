//! Repository retrieval merging, dependency expansion, and task-memory hints.

use super::dependency_graph::augment_repository_retrieval_with_dependency_graph;
use super::evidence::{rd_retrieval_evidence_reason, rd_retrieval_sources_from_notes};
use super::task_memory::augment_repository_retrieval_with_task_memory;
use super::*;

#[derive(Debug, Clone, Default)]
pub(super) struct RdRepositoryRetrievalContext {
    pub(super) text: String,
    pub(super) strategy: String,
    pub(super) terms: Vec<String>,
    pub(super) evidence: Vec<RdRepositoryRetrievalEvidenceFile>,
    pub(super) observability_sources: BTreeSet<String>,
    pub(super) task_memory_hit_count: u64,
    pub(super) context_summary_hit_count: u64,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RdRepositoryRetrievalEvidenceFile {
    pub(super) path: String,
    pub(super) rank: usize,
    pub(super) score: f32,
    pub(super) sources: BTreeSet<String>,
    pub(super) notes: Vec<String>,
    pub(super) reason: String,
}

impl RdRepositoryRetrievalEvidenceFile {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "path": self.path.clone(),
            "rank": self.rank,
            "score": self.score,
            "sources": self.sources.iter().cloned().collect::<Vec<_>>(),
            "notes": self.notes.clone(),
            "reason": self.reason.clone(),
        })
    }
}

pub(super) async fn build_repository_retrieval_context(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    prompt: &str,
    context_profile: RdContextProfile,
) -> Result<RdRepositoryRetrievalContext, AppError> {
    let context_budget = context_profile.budget();
    let semantic_hits = rd_semantic_repository_search(
        state,
        claims,
        repository_id,
        prompt,
        context_budget.semantic_top_k,
    )
    .await;
    let mut terms = extract_repository_retrieval_terms(prompt, context_budget.retrieval_term_limit);
    if context_profile == RdContextProfile::Overview {
        let mut merged = terms.into_iter().collect::<BTreeSet<_>>();
        merged.extend(default_repository_retrieval_terms(context_profile));
        terms = merged
            .into_iter()
            .take(context_budget.retrieval_term_limit)
            .collect();
    } else if terms.is_empty() {
        terms = default_repository_retrieval_terms(context_profile);
    }

    let mut file_notes: HashMap<String, Vec<String>> = HashMap::new();
    let mut file_scores: HashMap<String, f32> = HashMap::new();
    let mut context_notes = Vec::new();
    let mut task_notes = Vec::new();
    let mut observability_sources = BTreeSet::new();
    let mut task_memory_hit_count = 0_u64;
    let mut context_summary_hit_count = 0_u64;

    for hit in semantic_hits {
        let score = hit.score.clamp(0.0, 1.0);
        if let Some(file_path) = hit.file_path.as_deref().filter(|value| !value.is_empty()) {
            let label = match hit.chunk_type {
                RdEmbeddingChunkType::ContextSummary => "semantic-context",
                RdEmbeddingChunkType::FileSummary => "semantic-summary",
                RdEmbeddingChunkType::Symbol => "semantic-symbol",
                RdEmbeddingChunkType::Import => "semantic-import",
                RdEmbeddingChunkType::Task => "semantic-task",
            };
            let location = hit
                .line_number
                .map(|line| format!(" line {line}"))
                .unwrap_or_default();
            let symbol = hit
                .symbol_name
                .as_deref()
                .map(|name| format!(" `{name}`"))
                .unwrap_or_default();
            let metadata_hint = rd_semantic_hit_metadata_hint(&hit.metadata_json);
            file_notes
                .entry(file_path.to_string())
                .or_default()
                .push(format!(
                    "{label}(score={score:.2}){symbol}{location}: {}{}",
                    truncate_text(&hit.text, 520),
                    metadata_hint
                ));
            let type_boost = match hit.chunk_type {
                RdEmbeddingChunkType::ContextSummary => 1.8,
                RdEmbeddingChunkType::FileSummary => 1.4,
                RdEmbeddingChunkType::Symbol => 1.2,
                RdEmbeddingChunkType::Task => 0.7,
                RdEmbeddingChunkType::Import => 0.6,
            };
            *file_scores.entry(file_path.to_string()).or_default() += score * 8.0 * type_boost;
        } else if hit.chunk_type == RdEmbeddingChunkType::ContextSummary {
            observability_sources.insert("embedding_context".to_string());
            context_summary_hit_count = context_summary_hit_count.saturating_add(1);
            context_notes.push(format!(
                "- 仓库级语义摘要 score={score:.2}: {}{}",
                truncate_text(&hit.text, 900),
                rd_semantic_hit_metadata_hint(&hit.metadata_json)
            ));
        } else if hit.chunk_type == RdEmbeddingChunkType::Task {
            observability_sources.insert("embedding_task".to_string());
            task_memory_hit_count = task_memory_hit_count.saturating_add(1);
            let task_id = hit.task_id.as_deref().unwrap_or("unknown");
            task_notes.push(format!(
                "- 相似历史任务 taskId={task_id} score={score:.2}: {}",
                truncate_text(&hit.text, 650)
            ));
        }
    }

    for term in terms
        .iter()
        .take(context_budget.retrieval_term_limit.min(8))
    {
        let like = format!("%{term}%");
        let summary_rows = sqlx::query(
            "SELECT file_path, language, size_bytes, summary_text \
             FROM rd_repository_file_summaries \
             WHERE tenant_id = ? AND repository_id = ? \
               AND (file_path LIKE ? OR summary_text LIKE ?) \
             ORDER BY updated_at DESC LIMIT 8",
        )
        .bind(&claims.tenant_id)
        .bind(repository_id)
        .bind(&like)
        .bind(&like)
        .fetch_all(&state.db)
        .await?;
        for row in summary_rows {
            let file_path: String = row.get("file_path");
            let language: Option<String> = row.get("language");
            let size_bytes: u64 = row.get("size_bytes");
            let summary_text: String = row.get("summary_text");
            file_notes
                .entry(file_path.clone())
                .or_default()
                .push(format!(
                    "summary(term={term}, lang={}, bytes={}): {}",
                    language.unwrap_or_else(|| "text".to_string()),
                    size_bytes,
                    truncate_text(&summary_text, 500)
                ));
            *file_scores.entry(file_path).or_default() += 2.0;
        }

        let symbol_rows = sqlx::query(
            "SELECT file_path, symbol_name, symbol_kind, line_number \
             FROM rd_repository_symbols \
             WHERE tenant_id = ? AND repository_id = ? \
               AND (symbol_name LIKE ? OR file_path LIKE ? OR signature LIKE ?) \
             ORDER BY symbol_name ASC, file_path ASC LIMIT 8",
        )
        .bind(&claims.tenant_id)
        .bind(repository_id)
        .bind(&like)
        .bind(&like)
        .bind(&like)
        .fetch_all(&state.db)
        .await?;
        for row in symbol_rows {
            let file_path: String = row.get("file_path");
            let symbol_name: String = row.get("symbol_name");
            let symbol_kind: String = row.get("symbol_kind");
            let line_number: u64 = row.get("line_number");
            file_notes
                .entry(file_path.clone())
                .or_default()
                .push(format!(
                    "symbol(term={term}): {symbol_kind} `{symbol_name}` at line {line_number}"
                ));
            *file_scores.entry(file_path).or_default() += 1.6;
        }

        let import_rows = sqlx::query(
            "SELECT file_path, language, import_path, import_kind, line_number \
             FROM rd_repository_imports \
             WHERE tenant_id = ? AND repository_id = ? \
               AND (import_path LIKE ? OR file_path LIKE ?) \
             ORDER BY import_path ASC, file_path ASC LIMIT 6",
        )
        .bind(&claims.tenant_id)
        .bind(repository_id)
        .bind(&like)
        .bind(&like)
        .fetch_all(&state.db)
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
            file_notes
                .entry(file_path.clone())
                .or_default()
                .push(format!(
                    "import(term={term}): {import_kind} `{}` at line {line_number}",
                    truncate_text(&import_path, 180)
                ));
            *file_scores.entry(file_path).or_default() += 0.8;
        }
    }

    augment_repository_retrieval_with_dependency_graph(
        state,
        claims,
        repository_id,
        context_budget,
        &mut file_notes,
        &mut file_scores,
    )
    .await?;

    task_memory_hit_count = task_memory_hit_count.saturating_add(
        augment_repository_retrieval_with_task_memory(
            state,
            claims,
            repository_id,
            prompt,
            &terms,
            context_budget,
            &mut task_notes,
            &mut file_notes,
            &mut file_scores,
            &mut observability_sources,
        )
        .await?,
    );

    if file_notes.is_empty() && task_notes.is_empty() && context_notes.is_empty() {
        return Ok(RdRepositoryRetrievalContext {
            terms,
            observability_sources,
            task_memory_hit_count,
            context_summary_hit_count,
            ..Default::default()
        });
    }

    let mut ranked = file_notes.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(path_a, notes_a), (path_b, notes_b)| {
        let score_a = file_scores.get(path_a).copied().unwrap_or_default();
        let score_b = file_scores.get(path_b).copied().unwrap_or_default();
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| notes_b.len().cmp(&notes_a.len()))
            .then_with(|| path_a.cmp(path_b))
    });

    let mut lines = Vec::new();
    let semantic_enabled = ranked.iter().any(|(_, notes)| {
        notes
            .iter()
            .any(|note| note.starts_with("semantic-") || note.contains("semantic-"))
    }) || !task_notes.is_empty()
        || !context_notes.is_empty();
    let strategy = if semantic_enabled {
        "embedding 语义召回 + 词法索引混合重排"
    } else {
        "词法索引召回"
    };
    lines.push(format!(
        "召回策略：{}。召回词：{}。以下仅是候选入口，关键判断必须继续读取真实文件。",
        strategy,
        terms.join(", ")
    ));
    if !context_notes.is_empty() {
        lines.push("仓库级语义命中（适合概览/架构问题，仍需关键文件核对）：".to_string());
        lines.extend(context_notes.into_iter().take(3));
    }
    let selected_files = ranked
        .into_iter()
        .take(context_budget.retrieval_file_limit)
        .enumerate()
        .collect::<Vec<_>>();
    let mut evidence = Vec::with_capacity(selected_files.len());
    for (rank_index, (file_path, notes)) in selected_files {
        let deduped = notes
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(context_budget.retrieval_notes_per_file)
            .collect::<Vec<_>>();
        let sources = rd_retrieval_sources_from_notes(&deduped);
        let score = file_scores.get(&file_path).copied().unwrap_or_default();
        let reason = rd_retrieval_evidence_reason(&sources);
        evidence.push(RdRepositoryRetrievalEvidenceFile {
            path: file_path.clone(),
            rank: rank_index + 1,
            score,
            sources,
            notes: deduped.iter().take(6).cloned().collect(),
            reason,
        });
        lines.push(format!("- `{file_path}`\n  - {}", deduped.join("\n  - ")));
    }
    if !task_notes.is_empty() {
        lines.push("相似历史研发任务（仅作背景提示，不可替代真实文件核对）：".to_string());
        lines.extend(task_notes.into_iter().take(5));
    }
    Ok(RdRepositoryRetrievalContext {
        text: truncate_text(&lines.join("\n"), context_budget.retrieval_bytes),
        strategy: strategy.to_string(),
        terms,
        evidence,
        observability_sources,
        task_memory_hit_count,
        context_summary_hit_count,
    })
}

fn default_repository_retrieval_terms(context_profile: RdContextProfile) -> Vec<String> {
    let terms: &[&str] = match context_profile {
        RdContextProfile::Overview => &[
            "README",
            "package",
            "Cargo",
            "main",
            "app",
            "server",
            "route",
            "router",
            "api",
            "config",
            "docker",
            "workspace",
        ],
        RdContextProfile::Review | RdContextProfile::DeepReview => &[
            "auth",
            "security",
            "permission",
            "config",
            "route",
            "controller",
            "service",
            "sql",
            "migration",
            "test",
            "error",
            "unsafe",
        ],
        RdContextProfile::Modify | RdContextProfile::Explain | RdContextProfile::FocusedAsk => &[
            "main",
            "app",
            "route",
            "server",
            "api",
            "controller",
            "service",
            "config",
            "test",
        ],
    };
    terms.iter().map(|term| (*term).to_string()).collect()
}
