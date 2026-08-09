//! Repository instruction discovery and explicit @file context loading.

use super::*;

pub(in crate::routes::rd) async fn load_repository_instructions_for_task(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: Option<&str>,
    source_stage: &str,
) -> Result<RdRepositoryInstructionContext, AppError> {
    let Some(repo_id) = repository_id else {
        return Ok(RdRepositoryInstructionContext::default());
    };
    match build_repository_instruction_context(state, claims, repo_id).await {
        Ok(context) => {
            if !context.is_empty() {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "repository_instructions",
                    "completed",
                    "已加载仓库内开发规范",
                    json!({
                        "repositoryId": repo_id,
                        "sourceStage": source_stage,
                        "files": context.files.clone(),
                        "chars": context.text.chars().count(),
                    }),
                )
                .await?;
            }
            Ok(context)
        }
        Err(error) => {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "repository_instructions",
                "skipped",
                "仓库内开发规范加载失败，已继续任务",
                json!({
                    "repositoryId": repo_id,
                    "sourceStage": source_stage,
                    "error": error.to_string(),
                    "nonBlocking": true,
                }),
            )
            .await?;
            Ok(RdRepositoryInstructionContext::default())
        }
    }
}

pub(in crate::routes::rd) async fn load_prompt_file_context_for_task(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: Option<&str>,
    prompt: &str,
) -> Result<RdExplicitFileContext, AppError> {
    let Some(repo_id) = repository_id else {
        return Ok(RdExplicitFileContext::default());
    };
    let refs = extract_prompt_file_references(prompt);
    if refs.is_empty() {
        return Ok(RdExplicitFileContext::default());
    }
    let root = repository_root(state, claims, repo_id).await?;
    let context = read_prompt_file_context(&root, refs);
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "context",
        "completed",
        "已处理用户 @ 文件上下文",
        json!({
            "repositoryId": repo_id,
            "files": context.files.clone(),
            "skipped": context.skipped.clone(),
            "chars": context.text.chars().count(),
            "maxFiles": MAX_EXPLICIT_FILE_CONTEXT_FILES,
            "maxBytes": MAX_EXPLICIT_FILE_CONTEXT_BYTES,
        }),
    )
    .await?;
    Ok(context)
}

fn read_prompt_file_context(root: &Path, refs: Vec<String>) -> RdExplicitFileContext {
    let mut context = RdExplicitFileContext::default();
    let mut used_bytes = 0usize;

    for rel in refs.into_iter().take(MAX_EXPLICIT_FILE_CONTEXT_FILES) {
        if used_bytes >= MAX_EXPLICIT_FILE_CONTEXT_BYTES {
            context
                .skipped
                .push(format!("{rel}: context budget exhausted"));
            continue;
        }
        match read_prompt_file_context_entry(
            root,
            &rel,
            MAX_EXPLICIT_FILE_CONTEXT_BYTES - used_bytes,
        ) {
            Ok(Some(body)) => {
                used_bytes = used_bytes.saturating_add(body.len());
                context.files.push(rel.clone());
                context.text.push_str(&format!("\n### {rel}\n{body}\n"));
            }
            Ok(None) => context.skipped.push(format!("{rel}: empty file")),
            Err(reason) => context.skipped.push(format!("{rel}: {reason}")),
        }
    }

    if context.text.len() > MAX_EXPLICIT_FILE_CONTEXT_BYTES {
        context.text = truncate_text(&context.text, MAX_EXPLICIT_FILE_CONTEXT_BYTES);
    }
    context
}

fn read_prompt_file_context_entry(
    root: &Path,
    rel: &str,
    remaining_budget: usize,
) -> Result<Option<String>, String> {
    let path = safe_join(root, rel).map_err(|error| error.to_string())?;
    if should_skip_path(&path) {
        return Err("path is ignored by repository safety rules".to_string());
    }
    let meta = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err("path is not a file".to_string());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("file is too large: {} bytes", meta.len()));
    }
    let text = std::fs::read_to_string(&path).map_err(|_| "binary or non-UTF8 file".to_string())?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(truncate_text(trimmed, remaining_budget.min(12_000))))
}

fn extract_prompt_file_references(prompt: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut refs = Vec::new();
    for token in prompt.split_whitespace() {
        let token = token.trim_matches(is_prompt_reference_boundary);
        let Some(raw) = token.strip_prefix('@') else {
            continue;
        };
        let Some(path) = normalize_prompt_file_reference(raw) else {
            continue;
        };
        if seen.insert(path.clone()) {
            refs.push(path);
        }
    }
    refs
}

fn normalize_prompt_file_reference(raw: &str) -> Option<String> {
    let mut value = raw
        .trim()
        .trim_matches(is_prompt_reference_boundary)
        .trim_start_matches("./")
        .replace('\\', "/");
    while value.chars().last().is_some_and(|ch| {
        matches!(
            ch,
            ',' | '，' | '.' | '。' | ';' | '；' | ':' | '：' | ')' | '）' | ']' | '】'
        )
    }) {
        value.pop();
    }
    if let Some(idx) = value.rfind("#L") {
        if value[idx + 2..].chars().all(|ch| ch.is_ascii_digit()) {
            value.truncate(idx);
        }
    }
    if let Some(idx) = value.rfind(':') {
        if value[idx + 1..].chars().all(|ch| ch.is_ascii_digit()) {
            value.truncate(idx);
        }
    }
    let value = value.trim_matches(is_prompt_reference_boundary).to_string();
    if value.is_empty()
        || value.len() > 240
        || value.starts_with('/')
        || value.contains("://")
        || value.contains('\0')
    {
        return None;
    }
    Some(value)
}

fn is_prompt_reference_boundary(ch: char) -> bool {
    matches!(
        ch,
        '`' | '"'
            | '\''
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '，'
            | ','
            | '。'
            | ';'
            | '；'
    )
}

async fn build_repository_instruction_context(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
) -> Result<RdRepositoryInstructionContext, AppError> {
    let root = repository_root(state, claims, repository_id).await?;
    Ok(read_repository_instruction_context(&root))
}

fn read_repository_instruction_context(root: &Path) -> RdRepositoryInstructionContext {
    let mut files = Vec::new();
    let mut parts = Vec::new();
    let mut used_bytes = 0usize;
    for path in repository_instruction_candidates(root) {
        if used_bytes >= MAX_REPOSITORY_INSTRUCTION_BYTES {
            break;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .ok()
            .and_then(|value| value.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("repository-instructions"))
            .replace('\\', "/");
        let remaining = MAX_REPOSITORY_INSTRUCTION_BYTES.saturating_sub(used_bytes);
        let body = truncate_text(trimmed, remaining.min(8_000));
        used_bytes = used_bytes.saturating_add(body.len());
        files.push(rel.clone());
        parts.push(format!("### {rel}\n{body}"));
    }

    RdRepositoryInstructionContext {
        text: truncate_text(&parts.join("\n\n"), MAX_REPOSITORY_INSTRUCTION_BYTES),
        files,
    }
}

fn repository_instruction_candidates(root: &Path) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for rel in [
        "CLAUDE.md",
        "AGENTS.md",
        "AOS.md",
        ".cursorrules",
        ".windsurfrules",
        ".github/copilot-instructions.md",
    ] {
        let path = root.join(rel);
        if path.is_file() && seen.insert(rel.to_string()) {
            candidates.push(path);
        }
    }
    for rel_dir in [
        ".cursor/rules",
        ".kiro/steering",
        ".trae/rules",
        ".github/instructions",
    ] {
        let dir = root.join(rel_dir);
        if !dir.is_dir() {
            continue;
        }
        let mut dir_files = WalkDir::new(&dir)
            .max_depth(2)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| {
                path.extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|ext| matches!(ext, "md" | "mdc" | "txt"))
            })
            .collect::<Vec<_>>();
        dir_files.sort();
        for path in dir_files {
            let rel = path
                .strip_prefix(root)
                .ok()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .replace('\\', "/");
            if !rel.is_empty() && seen.insert(rel) {
                candidates.push(path);
            }
            if candidates.len() >= 40 {
                return candidates;
            }
        }
    }
    candidates
}
