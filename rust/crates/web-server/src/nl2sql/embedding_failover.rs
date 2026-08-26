use super::{resolve_embedding_profiles, EmbeddingProfileKind, EmbeddingTenantConfig};
use crate::nl2sql::embedding::EmbeddingModel;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::time::Duration;

const DEFAULT_FALLBACK_COOLDOWN_SECS: i64 = 30;
const DEFAULT_NOTIFICATION_INTERVAL_SECS: i64 = 1_800;

#[derive(Debug)]
pub struct EmbeddingBatchOutcome {
    pub config: EmbeddingTenantConfig,
    pub vectors: Vec<Vec<f32>>,
    pub usage: Option<api::Usage>,
    pub fallback_error: Option<String>,
}

fn fallback_cooldown_secs() -> i64 {
    std::env::var("AOS_EMBEDDING_FALLBACK_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_FALLBACK_COOLDOWN_SECS)
        .clamp(5, 3_600)
}

fn notification_interval_secs() -> i64 {
    std::env::var("AOS_EMBEDDING_ALERT_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_NOTIFICATION_INTERVAL_SECS)
        .clamp(60, 86_400)
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn alert_id(tenant_id: &str, profile_id: &str, scenario: &str) -> String {
    let input = format!("{tenant_id}\0{profile_id}\0{scenario}");
    format!(
        "embedding-alert-{}",
        hex::encode(Sha256::digest(input.as_bytes()))
    )
}

async fn external_circuit_allows_request(
    db: &SqlitePool,
    tenant_id: &str,
    profile_id: &str,
    scenario: &str,
) -> bool {
    let modifier = format!("-{} seconds", fallback_cooldown_secs());
    sqlx::query_scalar::<_, i64>(
        "SELECT CASE WHEN NOT EXISTS (
             SELECT 1 FROM embedding_provider_alerts
             WHERE tenant_id = ? AND profile_id = ? AND scenario = ?
               AND status = 'active' AND last_failed_at > datetime(CURRENT_TIMESTAMP, ?)
         ) THEN 1 ELSE 0 END",
    )
    .bind(tenant_id)
    .bind(profile_id)
    .bind(scenario)
    .bind(modifier)
    .fetch_one(db)
    .await
    .map_or(true, |allowed| allowed == 1)
}

pub async fn record_embedding_fallback_alert(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: &str,
    config: &EmbeddingTenantConfig,
    error: &str,
) -> anyhow::Result<()> {
    if config.profile_kind != EmbeddingProfileKind::Api {
        return Ok(());
    }
    record_embedding_fallback_alert_for_profile(
        db,
        tenant_id,
        scenario,
        &config.profile_id(tenant_id),
        &config.provider,
        &config.model,
        error,
    )
    .await
}

pub async fn record_embedding_fallback_alert_for_profile(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: &str,
    profile_id: &str,
    provider: &str,
    model: &str,
    error: &str,
) -> anyhow::Result<()> {
    let scenario = scenario.trim().to_ascii_lowercase();
    let id = alert_id(tenant_id, profile_id, &scenario);
    let error = truncate(error, 1_000);
    let modifier = format!("-{} seconds", notification_interval_secs());
    let row = sqlx::query(
        "INSERT INTO embedding_provider_alerts
           (id, tenant_id, profile_id, scenario, provider, model, last_error)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(tenant_id, profile_id, scenario) DO UPDATE SET
           provider = excluded.provider,
           model = excluded.model,
           status = 'active',
           failure_count = embedding_provider_alerts.failure_count + 1,
           notification_version = CASE
             WHEN embedding_provider_alerts.status = 'resolved'
               OR embedding_provider_alerts.last_notified_at <= datetime(CURRENT_TIMESTAMP, ?)
             THEN embedding_provider_alerts.notification_version + 1
             ELSE embedding_provider_alerts.notification_version
           END,
           last_failed_at = CURRENT_TIMESTAMP,
           last_error = excluded.last_error,
           last_notified_at = CASE
             WHEN embedding_provider_alerts.status = 'resolved'
               OR embedding_provider_alerts.last_notified_at <= datetime(CURRENT_TIMESTAMP, ?)
             THEN CURRENT_TIMESTAMP ELSE embedding_provider_alerts.last_notified_at
           END,
           resolved_at = NULL,
           updated_at = CURRENT_TIMESTAMP
         RETURNING id, notification_version",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(profile_id)
    .bind(&scenario)
    .bind(provider)
    .bind(model)
    .bind(&error)
    .bind(&modifier)
    .bind(&modifier)
    .fetch_one(db)
    .await?;
    let persisted_id: String = row.get("id");
    let notification_version: i64 = row.get("notification_version");
    let notification_id = format!("{persisted_id}:{notification_version}");
    let body = format!(
        "场景 {scenario} 的外部 Embedding（{}/{}）不可用，AOS 已自动降级到内置本地模型。最近错误：{}",
        provider,
        model,
        truncate(&error, 320),
    );
    sqlx::query(
        "INSERT OR IGNORE INTO notifications
           (id, tenant_id, user_id, title, body, level)
         VALUES (?, ?, NULL, 'Embedding 已降级到本地模型', ?, 'warning')",
    )
    .bind(notification_id)
    .bind(tenant_id)
    .bind(body)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn resolve_embedding_fallback_alert(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: &str,
    config: &EmbeddingTenantConfig,
) -> anyhow::Result<()> {
    if config.profile_kind != EmbeddingProfileKind::Api {
        return Ok(());
    }
    resolve_embedding_fallback_alert_for_profile(
        db,
        tenant_id,
        scenario,
        &config.profile_id(tenant_id),
    )
    .await
}

pub async fn resolve_embedding_fallback_alert_for_profile(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: &str,
    profile_id: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE embedding_provider_alerts
         SET status = 'resolved', resolved_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND profile_id = ? AND scenario = ? AND status = 'active'",
    )
    .bind(tenant_id)
    .bind(profile_id)
    .bind(scenario.trim().to_ascii_lowercase())
    .execute(db)
    .await?;
    Ok(())
}

pub(crate) async fn embed_batch_for_config(
    config: &EmbeddingTenantConfig,
    texts: &[String],
    background: bool,
    request_timeout: Option<Duration>,
) -> anyhow::Result<(Vec<Vec<f32>>, Option<api::Usage>)> {
    let model = EmbeddingModel::new_with_dimensions(
        &config.model,
        config.base_url.clone(),
        (config.profile_kind == EmbeddingProfileKind::Api).then(|| config.api_key.clone()),
        config.dimensions,
    );
    let request = async {
        if background {
            model.embed_batch_with_usage_background(texts).await
        } else {
            model.embed_batch_with_usage(texts).await
        }
    };
    if let Some(timeout) = request_timeout {
        tokio::time::timeout(timeout, request).await.map_err(|_| {
            anyhow::anyhow!(
                "embedding request timed out after {}ms",
                timeout.as_millis()
            )
        })?
    } else {
        request.await
    }
}

pub async fn embed_batch_with_failover(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: &str,
    texts: &[String],
    background: bool,
    request_timeout: Option<Duration>,
) -> anyhow::Result<EmbeddingBatchOutcome> {
    let profiles = resolve_embedding_profiles(db, tenant_id, Some(scenario)).await;
    if let Some(api) = profiles.api {
        let profile_id = api.profile_id(tenant_id);
        let api_failure = if external_circuit_allows_request(db, tenant_id, &profile_id, scenario)
            .await
        {
            match embed_batch_for_config(&api, texts, background, request_timeout).await {
                Ok((vectors, usage)) => {
                    if let Err(error) =
                        resolve_embedding_fallback_alert(db, tenant_id, scenario, &api).await
                    {
                        tracing::warn!(tenant_id, scenario, error = %error, "failed to resolve embedding provider alert");
                    }
                    return Ok(EmbeddingBatchOutcome {
                        config: api,
                        vectors,
                        usage,
                        fallback_error: None,
                    });
                }
                Err(error) => error.to_string(),
            }
        } else {
            "external embedding circuit is cooling down after a recent failure".to_string()
        };

        let (vectors, usage) =
            embed_batch_for_config(&profiles.local, texts, background, request_timeout)
                .await
                .map_err(|local_error| {
                    anyhow::anyhow!(
                "external embedding failed ({}) and bundled local fallback failed ({local_error})",
                api_failure
            )
                })?;
        if let Err(alert_error) =
            record_embedding_fallback_alert(db, tenant_id, scenario, &api, &api_failure).await
        {
            tracing::warn!(tenant_id, scenario, error = %alert_error, "failed to persist embedding fallback alert");
        }
        return Ok(EmbeddingBatchOutcome {
            config: profiles.local,
            vectors,
            usage,
            fallback_error: Some(api_failure),
        });
    }

    let (vectors, usage) =
        embed_batch_for_config(&profiles.local, texts, background, request_timeout).await?;
    Ok(EmbeddingBatchOutcome {
        config: profiles.local,
        vectors,
        usage,
        fallback_error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fallback_alerts_are_deduplicated_and_reopen_after_recovery() {
        let db = crate::test_sqlite_pool().await;
        sqlx::query("INSERT INTO tenants (id, name, slug, plan) VALUES ('tenant', 'Tenant', 'tenant', 'free')")
            .execute(&db)
            .await
            .unwrap();
        let config = EmbeddingTenantConfig {
            api_key: "secret-not-persisted".to_string(),
            model: "test-embedding".to_string(),
            base_url: Some("https://example.invalid/v1".to_string()),
            dimensions: Some(3),
            configured_via: "test",
            key_id: Some("key-id".to_string()),
            provider: "openai".to_string(),
            profile_kind: EmbeddingProfileKind::Api,
            model_version: "test-v1".to_string(),
            vector_signature: "test-vector-space".to_string(),
        };

        record_embedding_fallback_alert(&db, "tenant", "chat", &config, "network error")
            .await
            .unwrap();
        record_embedding_fallback_alert(&db, "tenant", "chat", &config, "network error again")
            .await
            .unwrap();
        let row: (i64, i64) = sqlx::query_as(
            "SELECT failure_count, notification_version FROM embedding_provider_alerts",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row, (2, 1));
        let notifications: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(notifications, 1);

        resolve_embedding_fallback_alert(&db, "tenant", "chat", &config)
            .await
            .unwrap();
        record_embedding_fallback_alert(&db, "tenant", "chat", &config, "new incident")
            .await
            .unwrap();
        let notifications: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(notifications, 2);
        let status: String = sqlx::query_scalar("SELECT status FROM embedding_provider_alerts")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(status, "active");
    }
}
