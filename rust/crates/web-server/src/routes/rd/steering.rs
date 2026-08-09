//! Team steering rules for AOS Code Studio prompts.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSteeringRuleRequest {
    repository_id: Option<String>,
    repository_ids: Option<Vec<String>>,
    name: String,
    description: Option<String>,
    content_md: String,
    enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdSteeringRuleDto {
    id: String,
    repository_id: Option<String>,
    repository_ids: Vec<String>,
    name: String,
    description: Option<String>,
    content_md: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default)]
pub(super) struct RdSteeringContext {
    pub(super) text: String,
    pub(super) rule_count: usize,
    pub(super) rule_names: Vec<String>,
}

pub(super) async fn list_steering_rules(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Vec<RdSteeringRuleDto>>, AppError> {
    let rows = sqlx::query("SELECT id, repository_id, name, description, content_md, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at FROM rd_steering_rules WHERE tenant_id = ? ORDER BY enabled DESC, repository_id IS NULL DESC, updated_at DESC")
        .bind(&claims.tenant_id)
        .fetch_all(&state.db)
        .await?;
    let mut rules = rows.iter().map(row_to_steering_rule).collect::<Vec<_>>();
    attach_steering_rule_repositories(&state.db, &claims.tenant_id, &mut rules).await?;
    Ok(Json(rules))
}

pub(super) async fn create_steering_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdSteeringRuleRequest>,
) -> Result<Json<RdSteeringRuleDto>, AppError> {
    let repository_ids = normalize_steering_repository_ids(&req);
    for repo_id in &repository_ids {
        ensure_repository_exists(&state, &claims, repo_id).await?;
    }
    let repository_id = repository_ids.first().cloned();
    let name = require_non_empty(&req.name, "name")?;
    let description = normalize_optional(req.description.as_deref()).map(ToOwned::to_owned);
    let content_md = require_non_empty(&req.content_md, "content_md")?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO rd_steering_rules (id, tenant_id, repository_id, name, description, content_md, enabled) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&claims.tenant_id)
        .bind(&repository_id)
        .bind(&name)
        .bind(&description)
        .bind(&content_md)
        .bind(req.enabled.unwrap_or(true))
        .execute(&mut *tx)
        .await?;
    replace_steering_rule_repositories(&mut tx, &claims.tenant_id, &id, &repository_ids).await?;
    tx.commit().await?;
    get_steering_rule_row(&state.db, &claims.tenant_id, &id)
        .await
        .map(Json)
}

pub(super) async fn update_steering_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RdSteeringRuleRequest>,
) -> Result<Json<RdSteeringRuleDto>, AppError> {
    let repository_ids = normalize_steering_repository_ids(&req);
    for repo_id in &repository_ids {
        ensure_repository_exists(&state, &claims, repo_id).await?;
    }
    let repository_id = repository_ids.first().cloned();
    let name = require_non_empty(&req.name, "name")?;
    let description = normalize_optional(req.description.as_deref()).map(ToOwned::to_owned);
    let content_md = require_non_empty(&req.content_md, "content_md")?;
    let mut tx = state.db.begin().await?;
    let result = sqlx::query("UPDATE rd_steering_rules SET repository_id = ?, name = ?, description = ?, content_md = ?, enabled = ? WHERE id = ? AND tenant_id = ?")
        .bind(&repository_id)
        .bind(&name)
        .bind(&description)
        .bind(&content_md)
        .bind(req.enabled.unwrap_or(true))
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("rd steering rule not found".to_string()));
    }
    replace_steering_rule_repositories(&mut tx, &claims.tenant_id, &id, &repository_ids).await?;
    tx.commit().await?;
    get_steering_rule_row(&state.db, &claims.tenant_id, &id)
        .await
        .map(Json)
}

pub(super) async fn delete_steering_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    let result = sqlx::query("DELETE FROM rd_steering_rules WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "deleted": result.rows_affected() > 0 })))
}

fn normalize_steering_repository_ids(req: &RdSteeringRuleRequest) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();

    if let Some(repository_ids) = &req.repository_ids {
        for raw in repository_ids {
            if let Some(value) = normalize_optional(Some(raw.as_str())) {
                let value = value.to_string();
                if seen.insert(value.clone()) {
                    ids.push(value);
                }
            }
        }
    }

    if let Some(value) = normalize_optional(req.repository_id.as_deref()) {
        let value = value.to_string();
        if seen.insert(value.clone()) {
            ids.push(value);
        }
    }

    ids
}

async fn replace_steering_rule_repositories(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    rule_id: &str,
    repository_ids: &[String],
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM rd_steering_rule_repositories WHERE rule_id = ? AND tenant_id = ?")
        .bind(rule_id)
        .bind(tenant_id)
        .execute(&mut **tx)
        .await?;

    for repository_id in repository_ids {
        sqlx::query(
            "INSERT INTO rd_steering_rule_repositories (rule_id, tenant_id, repository_id) VALUES (?, ?, ?)",
        )
        .bind(rule_id)
        .bind(tenant_id)
        .bind(repository_id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

async fn attach_steering_rule_repositories(
    db: &SqlitePool,
    tenant_id: &str,
    rules: &mut [RdSteeringRuleDto],
) -> Result<(), AppError> {
    if rules.is_empty() {
        return Ok(());
    }

    let rule_ids = rules
        .iter()
        .map(|rule| rule.id.clone())
        .collect::<HashSet<_>>();
    let rows = sqlx::query(
        "SELECT rule_id, repository_id \
         FROM rd_steering_rule_repositories \
         WHERE tenant_id = ? \
         ORDER BY created_at ASC, repository_id ASC",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    let mut scopes: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let rule_id: String = row.get("rule_id");
        if !rule_ids.contains(&rule_id) {
            continue;
        }
        scopes
            .entry(rule_id)
            .or_default()
            .push(row.get("repository_id"));
    }

    for rule in rules {
        if let Some(repository_ids) = scopes.remove(&rule.id) {
            rule.repository_ids = repository_ids;
        } else if let Some(repository_id) = &rule.repository_id {
            rule.repository_ids = vec![repository_id.clone()];
        }
    }

    Ok(())
}

async fn get_steering_rule_row(
    db: &SqlitePool,
    tenant_id: &str,
    rule_id: &str,
) -> Result<RdSteeringRuleDto, AppError> {
    let row = sqlx::query("SELECT id, repository_id, name, description, content_md, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at FROM rd_steering_rules WHERE id = ? AND tenant_id = ?")
        .bind(rule_id)
        .bind(tenant_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("rd steering rule not found".to_string()))?;
    let mut rules = vec![row_to_steering_rule(&row)];
    attach_steering_rule_repositories(db, tenant_id, &mut rules).await?;
    rules
        .pop()
        .ok_or_else(|| AppError::NotFound("rd steering rule not found".to_string()))
}

pub(super) async fn build_steering_context(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: Option<&str>,
) -> Result<RdSteeringContext, AppError> {
    let rows = if let Some(repo_id) = repository_id {
        sqlx::query(
            "SELECT r.name, r.content_md \
             FROM rd_steering_rules r \
             WHERE r.tenant_id = ? AND r.enabled = 1 \
               AND ( \
                 (r.repository_id IS NULL AND NOT EXISTS (SELECT 1 FROM rd_steering_rule_repositories rr_global WHERE rr_global.rule_id = r.id)) \
                 OR r.repository_id = ? \
                 OR EXISTS (SELECT 1 FROM rd_steering_rule_repositories rr_scope WHERE rr_scope.rule_id = r.id AND rr_scope.repository_id = ?) \
               ) \
             ORDER BY (r.repository_id IS NULL AND NOT EXISTS (SELECT 1 FROM rd_steering_rule_repositories rr_order WHERE rr_order.rule_id = r.id)) DESC, r.created_at ASC",
        )
            .bind(tenant_id)
            .bind(repo_id)
            .bind(repo_id)
            .fetch_all(db)
            .await?
    } else {
        sqlx::query(
            "SELECT r.name, r.content_md \
             FROM rd_steering_rules r \
             WHERE r.tenant_id = ? AND r.enabled = 1 AND r.repository_id IS NULL \
               AND NOT EXISTS (SELECT 1 FROM rd_steering_rule_repositories rr_global WHERE rr_global.rule_id = r.id) \
             ORDER BY r.created_at ASC",
        )
            .bind(tenant_id)
            .fetch_all(db)
            .await?
    };
    let mut rule_names = Vec::new();
    let mut sections = Vec::new();
    for row in rows {
        let name: String = row.get("name");
        let content_md: String = row.get("content_md");
        rule_names.push(name.clone());
        sections.push(format!("### {name}\n{}", content_md.trim()));
    }
    Ok(RdSteeringContext {
        text: sections.join("\n\n"),
        rule_count: rule_names.len(),
        rule_names,
    })
}

fn row_to_steering_rule(row: &sqlx::sqlite::SqliteRow) -> RdSteeringRuleDto {
    RdSteeringRuleDto {
        id: row.get("id"),
        repository_id: row.get("repository_id"),
        repository_ids: Vec::new(),
        name: row.get("name"),
        description: row.get("description"),
        content_md: row.get("content_md"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
