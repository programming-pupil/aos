//! Pure diff/path helpers shared by RD review, patch validation, and web adapters.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::should_skip_path;

#[derive(Debug, Default, Clone)]
pub struct RdDiffFilterResult {
    pub diff: String,
    pub excluded_paths: Vec<String>,
}

pub fn infer_files_from_unified_diff(diff: &str) -> Vec<String> {
    let mut files = BTreeSet::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("diff --git ") else {
            continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() >= 2 {
            let path = parts[1].trim_start_matches("b/").to_string();
            if !path.is_empty() && path != "/dev/null" {
                files.insert(path);
            }
        }
    }
    files.into_iter().collect()
}

pub fn filter_rd_unified_diff_excluded_paths(diff: &str) -> RdDiffFilterResult {
    if diff.trim().is_empty() {
        return RdDiffFilterResult {
            diff: String::new(),
            excluded_paths: Vec::new(),
        };
    }

    let mut output = String::new();
    let mut block = String::new();
    let mut block_excluded = false;
    let mut excluded_paths = BTreeSet::new();
    let mut saw_git_block = false;

    for raw_line in diff.split_inclusive('\n') {
        let line = raw_line.trim_end_matches('\n').trim_end_matches('\r');
        if line.starts_with("diff --git ") {
            if !block.is_empty() && !block_excluded {
                output.push_str(&block);
            }
            block.clear();
            saw_git_block = true;
            block_excluded = rd_diff_line_excluded_paths(line)
                .into_iter()
                .inspect(|path| {
                    excluded_paths.insert(path.clone());
                })
                .next()
                .is_some();
        } else if saw_git_block && (line.starts_with("--- ") || line.starts_with("+++ ")) {
            let header_excluded = rd_diff_line_excluded_paths(line);
            if !header_excluded.is_empty() {
                block_excluded = true;
                excluded_paths.extend(header_excluded);
            }
        }

        if saw_git_block {
            block.push_str(raw_line);
        } else {
            output.push_str(raw_line);
        }
    }

    if saw_git_block {
        if !block.is_empty() && !block_excluded {
            output.push_str(&block);
        }
    } else {
        let header_excluded = diff
            .lines()
            .filter(|line| line.starts_with("--- ") || line.starts_with("+++ "))
            .flat_map(rd_diff_line_excluded_paths)
            .collect::<BTreeSet<_>>();
        if header_excluded.is_empty() {
            output = diff.to_string();
        } else {
            excluded_paths.extend(header_excluded);
            output.clear();
        }
    }

    RdDiffFilterResult {
        diff: output,
        excluded_paths: excluded_paths.into_iter().collect(),
    }
}

pub fn rd_file_change_is_applyable(change_type: &str, file_path: &str, diff_patch: &str) -> bool {
    rd_change_type_is_applyable(change_type)
        && !rd_diff_path_should_be_excluded(file_path)
        && !rd_unified_diff_has_excluded_paths(diff_patch)
}

pub fn normalize_repo_relative_path(raw: &str) -> String {
    let trimmed = raw.trim().replace('\\', "/");
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = PathBuf::new();
    for component in Path::new(&trimmed).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return String::new()
            }
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

pub fn rd_baseline_should_skip_path(path: &str) -> bool {
    let normalized = path.trim().trim_start_matches("./").replace('\\', "/");
    if normalized.is_empty()
        || normalized == ".git"
        || normalized.starts_with(".git/")
        || normalized == ".aos"
        || normalized.starts_with(".aos/")
        || normalized == ".aos-rd-candidates"
        || normalized.starts_with(".aos-rd-candidates/")
    {
        return true;
    }
    let rel_path = Path::new(&normalized);
    should_skip_path(rel_path)
        || rel_path.components().any(|component| {
            component.as_os_str().to_str().is_some_and(|part| {
                matches!(
                    part,
                    "node_modules" | "target" | "dist" | "build" | ".next" | ".turbo" | ".vite"
                )
            })
        })
}

fn rd_unified_diff_has_excluded_paths(diff: &str) -> bool {
    !filter_rd_unified_diff_excluded_paths(diff)
        .excluded_paths
        .is_empty()
}

fn rd_diff_line_excluded_paths(line: &str) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(rest) = line.strip_prefix("diff --git ") {
        for part in rest.split_whitespace().take(2) {
            let path = rd_normalize_diff_path(part);
            if rd_diff_path_should_be_excluded(&path) {
                paths.push(path);
            }
        }
    } else if let Some(rest) = line
        .strip_prefix("--- ")
        .or_else(|| line.strip_prefix("+++ "))
    {
        let path = rd_normalize_diff_path(rest);
        if rd_diff_path_should_be_excluded(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn rd_normalize_diff_path(path: &str) -> String {
    let trimmed = path
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches("a/")
        .trim_start_matches("b/");
    if trimmed == "/dev/null" {
        String::new()
    } else {
        normalize_repo_relative_path(trimmed)
    }
}

fn rd_diff_path_should_be_excluded(path: &str) -> bool {
    let normalized = rd_normalize_diff_path(path);
    normalized.is_empty() || rd_baseline_should_skip_path(&normalized)
}

fn rd_change_type_is_applyable(change_type: &str) -> bool {
    matches!(
        change_type,
        "modify" | "repair_diff" | "auto_repair" | "partial_remaining"
    )
}
