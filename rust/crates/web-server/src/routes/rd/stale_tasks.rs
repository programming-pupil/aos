use serde_json::json;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::error::AppError;

use super::record_event;

const RD_STALE_TASK_GRACE_SECS: u64 = 300;
const RD_STALE_TASK_RECONCILE_LIMIT: i64 = 50;

pub(super) async fn reconcile_stale_rd_running_tasks(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: Option<&str>,
    task_id: Option<&str>,
    timeout_secs: u64,
) -> Result<(), AppError> {
    let stale_after_secs = timeout_secs.saturating_add(RD_STALE_TASK_GRACE_SECS);
    let stale_ids = find_stale_rd_running_task_ids(
        db,
        tenant_id,
        user_id,
        task_id,
        stale_after_secs,
        RD_STALE_TASK_RECONCILE_LIMIT,
    )
    .await?;
    if stale_ids.is_empty() {
        return Ok(());
    }

    let error_message = format!(
        "研发任务运行状态超过 {stale_after_secs}s 未更新，AOS 已判定为 runtime 中断或超时。常见原因：服务重启、进程被杀、runtime 事件流未完整落库，或上游模型/工具调用长时间未返回。"
    );
    for task_id in stale_ids {
        let result = sqlx::query(
            "UPDATE rd_tasks \
             SET status = 'failed', error_message = ?, completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ? AND tenant_id = ? AND status IN ('queued', 'running')",
        )
        .bind(&error_message)
        .bind(&task_id)
        .bind(tenant_id)
        .execute(db)
        .await?;
        if result.rows_affected() == 0 {
            continue;
        }
        record_event(
            db,
            tenant_id,
            &task_id,
            "stale_runtime_timeout",
            "failed",
            "任务长时间未更新，已自动标记为失败",
            json!({
                "timeoutSecs": timeout_secs,
                "graceSecs": RD_STALE_TASK_GRACE_SECS,
                "staleAfterSecs": stale_after_secs,
                "reason": "running_task_stale",
                "effect": "task_marked_failed",
            }),
        )
        .await?;
    }
    Ok(())
}

async fn find_stale_rd_running_task_ids(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: Option<&str>,
    task_id: Option<&str>,
    stale_after_secs: u64,
    limit: i64,
) -> Result<Vec<String>, AppError> {
    let mut builder = QueryBuilder::<Sqlite>::new("SELECT id FROM rd_tasks WHERE tenant_id = ");
    builder
        .push_bind(tenant_id)
        .push(" AND status IN ('queued', 'running') AND updated_at < datetime(CURRENT_TIMESTAMP, printf('-%d seconds', ")
        .push_bind(i64::try_from(stale_after_secs).unwrap_or(i64::MAX))
        .push("))");
    if let Some(user_id) = user_id {
        builder.push(" AND user_id = ").push_bind(user_id);
    }
    if let Some(task_id) = task_id {
        builder.push(" AND id = ").push_bind(task_id);
    }
    builder
        .push(" ORDER BY updated_at ASC LIMIT ")
        .push_bind(limit);

    let rows = builder.build().fetch_all(db).await?;
    Ok(rows.iter().map(|row| row.get("id")).collect())
}
