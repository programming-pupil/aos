//! Retrieval augmentation from repository import/dependency graph.

use super::*;

pub(super) async fn augment_repository_retrieval_with_dependency_graph(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    context_budget: RdContextBudget,
    file_notes: &mut HashMap<String, Vec<String>>,
    file_scores: &mut HashMap<String, f32>,
) -> Result<(), AppError> {
    if file_notes.is_empty() {
        return Ok(());
    }
    let candidate_files = file_notes
        .keys()
        .take(
            context_budget
                .retrieval_file_limit
                .saturating_mul(2)
                .max(16),
        )
        .cloned()
        .collect::<Vec<_>>();
    if candidate_files.is_empty() {
        return Ok(());
    }

    let file_rows = sqlx::query(
        "SELECT file_path \
         FROM rd_repository_file_summaries \
         WHERE tenant_id = ? AND repository_id = ? \
         LIMIT 3000",
    )
    .bind(&claims.tenant_id)
    .bind(repository_id)
    .fetch_all(&state.db)
    .await?;
    let all_files = file_rows
        .into_iter()
        .map(|row| row.get::<String, _>("file_path"))
        .collect::<BTreeSet<_>>();
    if all_files.is_empty() {
        return Ok(());
    }

    let mut builder = QueryBuilder::<Sqlite>::new(
        "SELECT file_path, language, import_path, import_kind, line_number \
         FROM rd_repository_imports \
         WHERE tenant_id = ",
    );
    builder
        .push_bind(&claims.tenant_id)
        .push(" AND repository_id = ")
        .push_bind(repository_id)
        .push(" AND file_path IN (");
    {
        let mut separated = builder.separated(", ");
        for file in &candidate_files {
            separated.push_bind(file);
        }
        separated.push_unseparated(") ORDER BY file_path ASC, line_number ASC LIMIT ");
    }
    builder.push_bind(i64::try_from(context_budget.retrieval_file_limit * 8).unwrap_or(240));
    let import_rows = builder.build().fetch_all(&state.db).await?;

    for row in import_rows {
        let importer_path: String = row.get("file_path");
        let language: Option<String> = row.get("language");
        let import_path: String = row.get("import_path");
        if !is_plausible_import_path(language.as_deref().unwrap_or("unknown"), &import_path) {
            continue;
        }
        let Some(resolved_file) = rd_resolve_local_import_to_file(
            &importer_path,
            language.as_deref().unwrap_or("unknown"),
            &import_path,
            &all_files,
        ) else {
            continue;
        };
        if resolved_file == importer_path {
            continue;
        }
        let import_kind: String = row.get("import_kind");
        let line_number: u64 = row.get("line_number");
        file_notes
            .entry(resolved_file.clone())
            .or_default()
            .push(format!(
                "dependency_graph(imported_by={}): {import_kind} `{}` at line {line_number}",
                importer_path,
                truncate_text(&import_path, 180)
            ));
        file_notes
            .entry(importer_path.clone())
            .or_default()
            .push(format!(
                "dependency_graph(imports={}): {import_kind} `{}` at line {line_number}",
                resolved_file,
                truncate_text(&import_path, 180)
            ));
        *file_scores.entry(resolved_file).or_default() += 1.25;
        *file_scores.entry(importer_path).or_default() += 0.35;
    }

    Ok(())
}

fn rd_resolve_local_import_to_file(
    importer_path: &str,
    language: &str,
    import_path: &str,
    all_files: &BTreeSet<String>,
) -> Option<String> {
    let import_path = import_path.trim();
    if import_path.is_empty() {
        return None;
    }
    let mut candidates = Vec::new();
    let importer_dir = Path::new(importer_path)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or_default()
        .replace('\\', "/");
    if import_path.starts_with('.') {
        candidates.push(rd_normalize_repo_relative_path(&format!(
            "{importer_dir}/{import_path}"
        )));
    } else if let Some(stripped) = import_path.strip_prefix("@/") {
        candidates.push(rd_normalize_repo_relative_path(stripped));
        candidates.push(rd_normalize_repo_relative_path(&format!("src/{stripped}")));
    } else if let Some(stripped) = import_path.strip_prefix("~/") {
        candidates.push(rd_normalize_repo_relative_path(stripped));
    } else if language.eq_ignore_ascii_case("rust")
        && (import_path.starts_with("crate::")
            || import_path.starts_with("super::")
            || import_path.starts_with("self::"))
    {
        let converted = import_path
            .trim_start_matches("crate::")
            .trim_start_matches("super::")
            .trim_start_matches("self::")
            .replace("::", "/");
        candidates.push(rd_normalize_repo_relative_path(&converted));
        candidates.push(rd_normalize_repo_relative_path(&format!("src/{converted}")));
    } else if matches!(
        language.to_ascii_lowercase().as_str(),
        "java" | "kotlin" | "scala" | "python"
    ) && import_path.contains('.')
    {
        candidates.push(rd_normalize_repo_relative_path(
            &import_path.replace('.', "/"),
        ));
    } else if import_path.starts_with("src/") || import_path.starts_with("app/") {
        candidates.push(rd_normalize_repo_relative_path(import_path));
    }

    for base in candidates {
        if let Some(resolved) = rd_match_repo_file_candidate(&base, all_files) {
            return Some(resolved);
        }
    }
    None
}

fn rd_match_repo_file_candidate(base: &str, all_files: &BTreeSet<String>) -> Option<String> {
    let base = rd_normalize_repo_relative_path(base);
    if base.is_empty() {
        return None;
    }
    let mut candidates = vec![base.clone()];
    for ext in [
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte", "rs", "py", "go", "java", "kt",
        "scala",
    ] {
        candidates.push(format!("{base}.{ext}"));
    }
    for index in [
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "mod.rs",
        "__init__.py",
    ] {
        candidates.push(format!("{base}/{index}"));
    }
    for candidate in &candidates {
        if all_files.contains(candidate) {
            return Some(candidate.clone());
        }
    }
    all_files
        .iter()
        .find(|file| candidates.iter().any(|candidate| file.ends_with(candidate)))
        .cloned()
}

pub(in crate::routes::rd) fn rd_normalize_repo_relative_path(raw: &str) -> String {
    let normalized = raw.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part.trim() {
            "" | "." => {}
            ".." => {
                let _ = parts.pop();
            }
            value => parts.push(value.to_string()),
        }
    }
    parts.join("/")
}
