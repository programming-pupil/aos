//! Incremental file summary indexing for RD repositories.

use super::*;

pub(in crate::routes::rd) async fn rebuild_repository_file_summary_index(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    root: &Path,
) -> Result<usize, AppError> {
    let existing =
        load_existing_repository_file_summary_cache(db, tenant_id, repository_id).await?;
    let root = root.to_path_buf();
    let output =
        tokio::task::spawn_blocking(move || collect_repository_file_summaries(&root, existing))
            .await
            .map_err(|error| {
                AppError::Internal(format!("file summary indexing task failed: {error}"))
            })?;

    let keep_hashes = output
        .summaries
        .iter()
        .map(|summary| stable_hash_hex(&summary.file_path))
        .collect::<HashSet<_>>();

    for summary in &output.summaries {
        sqlx::query("INSERT INTO rd_repository_file_summaries \
            (tenant_id, repository_id, file_path, file_path_hash, language, size_bytes, mtime_ms, content_hash, git_blob_sha, summary_text, summary_hash, symbols_json, imports_json, last_indexed_at) \
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) \
            ON CONFLICT DO UPDATE SET \
              language = excluded.language, \
              size_bytes = excluded.size_bytes, \
              mtime_ms = excluded.mtime_ms, \
              content_hash = excluded.content_hash, \
              git_blob_sha = excluded.git_blob_sha, \
              summary_text = excluded.summary_text, \
              summary_hash = excluded.summary_hash, \
              symbols_json = excluded.symbols_json, \
              imports_json = excluded.imports_json, \
              embedding_model = IIF(content_hash <> excluded.content_hash, NULL, embedding_model), \
              embedding_content_hash = IIF(content_hash <> excluded.content_hash, NULL, embedding_content_hash), \
              last_indexed_at = CURRENT_TIMESTAMP, \
              updated_at = CURRENT_TIMESTAMP")
            .bind(tenant_id)
            .bind(repository_id)
            .bind(&summary.file_path)
            .bind(stable_hash_hex(&summary.file_path))
            .bind(&summary.language)
            .bind(i64::try_from(summary.size_bytes).unwrap_or(i64::MAX))
            .bind(summary.mtime_ms.and_then(|value| i64::try_from(value).ok()))
            .bind(&summary.content_hash)
            .bind(&summary.git_blob_sha)
            .bind(&summary.summary_text)
            .bind(&summary.summary_hash)
            .bind(json!(summary.symbols))
            .bind(json!(summary.imports))
            .execute(db)
            .await?;
    }

    prune_repository_file_summary_cache(db, tenant_id, repository_id, &keep_hashes).await?;
    update_rd_repository_file_summary_cache_stats(
        db,
        tenant_id,
        repository_id,
        output.reused_count,
        output.regenerated_count,
    )
    .await?;

    Ok(output.summaries.len())
}

async fn load_existing_repository_file_summary_cache(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
) -> Result<HashMap<String, RdExistingFileSummaryCache>, AppError> {
    let rows = sqlx::query(
        "SELECT file_path, file_path_hash, language, size_bytes, mtime_ms, content_hash, git_blob_sha,
                summary_text, summary_hash,
                CAST(symbols_json AS TEXT) AS symbols_json,
                CAST(imports_json AS TEXT) AS imports_json
         FROM rd_repository_file_summaries
         WHERE tenant_id = ? AND repository_id = ?",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .fetch_all(db)
    .await?;

    let mut cache = HashMap::new();
    for row in rows {
        let file_path: String = row.get("file_path");
        let summary_text: String = row.get("summary_text");
        let summary_hash = row
            .get::<Option<String>, _>("summary_hash")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| stable_hash_hex(&summary_text));
        cache.insert(
            row.get::<String, _>("file_path_hash"),
            RdExistingFileSummaryCache {
                file_path,
                language: row.get("language"),
                size_bytes: row.get("size_bytes"),
                mtime_ms: row.get("mtime_ms"),
                content_hash: row.get("content_hash"),
                git_blob_sha: row.get("git_blob_sha"),
                summary_text,
                summary_hash,
                symbols: parse_json_string_array(row.get::<Option<String>, _>("symbols_json")),
                imports: parse_json_string_array(row.get::<Option<String>, _>("imports_json")),
            },
        );
    }
    Ok(cache)
}

async fn prune_repository_file_summary_cache(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    keep_hashes: &HashSet<String>,
) -> Result<(), AppError> {
    if keep_hashes.is_empty() {
        sqlx::query(
            "DELETE FROM rd_repository_file_summaries WHERE tenant_id = ? AND repository_id = ?",
        )
        .bind(tenant_id)
        .bind(repository_id)
        .execute(db)
        .await?;
        return Ok(());
    }

    let mut builder =
        QueryBuilder::<Sqlite>::new("DELETE FROM rd_repository_file_summaries WHERE tenant_id = ");
    builder
        .push_bind(tenant_id)
        .push(" AND repository_id = ")
        .push_bind(repository_id)
        .push(" AND file_path_hash NOT IN (");
    {
        let mut separated = builder.separated(", ");
        for hash in keep_hashes {
            separated.push_bind(hash);
        }
        separated.push_unseparated(")");
    }
    builder.build().execute(db).await?;
    Ok(())
}

async fn update_rd_repository_file_summary_cache_stats(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    reused_count: usize,
    regenerated_count: usize,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE rd_repository_indexes
         SET detail_json = JSON_SET(
             COALESCE(detail_json, JSON_OBJECT()),
             '$.fileSummaryReusedCount', ?,
             '$.fileSummaryRegeneratedCount', ?,
             '$.fileSummaryUpdatedAt', strftime('%Y-%m-%dT%H:%M:%SZ', CURRENT_TIMESTAMP)
         )
         WHERE tenant_id = ? AND repository_id = ?",
    )
    .bind(i64::try_from(reused_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(regenerated_count).unwrap_or(i64::MAX))
    .bind(tenant_id)
    .bind(repository_id)
    .execute(db)
    .await?;
    Ok(())
}

fn collect_repository_file_summaries(
    root: &Path,
    existing: HashMap<String, RdExistingFileSummaryCache>,
) -> RdFileSummaryIndexOutput {
    let mut summaries = Vec::new();
    let git_blob_shas = load_git_blob_sha_map(root);
    let mut reused_count = 0usize;
    let mut regenerated_count = 0usize;
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !should_skip_path(entry.path()));
    for entry in walker.filter_map(Result::ok) {
        if summaries.len() >= RD_FILE_SUMMARY_INDEX_LIMIT {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        let file_path_hash = stable_hash_hex(&rel);
        let mtime_ms = file_mtime_ms(&meta);
        let git_blob_sha = git_blob_shas.get(&rel).cloned();
        if let Some(existing_summary) = existing.get(&file_path_hash) {
            if existing_file_summary_is_fresh(
                existing_summary,
                meta.len(),
                mtime_ms,
                git_blob_sha.as_deref(),
            ) {
                summaries.push(RdRepositoryFileSummary {
                    file_path: existing_summary.file_path.clone(),
                    language: existing_summary.language.clone(),
                    size_bytes: existing_summary.size_bytes,
                    mtime_ms,
                    content_hash: existing_summary.content_hash.clone(),
                    git_blob_sha,
                    summary_text: existing_summary.summary_text.clone(),
                    summary_hash: existing_summary.summary_hash.clone(),
                    symbols: existing_summary.symbols.clone(),
                    imports: existing_summary.imports.clone(),
                });
                reused_count = reused_count.saturating_add(1);
                continue;
            }
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        summaries.push(build_repository_file_summary(
            path,
            &rel,
            meta.len(),
            mtime_ms,
            git_blob_sha,
            &content,
        ));
        regenerated_count = regenerated_count.saturating_add(1);
    }
    RdFileSummaryIndexOutput {
        summaries,
        reused_count,
        regenerated_count,
    }
}

fn build_repository_file_summary(
    path: &Path,
    rel: &str,
    size_bytes: u64,
    mtime_ms: Option<u64>,
    git_blob_sha: Option<String>,
    content: &str,
) -> RdRepositoryFileSummary {
    let language = language_for_path(path);
    let symbols = extract_lightweight_file_symbols(content, 16);
    let imports = extract_lightweight_file_imports(content, 16);
    let preview = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("//") || line.len() > 4)
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let summary_text = truncate_text(
        &format!(
            "path={rel}; language={}; bytes={size_bytes}; symbols={}; imports={}; preview={}",
            language.as_deref().unwrap_or("text"),
            symbols.join(", "),
            imports.join(", "),
            preview
        ),
        1_200,
    );
    RdRepositoryFileSummary {
        file_path: rel.to_string(),
        language,
        size_bytes,
        mtime_ms,
        content_hash: stable_hash_hex(content),
        git_blob_sha,
        summary_hash: stable_hash_hex(&summary_text),
        summary_text,
        symbols,
        imports,
    }
}

fn existing_file_summary_is_fresh(
    existing: &RdExistingFileSummaryCache,
    size_bytes: u64,
    mtime_ms: Option<u64>,
    git_blob_sha: Option<&str>,
) -> bool {
    if existing.size_bytes != size_bytes {
        return false;
    }
    match (existing.git_blob_sha.as_deref(), git_blob_sha) {
        (Some(previous), Some(current)) if !previous.is_empty() && previous == current => true,
        (Some(_), Some(_)) => false,
        _ => existing.mtime_ms.is_some() && existing.mtime_ms == mtime_ms,
    }
}

fn file_mtime_ms(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn load_git_blob_sha_map(root: &Path) -> HashMap<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .arg("-s")
        .arg("-z")
        .output();
    let Ok(output) = output else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    let mut map = HashMap::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(entry);
        let Some((meta, path)) = text.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split_whitespace();
        let _mode = parts.next();
        let Some(sha) = parts.next() else {
            continue;
        };
        let normalized = path.replace('\\', "/");
        if !normalized.is_empty() && sha.len() >= 40 {
            map.insert(normalized, sha.to_string());
        }
    }
    map
}

fn extract_lightweight_file_symbols(content: &str, limit: usize) -> Vec<String> {
    let mut symbols = BTreeSet::new();
    for line in content.lines().take(2_000) {
        let trimmed = line.trim_start();
        for prefix in [
            "fn ",
            "pub fn ",
            "struct ",
            "pub struct ",
            "enum ",
            "pub enum ",
            "trait ",
            "pub trait ",
            "impl ",
            "class ",
            "interface ",
            "function ",
            "def ",
            "const ",
            "export function ",
            "export const ",
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(name) = rest
                    .split(|ch: char| {
                        ch.is_whitespace()
                            || matches!(ch, '(' | '<' | ':' | '=' | '{' | '[' | ';' | ',')
                    })
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    symbols.insert(name.to_string());
                }
                break;
            }
        }
        if symbols.len() >= limit {
            break;
        }
    }
    symbols.into_iter().take(limit).collect()
}

fn extract_lightweight_file_imports(content: &str, limit: usize) -> Vec<String> {
    let mut imports = BTreeSet::new();
    for line in content.lines().take(600) {
        let trimmed = line.trim();
        if trimmed.starts_with("use ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("from ")
            || trimmed.starts_with("require(")
            || trimmed.starts_with("#include")
            || trimmed.starts_with("package ")
        {
            imports.insert(truncate_text(trimmed, 180));
        }
        if imports.len() >= limit {
            break;
        }
    }
    imports.into_iter().take(limit).collect()
}
