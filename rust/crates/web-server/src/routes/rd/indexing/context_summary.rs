//! Deterministic repository context summary indexing for RD.

use super::*;

pub(in crate::routes::rd) async fn rebuild_repository_context_summary_index(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    root: &Path,
    detection: &RdRepositoryDetection,
) -> Result<usize, AppError> {
    let file_summaries =
        load_repository_file_summaries_for_context(db, tenant_id, repository_id, 800).await?;
    if file_summaries.is_empty() {
        return Ok(0);
    }

    let mut summaries = Vec::new();
    summaries.push(build_repository_level_context_summary(
        root,
        detection,
        &file_summaries,
    ));
    if let Some(entrypoints) = build_entrypoint_context_summary(&file_summaries) {
        summaries.push(entrypoints);
    }
    summaries.extend(build_directory_context_summaries(&file_summaries));

    sqlx::query(
        "DELETE FROM rd_repository_context_summaries WHERE tenant_id = ? AND repository_id = ?",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .execute(db)
    .await?;

    for summary in &summaries {
        sqlx::query(
            "INSERT INTO rd_repository_context_summaries
             (tenant_id, repository_id, scope_type, scope_key, scope_key_hash, source_hash, summary_text, detail_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT DO UPDATE SET
               llm_summary_text = IIF(source_hash <> excluded.source_hash, NULL, llm_summary_text),
               llm_model = IIF(source_hash <> excluded.source_hash, NULL, llm_model),
               llm_updated_at = IIF(source_hash <> excluded.source_hash, NULL, llm_updated_at),
               source_hash = excluded.source_hash,
               summary_text = excluded.summary_text,
               detail_json = excluded.detail_json,
               updated_at = CURRENT_TIMESTAMP",
        )
        .bind(tenant_id)
        .bind(repository_id)
        .bind(&summary.scope_type)
        .bind(&summary.scope_key)
        .bind(stable_hash_hex(format!(
            "{}:{}",
            summary.scope_type, summary.scope_key
        )))
        .bind(&summary.source_hash)
        .bind(&summary.summary_text)
        .bind(&summary.detail_json)
        .execute(db)
        .await?;
    }

    Ok(summaries.len())
}

async fn load_repository_file_summaries_for_context(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    limit: usize,
) -> Result<Vec<RdRepositoryFileSummary>, AppError> {
    let rows = sqlx::query(
        "SELECT file_path, language, size_bytes, mtime_ms, content_hash, git_blob_sha, summary_text, summary_hash,
                CAST(symbols_json AS TEXT) AS symbols_json,
                CAST(imports_json AS TEXT) AS imports_json
         FROM rd_repository_file_summaries
         WHERE tenant_id = ? AND repository_id = ?
         ORDER BY file_path ASC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| RdRepositoryFileSummary {
            file_path: row.get("file_path"),
            language: row.get("language"),
            size_bytes: row.get("size_bytes"),
            mtime_ms: row.get("mtime_ms"),
            content_hash: row.get("content_hash"),
            git_blob_sha: row.get("git_blob_sha"),
            summary_text: row.get("summary_text"),
            summary_hash: row
                .get::<Option<String>, _>("summary_hash")
                .unwrap_or_else(|| stable_hash_hex(row.get::<String, _>("summary_text"))),
            symbols: parse_json_string_array(row.get::<Option<String>, _>("symbols_json")),
            imports: parse_json_string_array(row.get::<Option<String>, _>("imports_json")),
        })
        .collect())
}

fn build_repository_level_context_summary(
    root: &Path,
    detection: &RdRepositoryDetection,
    files: &[RdRepositoryFileSummary],
) -> RdRepositoryContextSummary {
    let mut language_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut directory_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut key_files = Vec::new();
    let mut manifest_files = Vec::new();
    let mut source_hash_parts = Vec::new();

    for file in files {
        total_bytes = total_bytes.saturating_add(file.size_bytes);
        source_hash_parts.push(format!("{}:{}", file.file_path, file.content_hash));
        if let Some(language) = file.language.as_deref().filter(|value| !value.is_empty()) {
            *language_counts.entry(language.to_string()).or_default() += 1;
        }
        *directory_counts
            .entry(rd_top_level_scope(&file.file_path))
            .or_default() += 1;
        if is_rd_manifest_or_config_file(&file.file_path) {
            manifest_files.push(file.file_path.clone());
        }
        if is_rd_high_value_entry_file(&file.file_path) {
            key_files.push(format!(
                "- `{}`: {}",
                file.file_path,
                truncate_text(&file.summary_text, 220)
            ));
        }
    }

    let language_line = language_counts
        .iter()
        .rev()
        .take(8)
        .map(|(language, count)| format!("{language}({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    let directory_line = directory_counts
        .iter()
        .take(12)
        .map(|(dir, count)| format!("{dir}({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    let detection_languages = detection
        .languages
        .iter()
        .take(8)
        .map(|item| format!("{}({})", item.language, item.file_count))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest_line = manifest_files
        .iter()
        .take(18)
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let key_file_lines = if key_files.is_empty() {
        "未从确定性索引识别到高价值入口文件；请 runtime 从 manifest/文件树继续定位。".to_string()
    } else {
        key_files
            .into_iter()
            .take(16)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let primary_language = detection.primary_language.clone().unwrap_or_else(|| {
        language_counts
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(language, _)| language.clone())
            .unwrap_or_else(|| "unknown".to_string())
    });
    let tree = collect_flat_tree(root, 80).join("\n");
    let summary_text = format!(
        "# 仓库级上下文摘要\n\
         - 主要语言：{}\n\
         - 检测语言：{}\n\
         - 技术栈：{}\n\
         - 包管理器：{}\n\
         - 默认测试命令：{}\n\
         - 默认构建命令：{}\n\
         - 索引文件数：{}，估算大小：{} bytes\n\
         - 主要目录：{}\n\
         - Manifest/配置入口：{}\n\n\
         ## 高价值入口候选\n{}\n\n\
         ## 轻量文件树\n{}\n\n\
         使用方式：这是仓库地图，不是代码事实。回答架构/概览问题时优先用它缩小范围；涉及关键判断、实现细节或 Diff 前必须读取真实文件核对。",
        primary_language,
        if detection_languages.is_empty() {
            if language_line.is_empty() {
                "unknown"
            } else {
                language_line.as_str()
            }
        } else {
            detection_languages.as_str()
        },
        if detection.stack.is_empty() {
            "unknown".to_string()
        } else {
            detection.stack.join(", ")
        },
        detection.package_manager.as_deref().unwrap_or("unknown"),
        detection.detected_test_command.as_deref().unwrap_or("unknown"),
        detection.detected_build_command.as_deref().unwrap_or("unknown"),
        files.len(),
        total_bytes,
        directory_line,
        if manifest_line.is_empty() {
            "none".to_string()
        } else {
            manifest_line
        },
        key_file_lines,
        tree
    );

    RdRepositoryContextSummary {
        scope_type: "repository".to_string(),
        scope_key: "root".to_string(),
        source_hash: stable_hash_hex(source_hash_parts.join("\n")),
        summary_text: truncate_text(&summary_text, 18_000),
        detail_json: json!({
            "fileCount": files.len(),
            "totalBytes": total_bytes,
            "languages": language_counts,
            "directories": directory_counts,
            "detection": detection,
        }),
    }
}

fn build_entrypoint_context_summary(
    files: &[RdRepositoryFileSummary],
) -> Option<RdRepositoryContextSummary> {
    let mut candidates = files
        .iter()
        .filter(|file| is_rd_high_value_entry_file(&file.file_path))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by_key(|file| rd_entrypoint_rank(&file.file_path));
    let mut source_hash_parts = Vec::new();
    let mut lines = Vec::new();
    for file in candidates.into_iter().take(28) {
        source_hash_parts.push(format!("{}:{}", file.file_path, file.content_hash));
        lines.push(format!(
            "- `{}` [{}]: {}\n  - symbols: {}\n  - imports: {}",
            file.file_path,
            file.language.as_deref().unwrap_or("text"),
            truncate_text(&file.summary_text, 260),
            file.symbols
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            file.imports
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let summary_text = format!(
        "# 入口/配置上下文摘要\n{}\n\n使用方式：适合项目概览、启动方式、架构图、模块入口定位。关键细节仍需读取对应真实文件。",
        lines.join("\n")
    );
    Some(RdRepositoryContextSummary {
        scope_type: "entrypoints".to_string(),
        scope_key: "default".to_string(),
        source_hash: stable_hash_hex(source_hash_parts.join("\n")),
        summary_text: truncate_text(&summary_text, 14_000),
        detail_json: json!({"candidateCount": lines.len()}),
    })
}

fn build_directory_context_summaries(
    files: &[RdRepositoryFileSummary],
) -> Vec<RdRepositoryContextSummary> {
    #[derive(Default)]
    struct DirectoryAgg<'a> {
        files: Vec<&'a RdRepositoryFileSummary>,
        languages: BTreeMap<String, usize>,
        source_hash_parts: Vec<String>,
    }

    let mut dirs: BTreeMap<String, DirectoryAgg<'_>> = BTreeMap::new();
    for file in files {
        let dir = rd_top_level_scope(&file.file_path);
        let agg = dirs.entry(dir).or_default();
        agg.source_hash_parts
            .push(format!("{}:{}", file.file_path, file.content_hash));
        if let Some(language) = file.language.as_deref().filter(|value| !value.is_empty()) {
            *agg.languages.entry(language.to_string()).or_default() += 1;
        }
        agg.files.push(file);
    }

    let mut ranked = dirs.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(dir_a, agg_a), (dir_b, agg_b)| {
        let rank_a = rd_directory_rank(dir_a);
        let rank_b = rd_directory_rank(dir_b);
        rank_a
            .cmp(&rank_b)
            .then_with(|| agg_b.files.len().cmp(&agg_a.files.len()))
            .then_with(|| dir_a.cmp(dir_b))
    });

    ranked
        .into_iter()
        .take(36)
        .filter_map(|(dir, mut agg)| {
            if agg.files.is_empty() {
                return None;
            }
            agg.files
                .sort_by_key(|file| rd_entrypoint_rank(&file.file_path));
            let language_line = agg
                .languages
                .iter()
                .take(8)
                .map(|(language, count)| format!("{language}({count})"))
                .collect::<Vec<_>>()
                .join(", ");
            let file_lines = agg
                .files
                .iter()
                .take(12)
                .map(|file| {
                    format!(
                        "- `{}`: {}",
                        file.file_path,
                        truncate_text(&file.summary_text, 180)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let symbols = agg
                .files
                .iter()
                .flat_map(|file| file.symbols.iter().take(6).cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(24)
                .collect::<Vec<_>>()
                .join(", ");
            let imports = agg
                .files
                .iter()
                .flat_map(|file| file.imports.iter().take(4).cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(18)
                .collect::<Vec<_>>()
                .join(", ");
            let summary_text = format!(
                "# 目录 `{dir}` 上下文摘要\n\
                 - 文件数：{}\n\
                 - 语言：{}\n\
                 - 代表符号：{}\n\
                 - 代表 imports：{}\n\n\
                 ## 代表文件\n{}\n\n\
                 使用方式：这是目录模块地图。需要确认实现细节时读取对应真实文件。",
                agg.files.len(),
                if language_line.is_empty() {
                    "unknown"
                } else {
                    language_line.as_str()
                },
                if symbols.is_empty() {
                    "none"
                } else {
                    symbols.as_str()
                },
                if imports.is_empty() {
                    "none"
                } else {
                    imports.as_str()
                },
                file_lines
            );
            Some(RdRepositoryContextSummary {
                scope_type: "directory".to_string(),
                scope_key: dir.clone(),
                source_hash: stable_hash_hex(agg.source_hash_parts.join("\n")),
                summary_text: truncate_text(&summary_text, 10_000),
                detail_json: json!({
                    "fileCount": agg.files.len(),
                    "languages": agg.languages,
                }),
            })
        })
        .collect()
}

fn rd_top_level_scope(file_path: &str) -> String {
    file_path
        .split('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(".")
        .to_string()
}

fn rd_directory_rank(dir: &str) -> usize {
    match dir {
        "src" | "crates" | "apps" | "app" | "packages" | "web" | "frontend" | "backend" => 0,
        "server" | "api" | "routes" | "components" | "pages" | "cmd" | "internal" => 1,
        "config" | ".github" | "docker" | "infra" | "migrations" | "tests" | "test" => 2,
        "." => 3,
        _ => 4,
    }
}

fn is_rd_manifest_or_config_file(file_path: &str) -> bool {
    let lower = file_path.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "readme.md"
            | "readme"
            | "package.json"
            | "pnpm-workspace.yaml"
            | "yarn.lock"
            | "cargo.toml"
            | "cargo.lock"
            | "go.mod"
            | "pyproject.toml"
            | "pom.xml"
            | "dockerfile"
            | "docker-compose.yml"
            | "compose.yml"
    ) || lower.ends_with("/package.json")
        || lower.ends_with("/cargo.toml")
        || lower.ends_with("/pom.xml")
        || lower.ends_with("/go.mod")
}

fn is_rd_high_value_entry_file(file_path: &str) -> bool {
    let lower = file_path.to_ascii_lowercase();
    is_rd_manifest_or_config_file(&lower)
        || lower.contains("/main.")
        || lower.ends_with("main.rs")
        || lower.ends_with("lib.rs")
        || lower.ends_with("main.ts")
        || lower.ends_with("main.tsx")
        || lower.ends_with("main.js")
        || lower.ends_with("main.jsx")
        || lower.ends_with("app.ts")
        || lower.ends_with("app.tsx")
        || lower.ends_with("server.ts")
        || lower.ends_with("server.js")
        || lower.ends_with("index.ts")
        || lower.ends_with("index.tsx")
        || lower.ends_with("index.js")
        || lower.ends_with("mod.rs")
        || lower.contains("router")
        || lower.contains("routes")
        || lower.contains("controller")
        || lower.contains("application.java")
        || lower.contains("springboot")
}

fn rd_entrypoint_rank(file_path: &str) -> usize {
    let lower = file_path.to_ascii_lowercase();
    if lower == "readme.md" || lower == "readme" {
        0
    } else if is_rd_manifest_or_config_file(&lower) {
        1
    } else if lower.contains("/main.") || lower.ends_with("main.rs") || lower.ends_with("main.ts") {
        2
    } else if lower.contains("router") || lower.contains("routes") {
        3
    } else if lower.contains("controller") || lower.contains("server") || lower.contains("app.") {
        4
    } else {
        9
    }
}
