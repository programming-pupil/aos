//! Diff path filtering and applyability guards for RD-generated patches.

use super::*;

pub(super) use rd_core::diff::{
    filter_rd_unified_diff_excluded_paths, infer_files_from_unified_diff,
    rd_file_change_is_applyable,
};

pub(super) fn sanitize_rd_parsed_diff_output(parsed: &mut ParsedRdOutput) {
    let Some(diff) = parsed.unified_diff.take() else {
        return;
    };
    let filtered = filter_rd_unified_diff_excluded_paths(&diff);
    parsed.unified_diff = (!filtered.diff.trim().is_empty()).then_some(filtered.diff);
    if !filtered.excluded_paths.is_empty() {
        let excluded = filtered.excluded_paths.into_iter().collect::<BTreeSet<_>>();
        parsed.touched_files.retain(|path| !excluded.contains(path));
    }
}
