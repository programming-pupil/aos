//! Repository indexing, summaries, embeddings, and path helpers for RD.

use super::*;

mod context_summary;
mod embedding_index;
mod file_summary;
mod llm_summary;
pub(super) use context_summary::rebuild_repository_context_summary_index;
pub(super) use embedding_index::{
    schedule_rd_repository_embedding_index, schedule_rd_task_embedding_index,
};
pub(super) use file_summary::rebuild_repository_file_summary_index;
pub(super) use llm_summary::schedule_rd_repository_llm_summary_refinement;

fn parse_json_string_array(raw: Option<String>) -> Vec<String> {
    raw.as_deref()
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default()
}

pub(super) async fn rebuild_repository_symbol_index(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    root: &Path,
) -> Result<usize, AppError> {
    let root = root.to_path_buf();
    let symbols = tokio::task::spawn_blocking(move || collect_repository_symbols(&root, 8_000))
        .await
        .map_err(|error| AppError::Internal(format!("symbol indexing task failed: {error}")))?;

    sqlx::query("DELETE FROM rd_repository_symbols WHERE tenant_id = ? AND repository_id = ?")
        .bind(tenant_id)
        .bind(repository_id)
        .execute(db)
        .await?;

    for symbol in &symbols {
        sqlx::query("INSERT INTO rd_repository_symbols (tenant_id, repository_id, file_path, language, symbol_name, symbol_kind, signature, line_number) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(tenant_id)
            .bind(repository_id)
            .bind(&symbol.file_path)
            .bind(&symbol.language)
            .bind(&symbol.symbol_name)
            .bind(&symbol.symbol_kind)
            .bind(&symbol.signature)
            .bind(i64::try_from(symbol.line_number).unwrap_or(i64::MAX))
            .execute(db)
            .await?;
    }

    Ok(symbols.len())
}

pub(super) async fn rebuild_repository_import_index(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    root: &Path,
) -> Result<usize, AppError> {
    let root = root.to_path_buf();
    let imports = tokio::task::spawn_blocking(move || collect_repository_imports(&root, 10_000))
        .await
        .map_err(|error| AppError::Internal(format!("import indexing task failed: {error}")))?;

    sqlx::query("DELETE FROM rd_repository_imports WHERE tenant_id = ? AND repository_id = ?")
        .bind(tenant_id)
        .bind(repository_id)
        .execute(db)
        .await?;

    for item in &imports {
        sqlx::query("INSERT INTO rd_repository_imports (tenant_id, repository_id, file_path, language, import_path, import_kind, line_number) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(tenant_id)
            .bind(repository_id)
            .bind(&item.file_path)
            .bind(&item.language)
            .bind(&item.import_path)
            .bind(&item.import_kind)
            .bind(i64::try_from(item.line_number).unwrap_or(i64::MAX))
            .execute(db)
            .await?;
    }

    Ok(imports.len())
}

pub(super) fn stable_hash_hex<T: Hash>(value: T) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(super) fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, AppError> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(AppError::ValidationError(
            "absolute paths are not allowed".to_string(),
        ));
    }
    for component in rel_path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err(AppError::ValidationError(
                "path traversal is not allowed".to_string(),
            ));
        }
    }
    let path = root.join(rel_path);
    let canonical_root = root.canonicalize()?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&canonical_root) {
        return Err(AppError::ValidationError(
            "path escapes repository root".to_string(),
        ));
    }
    Ok(canonical)
}

pub(super) fn safe_join_allow_missing(root: &Path, rel: &str) -> Result<PathBuf, AppError> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(AppError::ValidationError(
            "absolute paths are not allowed".to_string(),
        ));
    }
    for component in rel_path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err(AppError::ValidationError(
                "path traversal is not allowed".to_string(),
            ));
        }
    }
    let canonical_root = root.canonicalize()?;
    let path = canonical_root.join(rel_path);
    if !path.starts_with(&canonical_root) {
        return Err(AppError::ValidationError(
            "path escapes repository root".to_string(),
        ));
    }
    Ok(path)
}
