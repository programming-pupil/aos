use serde_json::Value;
use sqlx::SqlitePool;

use crate::error::AppError;

pub(super) async fn record_quality_metric(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: Option<&str>,
    task_id: Option<&str>,
    metric_name: &str,
    metric_value: f64,
    detail_json: Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO rd_quality_metrics \
         (tenant_id, repository_id, task_id, metric_name, metric_value, detail_json) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .bind(task_id)
    .bind(metric_name)
    .bind(metric_value)
    .bind(detail_json)
    .execute(db)
    .await?;
    Ok(())
}
