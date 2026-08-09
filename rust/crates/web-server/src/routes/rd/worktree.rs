//! Git worktree isolation and baseline handling for RD code tasks.

use super::*;

pub(super) use rd_core::diff::rd_baseline_should_skip_path;

pub(super) async fn read_rd_repository_worktree_status(
    root: &Path,
    repository_id: &str,
) -> Result<RdRepositoryWorktreeStatusDto, AppError> {
    let head_sha = git_head_sha(root).await?;
    let status_short = git_status_short(root).await?;
    let dirty_paths = parse_git_status_paths(&status_short);
    let untracked_files = git_untracked_files(root).await?;
    let tracked_modified_count = status_short
        .lines()
        .filter(|line| !line.trim_start().starts_with("??"))
        .count();
    Ok(RdRepositoryWorktreeStatusDto {
        repository_id: repository_id.to_string(),
        head_sha,
        dirty: !status_short.trim().is_empty(),
        dirty_path_count: dirty_paths.len(),
        tracked_modified_count,
        untracked_count: untracked_files.len(),
        dirty_paths_sample: dirty_paths.into_iter().take(30).collect(),
        status_short,
        default_baseline_policy: RdGitBaselinePolicy::CurrentWorktree.as_str().to_string(),
    })
}

async fn capture_rd_task_git_baseline_from_root(
    root: &Path,
    baseline_policy: RdGitBaselinePolicy,
) -> Result<RdTaskGitBaseline, AppError> {
    let head_sha = git_head_sha(root).await?;
    let status_short = git_status_short(root).await?;
    let dirty_paths = parse_git_status_paths(&status_short);
    let tracked_diff_patch = git_tracked_diff(root).await?;
    let untracked_files = git_untracked_files(root).await?;
    Ok(RdTaskGitBaseline {
        baseline_policy,
        head_sha,
        status_short,
        dirty_paths,
        tracked_diff_patch,
        untracked_files,
    })
}

pub(super) async fn capture_and_record_rd_task_git_baseline(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: &str,
    baseline_policy: RdGitBaselinePolicy,
) -> Result<RdTaskGitBaseline, AppError> {
    let root = repository_root(state, claims, repository_id).await?;
    let baseline = capture_rd_task_git_baseline_from_root(&root, baseline_policy).await?;
    let baseline_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO rd_task_git_baselines \
         (id, tenant_id, task_id, repository_id, baseline_policy, head_sha, status_short, dirty_paths_json, tracked_diff_patch, untracked_files_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT DO UPDATE SET \
           baseline_policy = excluded.baseline_policy, \
           head_sha = excluded.head_sha, \
           status_short = excluded.status_short, \
           dirty_paths_json = excluded.dirty_paths_json, \
           tracked_diff_patch = excluded.tracked_diff_patch, \
           untracked_files_json = excluded.untracked_files_json",
    )
    .bind(&baseline_id)
    .bind(&claims.tenant_id)
    .bind(task_id)
    .bind(repository_id)
    .bind(baseline.baseline_policy.as_str())
    .bind(&baseline.head_sha)
    .bind(&baseline.status_short)
    .bind(json!(baseline.dirty_paths.clone()))
    .bind(if baseline.tracked_diff_patch.trim().is_empty() {
        None::<&str>
    } else {
        Some(baseline.tracked_diff_patch.as_str())
    })
    .bind(json!(baseline.untracked_files.clone()))
    .execute(&state.db)
    .await?;

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "git_baseline",
        "completed",
        if baseline.is_dirty() {
            "已记录任务 Git 基线：当前仓库存在未提交变更，将作为当前代码状态读取，但不会归属于本任务 Diff"
        } else {
            "已记录任务 Git 基线：当前仓库工作区干净"
        },
        json!({
            "repositoryId": repository_id,
            "baselinePolicy": baseline.baseline_policy.as_str(),
            "headSha": baseline.head_sha.clone(),
            "dirty": baseline.is_dirty(),
            "dirtyPathCount": baseline.dirty_paths.len(),
            "dirtyPathsSample": baseline.dirty_paths.iter().take(30).cloned().collect::<Vec<_>>(),
            "trackedDiffBytes": baseline.tracked_diff_patch.len(),
            "untrackedFileCount": baseline.untracked_files.len(),
            "untrackedFilesSample": baseline.untracked_files.iter().take(30).cloned().collect::<Vec<_>>(),
        }),
    )
    .await?;
    Ok(baseline)
}

async fn git_head_sha(root: &Path) -> Result<Option<String>, AppError> {
    let output = run_git_output(root, &["rev-parse", "HEAD"], "git rev-parse HEAD").await?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

async fn git_status_short(root: &Path) -> Result<String, AppError> {
    let output = run_git_output(root, &["status", "--porcelain=v1"], "git status").await?;
    if !output.status.success() {
        return Err(AppError::ValidationError(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn git_tracked_diff(root: &Path) -> Result<String, AppError> {
    let output = run_git_output(
        root,
        &["diff", "--binary", "--no-ext-diff", "HEAD", "--", "."],
        "git diff HEAD",
    )
    .await?;
    if !output.status.success() {
        return Err(AppError::ValidationError(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn git_untracked_files(root: &Path) -> Result<Vec<String>, AppError> {
    let output = run_git_output(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "git ls-files --others",
    )
    .await?;
    if !output.status.success() {
        return Err(AppError::ValidationError(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut files = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| path.replace('\\', "/"))
        .filter(|path| !rd_baseline_should_skip_path(path))
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    Ok(files)
}

fn parse_git_status_paths(status_short: &str) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for line in status_short.lines() {
        if line.len() < 4 {
            continue;
        }
        let path = line[3..].trim();
        if let Some((old_path, new_path)) = path.split_once(" -> ") {
            insert_git_status_path(&mut paths, old_path);
            insert_git_status_path(&mut paths, new_path);
        } else {
            insert_git_status_path(&mut paths, path);
        }
    }
    paths
        .into_iter()
        .filter(|path| !rd_baseline_should_skip_path(path))
        .collect()
}

fn insert_git_status_path(paths: &mut BTreeSet<String>, path: &str) {
    let path = path.trim().trim_matches('"').replace('\\', "/");
    if !path.is_empty() {
        paths.insert(path);
    }
}

async fn apply_rd_baseline_to_candidate(
    repo_root: &Path,
    candidate_path: &Path,
    baseline: Option<&RdTaskGitBaseline>,
) -> Result<bool, AppError> {
    let Some(baseline) = baseline else {
        return Ok(false);
    };
    if baseline.baseline_policy != RdGitBaselinePolicy::CurrentWorktree || !baseline.is_dirty() {
        return Ok(false);
    }
    if !baseline.tracked_diff_patch.trim().is_empty() {
        git_apply(candidate_path, &baseline.tracked_diff_patch, false)
            .await
            .map_err(|error| {
                AppError::ValidationError(format!(
                    "apply current worktree baseline to candidate failed: {error}"
                ))
            })?;
    }
    copy_rd_baseline_untracked_files(repo_root, candidate_path, &baseline.untracked_files)
        .await
        .map_err(|error| {
            AppError::Internal(format!(
                "copy current worktree untracked baseline files to candidate failed: {error}"
            ))
        })?;
    commit_rd_candidate_baseline(candidate_path).await
}

async fn copy_rd_baseline_untracked_files(
    repo_root: &Path,
    candidate_path: &Path,
    files: &[String],
) -> Result<(), AppError> {
    for rel in files {
        if rd_baseline_should_skip_path(rel) {
            continue;
        }
        let src = safe_join(repo_root, rel)?;
        let dest = safe_join_allow_missing(candidate_path, rel)?;
        let metadata = match tokio::fs::metadata(&src).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(AppError::Internal(format!(
                    "read baseline source metadata failed: src={}, error={error}",
                    src.display()
                )));
            }
        };
        if metadata.is_dir() {
            tokio::fs::create_dir_all(&dest).await.map_err(|error| {
                AppError::Internal(format!(
                    "create baseline destination directory failed: dest={}, error={error}",
                    dest.display()
                ))
            })?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                AppError::Internal(format!(
                    "create baseline destination parent failed: parent={}, error={error}",
                    parent.display()
                ))
            })?;
        }
        tokio::fs::copy(&src, &dest).await.map_err(|error| {
            AppError::Internal(format!(
                "copy baseline untracked file failed: src={}, dest={}, error={error}",
                src.display(),
                dest.display()
            ))
        })?;
    }
    Ok(())
}

async fn commit_rd_candidate_baseline(worktree: &Path) -> Result<bool, AppError> {
    run_git_checked(worktree, &["add", "-A", "."], "git add baseline").await?;
    let status = git_status_short(worktree).await?;
    if status.trim().is_empty() {
        return Ok(false);
    }
    let output = run_git_output(
        worktree,
        &[
            "-c",
            "user.email=aos-code-studio@example.invalid",
            "-c",
            "user.name=AOS Code Studio",
            "commit",
            "-m",
            "AOS task baseline",
        ],
        "git commit baseline",
    )
    .await?;
    if !output.status.success() {
        return Err(AppError::ValidationError(format!(
            "git commit baseline failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(true)
}

#[derive(Debug)]
pub(super) struct RdCandidateWorktree {
    pub(super) repo_root: PathBuf,
    pub(super) path: PathBuf,
    pub(super) baseline_commit_created: bool,
}

pub(super) async fn create_rd_candidate_worktree(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: &str,
    baseline: Option<&RdTaskGitBaseline>,
    candidate_parent: Option<&Path>,
) -> Result<RdCandidateWorktree, AppError> {
    let repo_root = repository_root(state, claims, repository_id).await?;
    create_rd_candidate_worktree_from_root_with_baseline(
        &repo_root,
        task_id,
        baseline,
        candidate_parent,
    )
    .await
}

#[cfg(test)]
pub(super) async fn create_rd_candidate_worktree_from_root(
    repo_root: &Path,
    task_id: &str,
) -> Result<RdCandidateWorktree, AppError> {
    create_rd_candidate_worktree_from_root_with_baseline(repo_root, task_id, None, None).await
}

async fn create_rd_candidate_worktree_from_root_with_baseline(
    repo_root: &Path,
    task_id: &str,
    baseline: Option<&RdTaskGitBaseline>,
    candidate_parent: Option<&Path>,
) -> Result<RdCandidateWorktree, AppError> {
    let parent = match candidate_parent {
        Some(parent) => parent.to_path_buf(),
        None => repo_root
            .parent()
            .ok_or_else(|| AppError::Internal("repository root has no parent".to_string()))?
            .join(".aos-rd-candidates"),
    };
    tokio::fs::create_dir_all(&parent)
        .await
        .map_err(AppError::Io)?;
    let path = parent.join(task_id);
    if path.exists() {
        let stale_candidate = RdCandidateWorktree {
            repo_root: repo_root.to_path_buf(),
            path: path.clone(),
            baseline_commit_created: false,
        };
        cleanup_rd_candidate_worktree(&stale_candidate).await;
        if path.exists() {
            tokio::fs::remove_dir_all(&path).await.map_err(|error| {
                AppError::Internal(format!(
                    "remove stale RD candidate worktree directory failed: path={}, error={error}",
                    path.display()
                ))
            })?;
        }
    }

    run_git_checked(
        &repo_root,
        &["worktree", "add", "--detach", path_to_str(&path)?, "HEAD"],
        "git worktree add",
    )
    .await?;
    let mut candidate = RdCandidateWorktree {
        repo_root: repo_root.to_path_buf(),
        path,
        baseline_commit_created: false,
    };
    if let Err(error) = exclude_aos_runtime_dir_from_worktree(&candidate.path).await {
        cleanup_rd_candidate_worktree(&candidate).await;
        return Err(error);
    }
    match apply_rd_baseline_to_candidate(repo_root, &candidate.path, baseline).await {
        Ok(baseline_commit_created) => {
            candidate.baseline_commit_created = baseline_commit_created;
            Ok(candidate)
        }
        Err(error) => {
            cleanup_rd_candidate_worktree(&candidate).await;
            Err(error)
        }
    }
}

pub(super) async fn cleanup_rd_candidate_worktree(candidate: &RdCandidateWorktree) {
    let candidate_path = path_to_str_lossy(&candidate.path);
    match run_git_output(
        &candidate.repo_root,
        &["worktree", "remove", "--force", candidate_path.as_str()],
        "git worktree remove",
    )
    .await
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if git_worktree_remove_is_unsupported(&stderr) {
                tracing::debug!(
                    path = %candidate.path.display(),
                    "git worktree remove is unsupported by this git version; falling back to directory removal"
                );
            } else {
                tracing::warn!(
                    path = %candidate.path.display(),
                    stderr = %stderr.trim(),
                    "failed to remove RD candidate worktree via git; falling back to directory removal"
                );
            }
            let _ = tokio::fs::remove_dir_all(&candidate.path).await;
        }
        Err(error) => {
            tracing::warn!(
                path = %candidate.path.display(),
                "failed to spawn git worktree remove; falling back to directory removal: {}",
                error
            );
            let _ = tokio::fs::remove_dir_all(&candidate.path).await;
        }
    }
    let _ = run_git_checked(
        &candidate.repo_root,
        &["worktree", "prune"],
        "git worktree prune",
    )
    .await;
}

pub(super) fn git_worktree_remove_is_unsupported(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("usage: git worktree add")
        && !lower.contains("worktree remove")
        && lower.contains("worktree prune")
}

async fn exclude_aos_runtime_dir_from_worktree(worktree: &Path) -> Result<(), AppError> {
    let output = run_git_output(worktree, &["rev-parse", "--git-dir"], "git rev-parse").await?;
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return Ok(());
    }
    let git_dir = PathBuf::from(&raw);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        worktree.join(git_dir)
    };
    let info_dir = git_dir.join("info");
    tokio::fs::create_dir_all(&info_dir)
        .await
        .map_err(AppError::Io)?;
    let exclude_path = info_dir.join("exclude");
    let existing = tokio::fs::read_to_string(&exclude_path)
        .await
        .unwrap_or_default();
    if !existing.lines().any(|line| line.trim() == ".aos/") {
        let next = if existing.ends_with('\n') || existing.is_empty() {
            format!("{existing}.aos/\n")
        } else {
            format!("{existing}\n.aos/\n")
        };
        tokio::fs::write(&exclude_path, next)
            .await
            .map_err(AppError::Io)?;
    }
    Ok(())
}

pub(super) async fn extract_rd_candidate_diff(worktree: &Path) -> Result<String, AppError> {
    git_add_intent_for_candidate_untracked_files(worktree).await?;
    let output = run_git_output(
        worktree,
        &["diff", "--binary", "--no-ext-diff", "HEAD", "--", "."],
        "git diff",
    )
    .await?;
    let raw_diff = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(filter_rd_unified_diff_excluded_paths(&raw_diff).diff)
}

async fn git_add_intent_for_candidate_untracked_files(worktree: &Path) -> Result<(), AppError> {
    let files = git_untracked_files(worktree).await?;
    for chunk in files.chunks(100) {
        if chunk.is_empty() {
            continue;
        }
        let mut command = tokio::process::Command::new("git");
        command
            .arg("add")
            .arg("-N")
            .arg("--")
            .args(chunk)
            .current_dir(worktree);
        let output = command
            .output()
            .await
            .map_err(|error| AppError::Internal(format!("git add -N spawn failed: {error}")))?;
        if !output.status.success() {
            return Err(AppError::ValidationError(format!(
                "git add -N failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
    }
    Ok(())
}

async fn run_git_checked(root: &Path, args: &[&str], label: &str) -> Result<(), AppError> {
    let output = run_git_output(root, args, label).await?;
    if !output.status.success() {
        return Err(AppError::ValidationError(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

async fn run_git_output(
    root: &Path,
    args: &[&str],
    label: &str,
) -> Result<std::process::Output, AppError> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .await
        .map_err(|error| AppError::Internal(format!("{label} spawn failed: {error}")))
}

fn path_to_str(path: &Path) -> Result<&str, AppError> {
    path.to_str().ok_or_else(|| {
        AppError::ValidationError(format!("path is not valid UTF-8: {}", path.display()))
    })
}

fn path_to_str_lossy(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
