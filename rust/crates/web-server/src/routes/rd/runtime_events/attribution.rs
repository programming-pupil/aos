//! Runtime retrieval attribution context loaded from RD task events.

use super::*;

#[derive(Debug, Clone, Default)]
pub(super) struct RdRuntimeAttributionContext {
    pub(super) files: HashMap<String, RdRuntimeRetrievalEvidence>,
    pub(super) explicit_files: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RdRuntimeRetrievalEvidence {
    pub(super) score: f64,
    pub(super) rank: Option<u64>,
    pub(super) sources: BTreeSet<String>,
    pub(super) notes: Vec<String>,
    pub(super) reason: Option<String>,
}
pub(super) async fn load_rd_runtime_attribution_context(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> RdRuntimeAttributionContext {
    let rows = match sqlx::query(
        "SELECT stage, message, detail_json \
         FROM rd_task_events \
         WHERE task_id = ? AND tenant_id = ? \
           AND stage IN ('context_retrieval_evidence', 'context') \
         ORDER BY id ASC LIMIT 64",
    )
    .bind(task_id)
    .bind(tenant_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                task_id = %task_id,
                "failed to load RD runtime attribution context: {}",
                error
            );
            return RdRuntimeAttributionContext::default();
        }
    };

    let mut context = RdRuntimeAttributionContext::default();
    for row in rows {
        let stage: String = row.get("stage");
        let message: String = row.get("message");
        let detail: Option<Value> = row.get("detail_json");
        let Some(detail) = detail else {
            continue;
        };
        if stage == "context" && message.contains('@') {
            if let Some(files) = detail.get("files").and_then(Value::as_array) {
                for file in files.iter().filter_map(Value::as_str) {
                    let normalized = normalize_rd_runtime_attribution_path(file);
                    if !normalized.is_empty() {
                        context.explicit_files.insert(normalized);
                    }
                }
            }
            continue;
        }
        if stage != "context_retrieval_evidence" {
            continue;
        }
        let Some(files) = detail.get("files").and_then(Value::as_array) else {
            continue;
        };
        for file in files {
            let Some(path) = file.get("path").and_then(Value::as_str) else {
                continue;
            };
            let normalized = normalize_rd_runtime_attribution_path(path);
            if normalized.is_empty() {
                continue;
            }
            let entry = context.files.entry(normalized).or_default();
            entry.score = entry.score.max(
                file.get("score")
                    .and_then(Value::as_f64)
                    .unwrap_or_default(),
            );
            entry.rank = match (
                entry.rank,
                file.get("rank")
                    .and_then(Value::as_u64)
                    .filter(|rank| *rank > 0),
            ) {
                (Some(existing), Some(next)) => Some(existing.min(next)),
                (None, Some(next)) => Some(next),
                (existing, None) => existing,
            };
            if let Some(reason) = file
                .get("reason")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                entry.reason.get_or_insert_with(|| reason.to_string());
            }
            if let Some(sources) = file.get("sources").and_then(Value::as_array) {
                for source in sources.iter().filter_map(Value::as_str) {
                    let source = source.trim();
                    if !source.is_empty() {
                        entry.sources.insert(source.to_string());
                    }
                }
            }
            if let Some(notes) = file.get("notes").and_then(Value::as_array) {
                for note in notes.iter().filter_map(Value::as_str) {
                    let note = truncate_text(note.trim(), 300);
                    if !note.is_empty() && !entry.notes.contains(&note) && entry.notes.len() < 8 {
                        entry.notes.push(note);
                    }
                }
            }
        }
    }
    context
}

pub(super) fn normalize_rd_runtime_attribution_path(path: &str) -> String {
    path.trim()
        .trim_matches('"')
        .trim_matches('`')
        .trim_start_matches("./")
        .replace('\\', "/")
}

pub(super) fn rd_runtime_retrieval_reason_from_sources(sources: &BTreeSet<String>) -> String {
    if sources.iter().any(|source| source.starts_with("embedding")) {
        "该文件由 embedding 语义召回命中，适合优先读取真实内容核对。".to_string()
    } else if sources.contains("dependency_graph") {
        "该文件由本地 import/dependency 图命中，适合读取确认调用链或模块依赖关系。".to_string()
    } else if sources.contains("symbol_index") {
        "该文件的符号索引命中用户问题关键词，适合读取确认接口/函数实现。".to_string()
    } else if sources.contains("import_index") {
        "该文件的 import/dependency 索引命中相关依赖，适合读取确认调用链。".to_string()
    } else if sources.contains("file_summary") {
        "该文件摘要命中用户问题关键词，适合读取真实文件核对摘要。".to_string()
    } else {
        "该文件来自当前任务的结构化召回证据，读取用于核对真实代码。".to_string()
    }
}
