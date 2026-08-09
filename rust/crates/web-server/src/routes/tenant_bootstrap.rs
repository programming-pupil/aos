//! Tenant bootstrap defaults.
//!
//! Ensures newly created tenants have required baseline rows that older
//! migrations only backfilled for existing tenants.

use crate::error::Result;
use pm_domain::budget::{PmBudgetProfile, PmTimeoutBudget};

pub(crate) const BUILTIN_SKILL_REPOSITORIES: [(&str, &str); 4] = [
    ("ComposioHQ/awesome-claude-skills", "master"),
    ("JimLiu/baoyu-skills", "main"),
    ("anthropics/skills", "main"),
    ("cexll/myclaude", "master"),
];

const NL2SQL_DEFAULT_MASK_PATTERNS: [&str; 25] = [
    "%password%",
    "%passwd%",
    "%secret%",
    "%token%",
    "%api_key%",
    "%apikey%",
    "%private_key%",
    "%privatekey%",
    "%credential%",
    "%auth_token%",
    "%access_token%",
    "%refresh_token%",
    "%session_id%",
    "%sessionid%",
    "%ssn%",
    "%social_security%",
    "%credit_card%",
    "%card_number%",
    "%cvv%",
    "%pin%",
    "%bank_account%",
    "%account_number%",
    "%routing_number%",
    "%tax_id%",
    "%ein%",
];

pub(crate) async fn seed_tenant_defaults_with_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    created_by: Option<&str>,
) -> Result<()> {
    seed_pm_budget_profile_with_tx(tx, tenant_id, created_by).await?;
    seed_nl2sql_masking_rules_with_tx(tx, tenant_id, created_by).await?;
    seed_builtin_skill_repositories_with_tx(tx, tenant_id, created_by).await?;
    Ok(())
}

async fn seed_pm_budget_profile_with_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    created_by: Option<&str>,
) -> Result<()> {
    let budget = PmTimeoutBudget::from_profile(PmBudgetProfile::Normal);
    let actor = created_by.unwrap_or("system");
    let constraints_json = serde_json::json!({
        "source": "tenant_seed",
        "seededBy": actor,
        "seededAt": chrono::Utc::now().to_rfc3339(),
    })
    .to_string();

    sqlx::query(
        "INSERT INTO pm_budget_profiles
            (tenant_id, profile_key, display_name, enabled, is_default, priority,
             pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls, max_calls_per_source,
             source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
             preflight_model_timeout_secs, preflight_probe_timeout_secs, preflight_overall_timeout_secs,
             retry_step_budget_secs, retry_total_budget_secs, constraints_json)
         VALUES (?, 'normal', 'Normal', 1, 1, 100,
                 ?, ?, ?, ?,
                 ?, ?, ?,
                 ?, ?, ?,
                 ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            display_name = excluded.display_name,
            enabled = excluded.enabled,
            is_default = excluded.is_default,
            priority = excluded.priority,
            pipeline_timeout_secs = excluded.pipeline_timeout_secs,
            max_attempts = excluded.max_attempts,
            retrieve_max_tool_calls = excluded.retrieve_max_tool_calls,
            max_calls_per_source = excluded.max_calls_per_source,
            source_slot_search_secs = excluded.source_slot_search_secs,
            source_slot_browser_secs = excluded.source_slot_browser_secs,
            source_slot_api_fetch_secs = excluded.source_slot_api_fetch_secs,
            preflight_model_timeout_secs = excluded.preflight_model_timeout_secs,
            preflight_probe_timeout_secs = excluded.preflight_probe_timeout_secs,
            preflight_overall_timeout_secs = excluded.preflight_overall_timeout_secs,
            retry_step_budget_secs = excluded.retry_step_budget_secs,
            retry_total_budget_secs = excluded.retry_total_budget_secs,
            constraints_json = excluded.constraints_json,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(i32::try_from(budget.pipeline_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.max_attempts).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.retrieve_max_tool_calls).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.max_calls_per_source).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.source_slot_search_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.source_slot_browser_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.source_slot_api_fetch_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.preflight_model_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.preflight_probe_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.preflight_overall_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.retry_step_budget_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.retry_total_budget_secs).unwrap_or(i32::MAX))
    .bind(constraints_json)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE pm_budget_profiles
         SET is_default = CASE WHEN profile_key = 'normal' THEN 1 ELSE 0 END
         WHERE tenant_id = ?",
    )
    .bind(tenant_id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

async fn seed_nl2sql_masking_rules_with_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    created_by: Option<&str>,
) -> Result<()> {
    let actor = created_by.unwrap_or("system");

    for pattern in NL2SQL_DEFAULT_MASK_PATTERNS {
        sqlx::query(
            "INSERT INTO nl2sql_column_masking_rules
                (tenant_id, datasource_id, table_name, column_name, mask_type, pattern, constant_value,
                 priority, role_exception_patterns, condition_expression, description, enabled, created_by)
             SELECT
                ?, NULL, '%', ?, 'redact', NULL, NULL,
                20, NULL, NULL, ('Default sensitive column mask: ' || ?), 1, ?
             WHERE NOT EXISTS (
                SELECT 1
                FROM nl2sql_column_masking_rules
                WHERE tenant_id = ?
                  AND deleted_at IS NULL
                  AND enabled = 1
                  AND COALESCE(datasource_id, '') = ''
                  AND table_name = '%'
                  AND column_name = ?
                  AND mask_type = 'redact'
             )",
        )
        .bind(tenant_id)
        .bind(pattern)
        .bind(pattern)
        .bind(actor)
        .bind(tenant_id)
        .bind(pattern)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_tenant_defaults_seed_without_duplicates() {
        let db = crate::test_sqlite_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO tenants (id, name, slug, plan) VALUES (?, 'Test', ?, 'free')",
        )
        .bind(&tenant_id)
        .bind(format!("test-{}", uuid::Uuid::new_v4()))
        .execute(&db)
        .await
        .expect("insert tenant fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO users
               (id, email, name, password_hash, role, tenant_id, is_active)
             VALUES (?, ?, 'Test Admin', 'not-used', 'admin', ?, 1)",
        )
        .bind(&user_id)
        .bind(format!("{}@example.invalid", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .execute(&db)
        .await
        .expect("insert user fixture");

        for _ in 0..2 {
            let mut tx = db.begin().await.expect("begin seed transaction");
            seed_tenant_defaults_with_tx(&mut tx, &tenant_id, Some(&user_id))
                .await
                .expect("seed tenant defaults");
            tx.commit().await.expect("commit tenant defaults");
        }

        let mask_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT COUNT(*) FROM nl2sql_column_masking_rules
             WHERE tenant_id = ? AND deleted_at IS NULL",
        )
        .bind(&tenant_id)
        .fetch_one(&db)
        .await
        .expect("count default masking rules");
        assert_eq!(
            mask_count,
            i64::try_from(NL2SQL_DEFAULT_MASK_PATTERNS.len()).unwrap_or(i64::MAX)
        );

        let budget_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT COUNT(*) FROM pm_budget_profiles
             WHERE tenant_id = ? AND profile_key = 'normal' AND is_default = 1",
        )
        .bind(&tenant_id)
        .fetch_one(&db)
        .await
        .expect("count default PM budget profile");
        assert_eq!(budget_count, 1);

        let repositories: Vec<(String, String)> = sqlx::query_as::<sqlx::Sqlite, _>(
            "SELECT repo_full_name, branch FROM skills_market_repositories
             WHERE tenant_id = ? ORDER BY repo_full_name",
        )
        .bind(&tenant_id)
        .fetch_all(&db)
        .await
        .expect("load default Skill repositories");
        let mut expected_repositories = BUILTIN_SKILL_REPOSITORIES
            .iter()
            .map(|(repository, branch)| ((*repository).to_string(), (*branch).to_string()))
            .collect::<Vec<_>>();
        expected_repositories.sort_unstable();
        assert_eq!(repositories, expected_repositories);
        db.close().await;
    }
}

async fn seed_builtin_skill_repositories_with_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    created_by: Option<&str>,
) -> Result<()> {
    for (repo_full_name, branch) in BUILTIN_SKILL_REPOSITORIES {
        sqlx::query(
            "INSERT INTO skills_market_repositories
                (tenant_id, repo_full_name, repo_url, branch, enabled, discovered_count, last_scan_status, created_by)
             VALUES (?, ?, ?, ?, 1, 0, 'idle', ?)
             ON CONFLICT DO UPDATE SET
                repo_url = excluded.repo_url,
                enabled = excluded.enabled,
                updated_at = CURRENT_TIMESTAMP",
        )
        .bind(tenant_id)
        .bind(repo_full_name)
        .bind(format!("https://github.com/{repo_full_name}"))
        .bind(branch)
        .bind(created_by)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}
