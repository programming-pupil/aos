use std::collections::BTreeSet;
use std::path::Path;

use crate::error::AppError;

pub(crate) struct UnifiedDiffSplit {
    pub(crate) header: Vec<String>,
    pub(crate) hunks: Vec<Vec<String>>,
}

pub(crate) fn split_unified_diff_hunks(patch: &str) -> Result<UnifiedDiffSplit, AppError> {
    let mut header = Vec::new();
    let mut hunks: Vec<Vec<String>> = Vec::new();
    let mut current_hunk: Option<Vec<String>> = None;
    for line in patch.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                hunks.push(hunk);
            }
            current_hunk = Some(vec![line.to_string()]);
        } else if let Some(hunk) = current_hunk.as_mut() {
            hunk.push(line.to_string());
        } else {
            header.push(line.to_string());
        }
    }
    if let Some(hunk) = current_hunk {
        hunks.push(hunk);
    }
    if header.is_empty() || hunks.is_empty() {
        return Err(AppError::ValidationError(
            "diff does not contain selectable hunks".to_string(),
        ));
    }
    Ok(UnifiedDiffSplit { header, hunks })
}

pub(crate) fn build_patch_from_hunks(
    split: &UnifiedDiffSplit,
    selected_indexes: &BTreeSet<usize>,
) -> Result<String, AppError> {
    if selected_indexes.is_empty() {
        return Err(AppError::ValidationError(
            "at least one hunk must be selected".to_string(),
        ));
    }
    let mut lines = split.header.clone();
    for index in selected_indexes {
        let Some(hunk) = split.hunks.get(*index) else {
            return Err(AppError::ValidationError(format!(
                "hunk index out of range: {index}"
            )));
        };
        lines.extend(hunk.iter().cloned());
    }
    Ok(format!("{}\n", lines.join("\n")))
}

pub(crate) async fn git_apply(root: &Path, patch: &str, check_only: bool) -> Result<(), AppError> {
    git_apply_with_mode(root, patch, check_only, false).await
}

pub(crate) async fn git_apply_reverse(
    root: &Path,
    patch: &str,
    check_only: bool,
) -> Result<(), AppError> {
    git_apply_with_mode(root, patch, check_only, true).await
}

pub(crate) async fn git_dirty_paths(root: &Path) -> Result<BTreeSet<String>, AppError> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(root)
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("git status spawn failed: {e}")))?;
    if !output.status.success() {
        return Err(AppError::ValidationError(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut paths = BTreeSet::new();
    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let path = line[3..].trim();
        if path.is_empty() {
            continue;
        }
        if let Some((old_path, new_path)) = path.split_once(" -> ") {
            insert_status_path(&mut paths, old_path);
            insert_status_path(&mut paths, new_path);
        } else {
            insert_status_path(&mut paths, path);
        }
    }
    Ok(paths)
}

fn insert_status_path(paths: &mut BTreeSet<String>, path: &str) {
    let path = path.trim().trim_matches('"').replace('\\', "/");
    if !path.is_empty() {
        paths.insert(path);
    }
}

async fn git_apply_with_mode(
    root: &Path,
    patch: &str,
    check_only: bool,
    reverse: bool,
) -> Result<(), AppError> {
    let mut command = tokio::process::Command::new("git");
    command.arg("apply");
    if reverse {
        command.arg("-R");
    }
    if check_only {
        command.arg("--check");
    }
    let mut child = command
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Internal(format!("git apply spawn failed: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(patch.as_bytes())
            .await
            .map_err(AppError::Io)?;
    }
    let output = child.wait_with_output().await.map_err(AppError::Io)?;
    if !output.status.success() {
        return Err(AppError::ValidationError(format!(
            "git apply failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_HUNK_DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn a() {
-    old_a();
+    new_a();
 }
@@ -8,3 +8,3 @@
 fn b() {
-    old_b();
+    new_b();
 }
";

    #[test]
    fn build_patch_from_selected_hunk_keeps_header_and_selected_hunk_only() {
        let split = split_unified_diff_hunks(TWO_HUNK_DIFF).expect("diff should split");
        assert_eq!(split.hunks.len(), 2);
        let selected = BTreeSet::from([1]);
        let patch = build_patch_from_hunks(&split, &selected).expect("patch should build");

        assert!(patch.contains("diff --git a/src/lib.rs b/src/lib.rs"));
        assert!(patch.contains("new_b"));
        assert!(!patch.contains("new_a"));
    }

    #[test]
    fn build_patch_from_hunks_rejects_out_of_range_index() {
        let split = split_unified_diff_hunks(TWO_HUNK_DIFF).expect("diff should split");
        let selected = BTreeSet::from([9]);
        assert!(build_patch_from_hunks(&split, &selected).is_err());
    }
}
