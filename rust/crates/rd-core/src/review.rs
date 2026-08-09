//! Pure review quality scoring for RD tasks.

use std::collections::BTreeSet;
use std::path::Path;

use crate::diff::infer_files_from_unified_diff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RdReviewQuality {
    pub findings_count: u64,
    pub file_ref_count: u64,
    pub line_ref_count: u64,
}

pub fn analyze_review_quality(
    review_text: &str,
    diff: &str,
    primary_touched_files: &[String],
    reviewer_touched_files: &[String],
) -> RdReviewQuality {
    let mut candidate_files = BTreeSet::new();
    for path in infer_files_from_unified_diff(diff)
        .into_iter()
        .chain(primary_touched_files.iter().cloned())
        .chain(reviewer_touched_files.iter().cloned())
    {
        let trimmed = path
            .trim()
            .trim_start_matches("a/")
            .trim_start_matches("b/");
        if !trimmed.is_empty() && trimmed != "/dev/null" {
            candidate_files.insert(trimmed.to_string());
        }
    }

    let text_lower = review_text.to_ascii_lowercase();
    let file_ref_count = candidate_files
        .iter()
        .filter(|path| {
            text_lower.contains(&path.to_ascii_lowercase())
                || Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| text_lower.contains(&name.to_ascii_lowercase()))
        })
        .count() as u64;

    let line_ref_count = review_text
        .lines()
        .filter(|line| contains_line_reference(line))
        .count() as u64;
    let findings_count = count_review_findings(review_text);

    RdReviewQuality {
        findings_count,
        file_ref_count,
        line_ref_count,
    }
}

fn count_review_findings(review_text: &str) -> u64 {
    let lower = review_text.to_ascii_lowercase();
    if lower.contains("没有明显问题")
        || lower.contains("未发现明显问题")
        || lower.contains("no obvious issue")
        || lower.contains("no findings")
        || lower.contains("looks good")
    {
        return 0;
    }

    let mut count = 0u64;
    for line in review_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower_line = trimmed.to_ascii_lowercase();
        let looks_like_item = trimmed.starts_with('-')
            || trimmed.starts_with('*')
            || trimmed.chars().next().is_some_and(|ch| ch.is_ascii_digit());
        let has_signal = [
            "blocker",
            "critical",
            "high",
            "medium",
            "low",
            "error",
            "warning",
            "bug",
            "risk",
            "regression",
            "test",
            "严重",
            "高",
            "中",
            "低",
            "风险",
            "缺陷",
            "回归",
            "测试",
            "建议",
        ]
        .iter()
        .any(|needle| lower_line.contains(needle));
        if looks_like_item && has_signal {
            count += 1;
        }
    }

    if count == 0
        && [
            "blocker",
            "critical",
            "risk",
            "regression",
            "严重",
            "风险",
            "缺陷",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        1
    } else {
        count
    }
}

fn contains_line_reference(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("第") && lower.contains("行") && lower.chars().any(|ch| ch.is_ascii_digit())
    {
        return true;
    }
    if lower.split("line").skip(1).any(|tail| {
        tail.trim_start()
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
    }) {
        return true;
    }
    if lower
        .split("#l")
        .skip(1)
        .any(|tail| tail.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
    {
        return true;
    }
    let mut chars = lower.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == ':' && chars.peek().is_some_and(|next| next.is_ascii_digit()) {
            return true;
        }
    }
    false
}
