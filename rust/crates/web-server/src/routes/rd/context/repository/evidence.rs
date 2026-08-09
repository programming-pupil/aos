//! Retrieval evidence source classification and human-readable reasons.

use super::*;

pub(super) fn rd_retrieval_sources_from_notes(notes: &[String]) -> BTreeSet<String> {
    let mut sources = BTreeSet::new();
    for note in notes {
        let lower = note.to_ascii_lowercase();
        if lower.starts_with("semantic-context") {
            sources.insert("embedding_context".to_string());
        }
        if lower.starts_with("semantic-summary") {
            sources.insert("embedding_summary".to_string());
        }
        if lower.starts_with("semantic-symbol") {
            sources.insert("embedding_symbol".to_string());
        }
        if lower.starts_with("semantic-import") {
            sources.insert("embedding_import".to_string());
        }
        if lower.starts_with("semantic-task") {
            sources.insert("embedding_task".to_string());
        }
        if lower.starts_with("history-task") {
            sources.insert("task_memory".to_string());
        }
        if lower.starts_with("summary(") {
            sources.insert("file_summary".to_string());
        }
        if lower.starts_with("symbol(") {
            sources.insert("symbol_index".to_string());
        }
        if lower.starts_with("import(") {
            sources.insert("import_index".to_string());
        }
        if lower.starts_with("dependency_graph(") {
            sources.insert("dependency_graph".to_string());
        }
    }
    if sources.is_empty() {
        sources.insert("retrieval_context".to_string());
    }
    sources
}

pub(super) fn rd_retrieval_evidence_reason(sources: &BTreeSet<String>) -> String {
    let has_embedding = sources.iter().any(|source| source.starts_with("embedding"));
    if has_embedding && sources.contains("symbol_index") {
        "该文件同时被 embedding 语义召回和 symbol 索引命中，优先读取可提升代码事实核对质量。"
            .to_string()
    } else if has_embedding && sources.contains("file_summary") {
        "该文件同时被 embedding 语义召回和文件摘要命中，适合作为当前问题的优先核对入口。"
            .to_string()
    } else if has_embedding {
        "该文件由 embedding 语义召回命中，说明其摘要/符号/历史任务与当前问题语义相关。".to_string()
    } else if sources.contains("dependency_graph") {
        "该文件由本地 import/dependency 图命中，可帮助确认调用链、模块边界和真实依赖方向。"
            .to_string()
    } else if sources.contains("task_memory") {
        "该文件由相似历史研发任务命中，适合优先读取真实文件确认当前代码是否已具备相关实现。"
            .to_string()
    } else if sources.contains("symbol_index") {
        "该文件的符号索引命中用户问题，优先读取可确认接口、函数或类型实现。".to_string()
    } else if sources.contains("import_index") {
        "该文件的依赖/import 索引命中相关模块，适合读取确认调用链和依赖关系。".to_string()
    } else if sources.contains("file_summary") {
        "该文件摘要命中用户问题关键词，读取真实文件可核对摘要是否足够准确。".to_string()
    } else {
        "该文件来自当前任务的索引召回候选，读取用于补齐真实代码证据。".to_string()
    }
}
