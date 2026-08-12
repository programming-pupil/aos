//! Repository text search and @file suggestion endpoints.

use super::*;

pub(in crate::routes::rd) async fn repository_file_suggestions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Query(query): Query<RdRepositoryFileSuggestionQuery>,
) -> Result<Json<Vec<RdRepositoryFileSuggestionDto>>, AppError> {
    ensure_repository_exists(&state, &claims, &repository_id).await?;
    let needle = query.q.as_deref().unwrap_or_default().trim();
    if needle.chars().count() > 200 {
        return Err(AppError::ValidationError(
            "file suggestion query is too long".to_string(),
        ));
    }
    let limit = usize::try_from(query.limit.unwrap_or(30).clamp(1, 80)).unwrap_or(30);
    let mut suggestions = query_repository_file_suggestions(
        &state.db,
        &claims.tenant_id,
        &repository_id,
        needle,
        limit,
    )
    .await?;

    if suggestions.is_empty() {
        let root = repository_root(&state, &claims, &repository_id).await?;
        suggestions = run_rg_repository_file_suggestions(&root, needle, limit).await?;
    }

    Ok(Json(suggestions))
}

pub(in crate::routes::rd) async fn repository_search(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(repository_id): AxumPath<String>,
    Query(query): Query<RdRepositorySearchQuery>,
) -> Result<Json<Vec<RdRepositorySearchHitDto>>, AppError> {
    let needle = query.q.trim();
    if needle.is_empty() {
        return Err(AppError::ValidationError(
            "search query is required".to_string(),
        ));
    }
    if needle.chars().count() > 200 {
        return Err(AppError::ValidationError(
            "search query is too long".to_string(),
        ));
    }
    let limit = usize::try_from(query.limit.unwrap_or(30).clamp(1, 80)).unwrap_or(30);
    let root = repository_root(&state, &claims, &repository_id).await?;
    let hits = match run_rg_repository_search(&root, needle, limit).await? {
        Some(hits) => hits,
        None => manual_repository_search(&root, needle, limit),
    };
    Ok(Json(hits))
}

pub(in crate::routes::rd) async fn run_rg_repository_search(
    root: &Path,
    needle: &str,
    limit: usize,
) -> Result<Option<Vec<RdRepositorySearchHitDto>>, AppError> {
    let output = match timeout(
        Duration::from_secs(8),
        tokio::process::Command::new("rg")
            .args([
                "--line-number",
                "--no-heading",
                "--color",
                "never",
                "--smart-case",
                "--max-columns",
                "240",
                "--max-columns-preview",
                "--max-filesize",
                "512K",
                "--glob",
                "!.git",
                "--glob",
                "!node_modules",
                "--glob",
                "!target",
                "--glob",
                "!dist",
                "--glob",
                "!build",
                "--glob",
                "!.next",
                "--glob",
                "!.cache",
                "--glob",
                "!vendor",
                needle,
                ".",
            ])
            .current_dir(root)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(Err(error)) => {
            return Err(AppError::Internal(format!(
                "repository search failed: {error}"
            )));
        }
        Err(_) => return Ok(None),
    };

    if !output.status.success() && output.status.code() != Some(1) {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut hits = Vec::new();
    for line in stdout.lines() {
        if hits.len() >= limit {
            break;
        }
        let mut parts = line.splitn(3, ':');
        let Some(path) = parts.next().map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        let line_number = parts
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let snippet = parts.next().unwrap_or_default().trim();
        hits.push(RdRepositorySearchHitDto {
            path: path.trim_start_matches("./").to_string(),
            line_number,
            snippet: truncate_text(snippet, 500),
        });
    }
    Ok(Some(hits))
}

pub(in crate::routes::rd) async fn run_exact_repository_search(
    root: &Path,
    needle: &str,
    limit: usize,
) -> Result<Vec<RdRepositorySearchHitDto>, AppError> {
    let needle = needle.trim();
    if needle.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let output = match timeout(
        Duration::from_secs(8),
        tokio::process::Command::new("rg")
            .args([
                "--line-number",
                "--no-heading",
                "--color",
                "never",
                "--smart-case",
                "--fixed-strings",
                "--max-columns",
                "240",
                "--max-columns-preview",
                "--max-filesize",
                "512K",
                "--glob",
                "!.git",
                "--glob",
                "!node_modules",
                "--glob",
                "!target",
                "--glob",
                "!dist",
                "--glob",
                "!build",
                "--glob",
                "!.next",
                "--glob",
                "!.cache",
                "--glob",
                "!vendor",
                needle,
                ".",
            ])
            .current_dir(root)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(manual_repository_search(root, needle, limit));
        }
        Ok(Err(error)) => {
            return Err(AppError::Internal(format!(
                "exact repository search failed: {error}"
            )));
        }
        Err(_) => return Ok(manual_repository_search(root, needle, limit)),
    };

    if !output.status.success() && output.status.code() != Some(1) {
        return Ok(manual_repository_search(root, needle, limit));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut hits = Vec::new();
    for line in stdout.lines() {
        if hits.len() >= limit {
            break;
        }
        let mut parts = line.splitn(3, ':');
        let Some(path) = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let line_number = parts
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let snippet = parts.next().unwrap_or_default().trim();
        hits.push(RdRepositorySearchHitDto {
            path: path.trim_start_matches("./").to_string(),
            line_number,
            snippet: truncate_text(snippet, 500),
        });
    }
    Ok(hits)
}

async fn query_repository_file_suggestions(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    needle: &str,
    limit: usize,
) -> Result<Vec<RdRepositoryFileSuggestionDto>, AppError> {
    let limit = i64::try_from(limit).unwrap_or(30);
    let rows = if needle.is_empty() {
        sqlx::query(
            "SELECT file_path, language, size_bytes \
             FROM rd_repository_file_summaries \
             WHERE tenant_id = ? AND repository_id = ? \
             ORDER BY \
               CASE \
                 WHEN LOWER(file_path) IN ('readme.md', 'package.json', 'cargo.toml', 'pom.xml', 'build.gradle', 'settings.gradle', 'go.mod', 'pyproject.toml') THEN 0 \
                 WHEN LOWER(file_path) LIKE '%/readme.md' THEN 1 \
                 WHEN LOWER(file_path) LIKE '%/index.%' THEN 2 \
                 ELSE 3 \
               END, LENGTH(file_path) ASC, file_path ASC \
             LIMIT ?",
        )
        .bind(tenant_id)
        .bind(repository_id)
        .bind(limit)
        .fetch_all(db)
        .await?
    } else {
        let path_prefix = format!("{needle}%");
        let segment_prefix = format!("%/{needle}%");
        let contains = format!("%{needle}%");
        sqlx::query(
            "SELECT file_path, language, size_bytes \
             FROM rd_repository_file_summaries \
             WHERE tenant_id = ? AND repository_id = ? \
               AND (file_path LIKE ? OR file_path LIKE ? OR file_path LIKE ?) \
             ORDER BY \
               CASE \
                 WHEN file_path LIKE ? THEN 0 \
                 WHEN file_path LIKE ? THEN 1 \
                 WHEN file_path LIKE ? THEN 2 \
                 ELSE 3 \
               END, LENGTH(file_path) ASC, file_path ASC \
             LIMIT ?",
        )
        .bind(tenant_id)
        .bind(repository_id)
        .bind(&path_prefix)
        .bind(&segment_prefix)
        .bind(&contains)
        .bind(&path_prefix)
        .bind(&segment_prefix)
        .bind(&contains)
        .bind(limit)
        .fetch_all(db)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|row| {
            let path: String = row.get("file_path");
            let name = Path::new(&path)
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| path.clone());
            let size_bytes = row
                .try_get::<i64, _>("size_bytes")
                .ok()
                .and_then(|value| u64::try_from(value).ok());
            RdRepositoryFileSuggestionDto {
                path,
                name,
                language: row.try_get("language").ok(),
                size_bytes,
            }
        })
        .collect())
}

async fn run_rg_repository_file_suggestions(
    root: &Path,
    needle: &str,
    limit: usize,
) -> Result<Vec<RdRepositoryFileSuggestionDto>, AppError> {
    let output = match timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("rg")
            .args([
                "--files",
                "--hidden",
                "--glob",
                "!.git",
                "--glob",
                "!node_modules",
                "--glob",
                "!target",
                "--glob",
                "!dist",
                "--glob",
                "!build",
                "--glob",
                "!.next",
                "--glob",
                "!.cache",
                "--glob",
                "!vendor",
            ])
            .current_dir(root)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(manual_repository_file_suggestions(root, needle, limit));
        }
        Ok(Err(error)) => {
            return Err(AppError::Internal(format!(
                "repository file suggestion failed: {error}"
            )));
        }
        Err(_) => return Ok(manual_repository_file_suggestions(root, needle, limit)),
    };

    if !output.status.success() {
        return Ok(manual_repository_file_suggestions(root, needle, limit));
    }

    let mut candidates = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| normalize_repository_suggestion_path(line, needle))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        repository_file_suggestion_rank(left, needle)
            .cmp(&repository_file_suggestion_rank(right, needle))
            .then_with(|| left.len().cmp(&right.len()))
            .then_with(|| left.cmp(right))
    });
    candidates.truncate(limit);

    Ok(candidates
        .into_iter()
        .map(|path| {
            let name = Path::new(&path)
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| path.clone());
            RdRepositoryFileSuggestionDto {
                language: language_for_path(Path::new(&path)),
                path,
                name,
                size_bytes: None,
            }
        })
        .collect())
}

fn manual_repository_file_suggestions(
    root: &Path,
    needle: &str,
    limit: usize,
) -> Vec<RdRepositoryFileSuggestionDto> {
    let mut candidates = Vec::new();
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !should_skip_path(entry.path()));
    for entry in walker.filter_map(Result::ok) {
        if candidates.len() >= limit.saturating_mul(8).max(limit) {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Some(path) = normalize_repository_suggestion_path(&rel, needle) else {
            continue;
        };
        let size_bytes = entry.metadata().ok().map(|meta| meta.len());
        let name = Path::new(&path)
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| path.clone());
        candidates.push(RdRepositoryFileSuggestionDto {
            language: language_for_path(Path::new(&path)),
            path,
            name,
            size_bytes,
        });
    }
    candidates.sort_by(|left, right| {
        repository_file_suggestion_rank(&left.path, needle)
            .cmp(&repository_file_suggestion_rank(&right.path, needle))
            .then_with(|| left.path.len().cmp(&right.path.len()))
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates.truncate(limit);
    candidates
}

fn normalize_repository_suggestion_path(path: &str, needle: &str) -> Option<String> {
    let normalized = path.trim().trim_start_matches("./").replace('\\', "/");
    if normalized.is_empty() {
        return None;
    }
    if needle.is_empty() || repository_file_path_matches(&normalized, needle) {
        Some(normalized)
    } else {
        None
    }
}

fn repository_file_path_matches(path: &str, needle: &str) -> bool {
    let path_lower = path.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    path_lower.starts_with(&needle_lower)
        || path_lower
            .split('/')
            .any(|segment| segment.starts_with(&needle_lower))
        || path_lower.contains(&needle_lower)
}

fn repository_file_suggestion_rank(path: &str, needle: &str) -> u8 {
    if needle.is_empty() {
        return match path.to_ascii_lowercase().as_str() {
            "readme.md" | "package.json" | "cargo.toml" | "pom.xml" | "build.gradle"
            | "settings.gradle" | "go.mod" | "pyproject.toml" => 0,
            _ => {
                if path.to_ascii_lowercase().ends_with("/readme.md") {
                    1
                } else if path.contains("/index.") {
                    2
                } else {
                    3
                }
            }
        };
    }
    let path_lower = path.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    if path_lower.starts_with(&needle_lower) {
        0
    } else if path_lower
        .split('/')
        .any(|segment| segment.starts_with(&needle_lower))
    {
        1
    } else if path_lower.contains(&needle_lower) {
        2
    } else {
        3
    }
}

fn manual_repository_search(
    root: &Path,
    needle: &str,
    limit: usize,
) -> Vec<RdRepositorySearchHitDto> {
    let needle_lower = needle.to_ascii_lowercase();
    let mut hits = Vec::new();
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !should_skip_path(entry.path()));
    for entry in walker.filter_map(Result::ok) {
        if hits.len() >= limit {
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
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if hits.len() >= limit {
                break;
            }
            if line.to_ascii_lowercase().contains(&needle_lower) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                hits.push(RdRepositorySearchHitDto {
                    path: rel,
                    line_number: u64::try_from(idx + 1).unwrap_or(0),
                    snippet: truncate_text(line.trim(), 500),
                });
            }
        }
    }
    hits
}
