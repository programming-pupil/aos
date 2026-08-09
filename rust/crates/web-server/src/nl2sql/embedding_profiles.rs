use super::{
    resolve_embedding_profiles, EmbeddingProfileKind, EmbeddingProfiles, EmbeddingTenantConfig,
};
use sqlx::{Row, SqlitePool};

#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub id: String,
    pub config: EmbeddingTenantConfig,
}

impl ResolvedProfile {
    fn new(tenant_id: &str, config: EmbeddingTenantConfig) -> Self {
        let id = config.profile_id(tenant_id);
        Self { id, config }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedProfiles {
    pub api: Option<ResolvedProfile>,
    pub local: ResolvedProfile,
}

pub async fn resolve_profiles(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: Option<&str>,
) -> anyhow::Result<ResolvedProfiles> {
    let EmbeddingProfiles { api, local } =
        resolve_embedding_profiles(db, tenant_id, scenario).await;
    let profiles = ResolvedProfiles {
        api: api.map(|config| ResolvedProfile::new(tenant_id, config)),
        local: ResolvedProfile::new(tenant_id, local),
    };
    register_profile(db, tenant_id, &profiles.local).await?;
    if let Some(api) = &profiles.api {
        register_profile(db, tenant_id, api).await?;
    }
    Ok(profiles)
}

async fn register_profile(
    db: &SqlitePool,
    tenant_id: &str,
    profile: &ResolvedProfile,
) -> anyhow::Result<()> {
    let cfg = &profile.config;
    sqlx::query(
        "INSERT INTO nl2sql_embedding_profiles
         (id, tenant_id, profile_kind, provider, base_url, model, dimensions,
          model_version, vector_signature, configured_via)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           configured_via = excluded.configured_via,
           updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&profile.id)
    .bind(tenant_id)
    .bind(cfg.profile_kind.as_str())
    .bind(&cfg.provider)
    .bind(cfg.normalized_base_url())
    .bind(&cfg.model)
    .bind(i64::try_from(cfg.effective_dimensions()).unwrap_or(i64::MAX))
    .bind(&cfg.model_version)
    .bind(&cfg.vector_signature)
    .bind(cfg.configured_via)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn reconcile_tenant_profiles(
    db: &SqlitePool,
    tenant_id: &str,
) -> anyhow::Result<ResolvedProfiles> {
    let profiles = resolve_profiles(db, tenant_id, Some("nl2sql")).await?;
    let datasource_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM data_sources WHERE tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    for datasource_id in datasource_ids {
        reconcile_datasource_kind(db, tenant_id, &datasource_id, &profiles.local).await?;
        if let Some(api) = &profiles.api {
            reconcile_datasource_kind(db, tenant_id, &datasource_id, api).await?;
        } else {
            sqlx::query(
                "INSERT INTO nl2sql_datasource_embedding_profiles
                 (tenant_id, datasource_id, profile_kind, status)
                 VALUES (?, ?, 'api', 'disabled')
                 ON CONFLICT(tenant_id, datasource_id, profile_kind) DO UPDATE SET
                   desired_profile_id = NULL,
                   status = 'disabled',
                   last_error = NULL,
                   updated_at = CURRENT_TIMESTAMP",
            )
            .bind(tenant_id)
            .bind(&datasource_id)
            .execute(db)
            .await?;
        }
    }
    Ok(profiles)
}

pub async fn ensure_datasource_profiles(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    profiles: &ResolvedProfiles,
) -> anyhow::Result<()> {
    reconcile_datasource_kind(db, tenant_id, datasource_id, &profiles.local).await?;
    if let Some(api) = &profiles.api {
        reconcile_datasource_kind(db, tenant_id, datasource_id, api).await?;
    }
    Ok(())
}

async fn reconcile_datasource_kind(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    profile: &ResolvedProfile,
) -> anyhow::Result<()> {
    let kind = profile.config.profile_kind.as_str();
    sqlx::query(
        "INSERT INTO nl2sql_datasource_embedding_profiles
         (tenant_id, datasource_id, profile_kind, desired_profile_id, status)
         VALUES (?, ?, ?, ?, 'pending')
         ON CONFLICT(tenant_id, datasource_id, profile_kind) DO UPDATE SET
           desired_profile_id = excluded.desired_profile_id,
           status = CASE
             WHEN active_profile_id = excluded.desired_profile_id THEN 'ready'
             WHEN status = 'building' AND desired_profile_id = excluded.desired_profile_id THEN status
             ELSE 'pending'
           END,
           updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(kind)
    .bind(&profile.id)
    .execute(db)
    .await?;

    let active: Option<String> = sqlx::query_scalar(
        "SELECT active_profile_id FROM nl2sql_datasource_embedding_profiles
         WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = ?",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(kind)
    .fetch_optional(db)
    .await?
    .flatten();
    if active.as_deref() != Some(profile.id.as_str()) {
        enqueue_reindex(db, tenant_id, datasource_id, profile).await?;
    }
    Ok(())
}

pub async fn enqueue_reindex(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    profile: &ResolvedProfile,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO nl2sql_embedding_reindex_jobs
         (id, tenant_id, datasource_id, profile_kind, profile_id, status)
         VALUES (?, ?, ?, ?, ?, 'pending')",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(profile.config.profile_kind.as_str())
    .bind(&profile.id)
    .execute(db)
    .await?;
    sqlx::query(
        "UPDATE nl2sql_datasource_embedding_profiles SET \
           status = CASE WHEN active_profile_id = ? THEN 'degraded' ELSE status END, \
           updated_at = CURRENT_TIMESTAMP \
         WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = ? \
           AND desired_profile_id = ?",
    )
    .bind(&profile.id)
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(profile.config.profile_kind.as_str())
    .bind(&profile.id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn active_profile_ready_for_datasources(
    db: &SqlitePool,
    tenant_id: &str,
    profile: &ResolvedProfile,
    datasource_ids: &[String],
) -> anyhow::Result<bool> {
    if datasource_ids.is_empty() {
        return Ok(true);
    }
    let placeholders = std::iter::repeat_n("?", datasource_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT COUNT(*) FROM nl2sql_datasource_embedding_profiles
         WHERE tenant_id = ? AND profile_kind = ? AND active_profile_id = ?
           AND status = 'ready' AND datasource_id IN ({placeholders})"
    );
    let mut query = sqlx::query_scalar::<_, i64>(&query)
        .bind(tenant_id)
        .bind(profile.config.profile_kind.as_str())
        .bind(&profile.id);
    for datasource_id in datasource_ids {
        query = query.bind(datasource_id);
    }
    let count = query.fetch_one(db).await?;
    Ok(count == i64::try_from(datasource_ids.len()).unwrap_or(i64::MAX))
}

pub async fn circuit_allows_request(db: &SqlitePool, profile_id: &str) -> anyhow::Result<bool> {
    let allowed: i64 = sqlx::query_scalar(
        "SELECT CASE
           WHEN circuit_open_until IS NULL OR circuit_open_until <= CURRENT_TIMESTAMP THEN 1
           ELSE 0
         END
         FROM nl2sql_embedding_profiles WHERE id = ?",
    )
    .bind(profile_id)
    .fetch_one(db)
    .await?;
    Ok(allowed == 1)
}

pub async fn record_profile_success(db: &SqlitePool, profile_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE nl2sql_embedding_profiles SET
           health_status = 'healthy', consecutive_failures = 0,
           circuit_open_until = NULL, last_success_at = CURRENT_TIMESTAMP,
           last_error = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(profile_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn record_profile_failure(
    db: &SqlitePool,
    profile_id: &str,
    error: &str,
) -> anyhow::Result<()> {
    let threshold = std::env::var("NL2SQL_EMBEDDING_CIRCUIT_FAILURE_THRESHOLD")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(2)
        .max(1);
    let cooldown_secs = std::env::var("NL2SQL_EMBEDDING_CIRCUIT_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(30)
        .max(5);
    let modifier = format!("+{cooldown_secs} seconds");
    sqlx::query(
        "UPDATE nl2sql_embedding_profiles SET
           consecutive_failures = consecutive_failures + 1,
           health_status = 'degraded',
           circuit_open_until = CASE
             WHEN consecutive_failures + 1 >= ? THEN datetime(CURRENT_TIMESTAMP, ?)
             ELSE circuit_open_until
           END,
           last_failure_at = CURRENT_TIMESTAMP,
           last_error = ?, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(threshold)
    .bind(modifier)
    .bind(truncate_error(error))
    .bind(profile_id)
    .execute(db)
    .await?;
    Ok(())
}

fn truncate_error(error: &str) -> String {
    error.chars().take(1_000).collect()
}

pub async fn activate_profile(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    profile: &ResolvedProfile,
    indexed_items: usize,
    total_items: usize,
) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "UPDATE nl2sql_datasource_embedding_profiles SET
           active_profile_id = desired_profile_id,
           status = 'ready', indexed_items = ?, total_items = ?,
           last_error = NULL, activated_at = CURRENT_TIMESTAMP,
           updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = ?
           AND desired_profile_id = ?",
    )
    .bind(i64::try_from(indexed_items).unwrap_or(i64::MAX))
    .bind(i64::try_from(total_items).unwrap_or(i64::MAX))
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(profile.config.profile_kind.as_str())
    .bind(&profile.id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn mark_profile_building(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    kind: EmbeddingProfileKind,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE nl2sql_datasource_embedding_profiles
         SET status = 'building', last_error = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = ?",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(kind.as_str())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn profile_status_rows(
    db: &SqlitePool,
    tenant_id: &str,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let rows = sqlx::query(
        "SELECT d.datasource_id, d.profile_kind, d.active_profile_id,
                d.desired_profile_id, d.status, d.indexed_items, d.total_items,
                d.last_error, p.model, p.provider, p.health_status,
                p.circuit_open_until
         FROM nl2sql_datasource_embedding_profiles d
         LEFT JOIN nl2sql_embedding_profiles p ON p.id = d.desired_profile_id
         WHERE d.tenant_id = ? ORDER BY d.datasource_id, d.profile_kind",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "datasourceId": row.get::<String, _>("datasource_id"),
                "kind": row.get::<String, _>("profile_kind"),
                "activeProfileId": row.get::<Option<String>, _>("active_profile_id"),
                "desiredProfileId": row.get::<Option<String>, _>("desired_profile_id"),
                "status": row.get::<String, _>("status"),
                "indexedItems": row.get::<i64, _>("indexed_items"),
                "totalItems": row.get::<i64, _>("total_items"),
                "lastError": row.get::<Option<String>, _>("last_error"),
                "model": row.get::<Option<String>, _>("model"),
                "provider": row.get::<Option<String>, _>("provider"),
                "healthStatus": row.get::<Option<String>, _>("health_status"),
                "circuitOpenUntil": row.get::<Option<String>, _>("circuit_open_until"),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nl2sql::{
        local_embedding_config_for_runtime, EmbeddingProfileKind, EmbeddingTenantConfig,
    };

    async fn fixture() -> (SqlitePool, String, String) {
        let db = crate::test_sqlite_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let datasource_id = format!("datasource-{}", uuid::Uuid::new_v4());
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES (?, 'Profile test', ?)")
            .bind(&tenant_id)
            .bind(format!("profile-{}", uuid::Uuid::new_v4()))
            .execute(&db)
            .await
            .expect("insert profile test tenant");
        sqlx::query(
            "INSERT INTO data_sources (id, tenant_id, name, db_type, config) \
             VALUES (?, ?, 'Profile datasource', 'sqlite', '{}')",
        )
        .bind(&datasource_id)
        .bind(&tenant_id)
        .execute(&db)
        .await
        .expect("insert profile test datasource");
        (db, tenant_id, datasource_id)
    }

    fn changed_local_profile(tenant_id: &str) -> ResolvedProfile {
        let mut config = local_embedding_config_for_runtime();
        config.model = "local/test-model-v2".to_string();
        config.model_version = "test-v2".to_string();
        config.vector_signature = "sha256:test-v2".to_string();
        ResolvedProfile::new(tenant_id, config)
    }

    fn api_profile(tenant_id: &str) -> ResolvedProfile {
        ResolvedProfile::new(
            tenant_id,
            EmbeddingTenantConfig {
                api_key: "secret".to_string(),
                model: "embedding-test".to_string(),
                base_url: Some("https://embedding.example/v1".to_string()),
                dimensions: Some(3),
                configured_via: "api_key",
                key_id: Some("key-id".to_string()),
                provider: "custom".to_string(),
                profile_kind: EmbeddingProfileKind::Api,
                model_version: "v1".to_string(),
                vector_signature: "sha256:api-test".to_string(),
            },
        )
    }

    #[tokio::test]
    async fn shadow_profile_switch_is_atomic_and_jobs_are_deduplicated() {
        let (db, tenant_id, datasource_id) = fixture().await;
        let old_profile = ResolvedProfile::new(&tenant_id, local_embedding_config_for_runtime());
        register_profile(&db, &tenant_id, &old_profile)
            .await
            .expect("register old profile");
        reconcile_datasource_kind(&db, &tenant_id, &datasource_id, &old_profile)
            .await
            .expect("reconcile old profile");
        enqueue_reindex(&db, &tenant_id, &datasource_id, &old_profile)
            .await
            .expect("duplicate enqueue is harmless");
        let jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nl2sql_embedding_reindex_jobs \
             WHERE tenant_id = ? AND datasource_id = ? AND profile_id = ?",
        )
        .bind(&tenant_id)
        .bind(&datasource_id)
        .bind(&old_profile.id)
        .fetch_one(&db)
        .await
        .expect("count old jobs");
        assert_eq!(jobs, 1);
        assert!(
            activate_profile(&db, &tenant_id, &datasource_id, &old_profile, 4, 4)
                .await
                .expect("activate old profile")
        );

        let new_profile = changed_local_profile(&tenant_id);
        register_profile(&db, &tenant_id, &new_profile)
            .await
            .expect("register new profile");
        reconcile_datasource_kind(&db, &tenant_id, &datasource_id, &new_profile)
            .await
            .expect("reconcile new profile");
        let (active, desired, status): (Option<String>, Option<String>, String) = sqlx::query_as(
            "SELECT active_profile_id, desired_profile_id, status \
                 FROM nl2sql_datasource_embedding_profiles \
                 WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = 'local'",
        )
        .bind(&tenant_id)
        .bind(&datasource_id)
        .fetch_one(&db)
        .await
        .expect("read shadow profile state");
        assert_eq!(active.as_deref(), Some(old_profile.id.as_str()));
        assert_eq!(desired.as_deref(), Some(new_profile.id.as_str()));
        assert_eq!(status, "pending");
        assert!(
            !activate_profile(&db, &tenant_id, &datasource_id, &old_profile, 4, 4)
                .await
                .expect("stale activation is rejected")
        );
        assert!(
            activate_profile(&db, &tenant_id, &datasource_id, &new_profile, 5, 5)
                .await
                .expect("activate new profile")
        );
        let active: Option<String> = sqlx::query_scalar(
            "SELECT active_profile_id FROM nl2sql_datasource_embedding_profiles \
             WHERE tenant_id = ? AND datasource_id = ? AND profile_kind = 'local'",
        )
        .bind(&tenant_id)
        .bind(&datasource_id)
        .fetch_one(&db)
        .await
        .expect("read active profile after switch");
        assert_eq!(active.as_deref(), Some(new_profile.id.as_str()));
        db.close().await;
    }

    #[tokio::test]
    async fn circuit_failure_and_success_control_api_eligibility() {
        let (db, tenant_id, _) = fixture().await;
        let profile = api_profile(&tenant_id);
        register_profile(&db, &tenant_id, &profile)
            .await
            .expect("register API profile");
        record_profile_failure(&db, &profile.id, "rate limited")
            .await
            .expect("record API failure");
        let (health, failures): (String, i64) = sqlx::query_as(
            "SELECT health_status, consecutive_failures FROM nl2sql_embedding_profiles WHERE id = ?",
        )
        .bind(&profile.id)
        .fetch_one(&db)
        .await
        .expect("read failed profile");
        assert_eq!(health, "degraded");
        assert_eq!(failures, 1);

        sqlx::query(
            "UPDATE nl2sql_embedding_profiles SET circuit_open_until = datetime(CURRENT_TIMESTAMP, '+5 minutes') WHERE id = ?",
        )
        .bind(&profile.id)
        .execute(&db)
        .await
        .expect("open test circuit");
        assert!(!circuit_allows_request(&db, &profile.id)
            .await
            .expect("open circuit blocks requests"));
        record_profile_success(&db, &profile.id)
            .await
            .expect("record recovery");
        assert!(circuit_allows_request(&db, &profile.id)
            .await
            .expect("recovered circuit allows requests"));
        let (health, failures, open_until): (String, i64, Option<String>) = sqlx::query_as(
            "SELECT health_status, consecutive_failures, circuit_open_until \
             FROM nl2sql_embedding_profiles WHERE id = ?",
        )
        .bind(&profile.id)
        .fetch_one(&db)
        .await
        .expect("read recovered profile");
        assert_eq!(health, "healthy");
        assert_eq!(failures, 0);
        assert!(open_until.is_none());
        db.close().await;
    }
}
