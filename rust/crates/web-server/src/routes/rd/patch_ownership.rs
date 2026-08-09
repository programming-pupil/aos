//! Diff output guardrails and patch ownership bookkeeping.

use sha2::{Digest, Sha256};

use super::*;

pub(super) async fn enforce_rd_diff_output_policy(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    parsed: &mut ParsedRdOutput,
) -> Result<(), AppError> {
    if mode_allows_rd_diff(mode) {
        return Ok(());
    }
    let diff_bytes = parsed
        .unified_diff
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::len)
        .unwrap_or(0);
    let touched_files = std::mem::take(&mut parsed.touched_files);
    let had_diff = diff_bytes > 0;
    parsed.unified_diff = None;
    if had_diff || !touched_files.is_empty() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "diff_guard",
            "skipped",
            "非代码修改任务输出了 Diff/变更文件，已丢弃以避免污染本次任务审批",
            json!({
                "mode": mode,
                "diffBytes": diff_bytes,
                "discardedTouchedFiles": touched_files,
            }),
        )
        .await?;
        record_quality_metric(
            &state.db,
            &claims.tenant_id,
            None,
            Some(task_id),
            "diff_guard_discarded_non_modify",
            1.0,
            json!({
                "mode": mode,
                "diffBytes": diff_bytes,
            }),
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn record_rd_patch_ownerships(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: Option<&str>,
    task_id: &str,
    change_id: &str,
    file_paths: &[String],
    patch: &str,
) -> Result<(), AppError> {
    let Some(repository_id) = repository_id else {
        return Ok(());
    };
    let patch_hash = sha256_hex(patch);
    let mut paths = file_paths
        .iter()
        .map(|path| path.trim().replace('\\', "/"))
        .filter(|path| !path.is_empty())
        .collect::<BTreeSet<_>>();
    if paths.is_empty() {
        if let Some(path) = infer_first_file_from_diff(patch) {
            paths.insert(path);
        }
    }
    for file_path in paths {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO rd_patch_ownerships \
             (id, tenant_id, repository_id, task_id, change_id, file_path, patch_hash, applied) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0) \
             ON CONFLICT DO UPDATE SET patch_hash = excluded.patch_hash, applied = 0, applied_at = NULL",
        )
        .bind(&id)
        .bind(tenant_id)
        .bind(repository_id)
        .bind(task_id)
        .bind(change_id)
        .bind(file_path)
        .bind(&patch_hash)
        .execute(db)
        .await?;
    }
    Ok(())
}

pub(super) async fn mark_rd_patch_ownership_applied(
    db: &SqlitePool,
    tenant_id: &str,
    change_id: &str,
    applied: bool,
) -> Result<(), AppError> {
    if applied {
        sqlx::query(
            "UPDATE rd_patch_ownerships SET applied = 1, applied_at = CURRENT_TIMESTAMP WHERE change_id = ? AND tenant_id = ?",
        )
        .bind(change_id)
        .bind(tenant_id)
        .execute(db)
        .await?;
    } else {
        sqlx::query(
            "UPDATE rd_patch_ownerships SET applied = 0, applied_at = NULL WHERE change_id = ? AND tenant_id = ?",
        )
        .bind(change_id)
        .bind(tenant_id)
        .execute(db)
        .await?;
    }
    Ok(())
}

fn mode_allows_rd_diff(mode: &str) -> bool {
    mode == "modify"
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
