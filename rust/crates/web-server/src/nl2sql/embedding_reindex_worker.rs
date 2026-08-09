use super::embedding::EmbeddingStoreRegistry;
use super::embedding_profiles::{self, ResolvedProfile};
use sqlx::{Row, SqlitePool};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
struct ReindexJob {
    id: String,
    tenant_id: String,
    datasource_id: String,
    profile_kind: String,
    profile_id: String,
    attempts: i64,
}

pub fn start(db: SqlitePool, registry: Arc<EmbeddingStoreRegistry>) {
    tokio::spawn(async move {
        if let Err(error) = prepare_jobs(&db).await {
            tracing::warn!(error = %error, "failed to prepare embedding reindex jobs");
        }
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match claim_job(&db).await {
                Ok(Some(job)) => {
                    let profile_id = job.profile_id.clone();
                    if let Err(error) = process_job(&db, registry.as_ref(), &job).await {
                        let _ = embedding_profiles::record_profile_failure(
                            &db,
                            &profile_id,
                            &error.to_string(),
                        )
                        .await;
                        if let Err(update_error) = retry_job(&db, &job, &error.to_string()).await {
                            tracing::error!(
                                job_id = %job.id,
                                error = %update_error,
                                "failed to reschedule embedding reindex job"
                            );
                        }
                        tracing::warn!(
                            job_id = %job.id,
                            tenant_id = %job.tenant_id,
                            datasource_id = %job.datasource_id,
                            profile_id = %job.profile_id,
                            error = %error,
                            "embedding shadow-index job failed; retry scheduled"
                        );
                    } else if let Err(error) = complete_job(&db, &job.id).await {
                        tracing::error!(job_id = %job.id, error = %error, "failed to complete embedding reindex job");
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "failed to claim embedding reindex job");
                }
            }
        }
    });
}

async fn prepare_jobs(db: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE nl2sql_embedding_reindex_jobs SET status = 'pending', \
           next_attempt_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE status = 'running'",
    )
    .execute(db)
    .await?;
    let tenant_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM tenants")
        .fetch_all(db)
        .await?;
    for tenant_id in tenant_ids {
        if let Err(error) = embedding_profiles::reconcile_tenant_profiles(db, &tenant_id).await {
            tracing::warn!(tenant_id, error = %error, "failed to reconcile embedding profiles at startup");
        }
    }
    Ok(())
}

async fn claim_job(db: &SqlitePool) -> anyhow::Result<Option<ReindexJob>> {
    let mut tx = db.begin().await?;
    let row = sqlx::query(
        "SELECT id, tenant_id, datasource_id, profile_kind, profile_id, attempts \
         FROM nl2sql_embedding_reindex_jobs \
         WHERE status = 'pending' AND next_attempt_at <= CURRENT_TIMESTAMP \
         ORDER BY created_at LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let job = ReindexJob {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        datasource_id: row.get("datasource_id"),
        profile_kind: row.get("profile_kind"),
        profile_id: row.get("profile_id"),
        attempts: row.get("attempts"),
    };
    let updated = sqlx::query(
        "UPDATE nl2sql_embedding_reindex_jobs SET status = 'running', \
           started_at = CURRENT_TIMESTAMP, attempts = attempts + 1, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND status = 'pending'",
    )
    .bind(&job.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if updated.rows_affected() == 1 {
        Ok(Some(job))
    } else {
        Ok(None)
    }
}

async fn process_job(
    db: &SqlitePool,
    registry: &EmbeddingStoreRegistry,
    job: &ReindexJob,
) -> anyhow::Result<()> {
    let desired_profile_id: Option<String> = sqlx::query_scalar(
        "SELECT desired_profile_id FROM nl2sql_datasource_embedding_profiles \
         WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = ?",
    )
    .bind(&job.tenant_id)
    .bind(&job.datasource_id)
    .bind(&job.profile_kind)
    .fetch_optional(db)
    .await?
    .flatten();
    if desired_profile_id.as_deref() != Some(job.profile_id.as_str()) {
        sqlx::query(
            "UPDATE nl2sql_embedding_reindex_jobs SET status = 'cancelled', \
             completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(&job.id)
        .execute(db)
        .await?;
        return Ok(());
    }

    let profiles = embedding_profiles::resolve_profiles(db, &job.tenant_id, Some("nl2sql")).await?;
    let profile: ResolvedProfile = if profiles.local.id == job.profile_id {
        profiles.local
    } else if profiles.api.as_ref().map(|item| item.id.as_str()) == Some(job.profile_id.as_str()) {
        profiles.api.expect("API profile checked above")
    } else {
        anyhow::bail!("credentials for desired embedding profile are no longer configured");
    };

    embedding_profiles::mark_profile_building(
        db,
        &job.tenant_id,
        &job.datasource_id,
        profile.config.profile_kind,
    )
    .await?;
    let (schema_indexed, schema_total) =
        super::schema_describer::rebuild_profile_from_existing_semantics(
            db,
            registry,
            &job.tenant_id,
            &job.datasource_id,
            &profile,
        )
        .await?;
    if schema_indexed != schema_total {
        anyhow::bail!("schema shadow index is incomplete");
    }
    let (reference_indexed, reference_total) =
        crate::routes::nl2sql::reference::rebuild_reference_profile(
            db,
            &job.tenant_id,
            &job.datasource_id,
            &profile,
        )
        .await?;
    if reference_indexed != reference_total {
        anyhow::bail!("SQL knowledge shadow index is incomplete");
    }

    let total = schema_total.saturating_add(reference_total);
    let activated = embedding_profiles::activate_profile(
        db,
        &job.tenant_id,
        &job.datasource_id,
        &profile,
        total,
        total,
    )
    .await?;
    if !activated {
        anyhow::bail!("desired embedding profile changed before atomic activation");
    }
    embedding_profiles::record_profile_success(db, &profile.id).await?;
    if let Err(error) = registry.persist_ann_snapshots_if_dirty() {
        tracing::warn!(profile_id = %profile.id, error = %error, "failed to persist ANN snapshot after profile activation");
    }
    Ok(())
}

async fn retry_job(db: &SqlitePool, job: &ReindexJob, error: &str) -> anyhow::Result<()> {
    let exponent = u32::try_from(job.attempts.clamp(0, 7)).unwrap_or(7);
    let backoff_secs = 5_i64
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(300);
    let modifier = format!("+{backoff_secs} seconds");
    let last_error: String = error.chars().take(1_000).collect();
    sqlx::query(
        "UPDATE nl2sql_embedding_reindex_jobs SET status = 'pending', \
           next_attempt_at = datetime(CURRENT_TIMESTAMP, ?), last_error = ?, \
           updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(modifier)
    .bind(last_error)
    .bind(&job.id)
    .execute(db)
    .await?;
    sqlx::query(
        "UPDATE nl2sql_datasource_embedding_profiles SET status = 'degraded', \
           last_error = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = ? \
           AND desired_profile_id = ?",
    )
    .bind(error.chars().take(1_000).collect::<String>())
    .bind(&job.tenant_id)
    .bind(&job.datasource_id)
    .bind(&job.profile_kind)
    .bind(&job.profile_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn complete_job(db: &SqlitePool, job_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE nl2sql_embedding_reindex_jobs SET status = 'completed', \
           completed_at = CURRENT_TIMESTAMP, last_error = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND status = 'running'",
    )
    .bind(job_id)
    .execute(db)
    .await?;
    Ok(())
}
