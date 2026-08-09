//! Column-level masking rules (R-7 / R-8).
//!
//! Backs the previously-dead `nl2sql_column_masking_rules` table with a full
//! CRUD surface and a row-application engine that runs at the end of every
//! NL2SQL execution path. Rules let admins declaratively mask sensitive
//! columns by wildcard pattern, with multiple strategies:
//!
//! * `redact`   — replace with `"***MASKED***"`
//! * `null`     — replace with JSON null
//! * `constant` — replace with `constant_value`
//! * `hash`     — replace with the first 8 hex chars of SHA-256(value)
//! * `partial`  — keep leading 2 + trailing 2 chars, fill middle with `*`
//! * `tokenize` — deterministic per-tenant token (SHA-256 of value + tenant_id)
//!
//! Rules are matched in priority order (lowest priority number = highest
//! priority). The first rule whose `(datasource_id, table_name, column_name)`
//! patterns match wins. Wildcards use SQL `LIKE` semantics (`%` = any
//! sequence, `_` = single char).

use super::auth::require_admin;
use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── DTOs ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MaskingRuleRow {
    pub id: u64,
    pub datasource_id: Option<String>,
    pub table_name: String,
    pub column_name: String,
    pub mask_type: String,
    pub pattern: Option<String>,
    pub constant_value: Option<String>,
    pub priority: i32,
    pub description: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct ListMaskingRulesResponse {
    pub rules: Vec<MaskingRuleRow>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateMaskingRuleRequest {
    pub datasource_id: Option<String>,
    pub table_name: String,
    pub column_name: String,
    pub mask_type: String,
    pub pattern: Option<String>,
    pub constant_value: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: i32,
    pub description: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_priority() -> i32 {
    100
}
fn default_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateMaskingRuleRequest {
    pub table_name: Option<String>,
    pub column_name: Option<String>,
    pub mask_type: Option<String>,
    pub pattern: Option<String>,
    pub constant_value: Option<String>,
    pub priority: Option<i32>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct GenericIdResponse {
    pub id: u64,
}

#[derive(Debug, Serialize)]
pub struct GenericSuccessResponse {
    pub success: bool,
}

// ── Validation ──────────────────────────────────────────────────────────────

const ALLOWED_MASK_TYPES: &[&str] = &["redact", "hash", "tokenize", "partial", "null", "constant"];

fn validate_mask_type(t: &str) -> Result<()> {
    if ALLOWED_MASK_TYPES.contains(&t) {
        Ok(())
    } else {
        Err(crate::error::AppError::ValidationError(format!(
            "invalid mask_type '{t}', expected one of: {}",
            ALLOWED_MASK_TYPES.join(", ")
        )))
    }
}

// ── HTTP handlers ───────────────────────────────────────────────────────────

/// GET /nl2sql/masking-rules — list all rules for the current tenant.
pub(crate) async fn list_masking_rules(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ListMaskingRulesResponse>> {
    let tenant_id = &claims.tenant_id;

    let rules: Vec<MaskingRuleRow> = sqlx::query_as::<sqlx::Sqlite, _>(
        r#"
        SELECT id, datasource_id, table_name, column_name, mask_type, pattern,
               constant_value, priority, description, enabled
        FROM nl2sql_column_masking_rules
        WHERE tenant_id = ? AND deleted_at IS NULL
        ORDER BY priority ASC, id ASC
        "#,
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(ListMaskingRulesResponse { rules }))
}

/// POST /nl2sql/masking-rules — create a rule. Admin only.
pub(crate) async fn create_masking_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateMaskingRuleRequest>,
) -> Result<Json<GenericIdResponse>> {
    require_admin(&claims)?;
    validate_mask_type(&req.mask_type)?;

    let tenant_id = &claims.tenant_id;
    let id: u64 = sqlx::query::<sqlx::Sqlite>(
        r#"
        INSERT INTO nl2sql_column_masking_rules
          (tenant_id, datasource_id, table_name, column_name, mask_type, pattern,
           constant_value, priority, description, enabled, created_by)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(tenant_id)
    .bind(&req.datasource_id)
    .bind(&req.table_name)
    .bind(&req.column_name)
    .bind(&req.mask_type)
    .bind(&req.pattern)
    .bind(&req.constant_value)
    .bind(req.priority)
    .bind(&req.description)
    .bind(req.enabled)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?
    .last_insert_rowid()
    .try_into()
    .map_err(|_| crate::AppError::Internal("invalid SQLite rowid".into()))?;

    Ok(Json(GenericIdResponse { id }))
}

/// PATCH /nl2sql/masking-rules/{id} — update a rule. Admin only.
pub(crate) async fn update_masking_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<u64>,
    Json(req): Json<UpdateMaskingRuleRequest>,
) -> Result<Json<GenericSuccessResponse>> {
    require_admin(&claims)?;
    if let Some(t) = &req.mask_type {
        validate_mask_type(t)?;
    }
    let tenant_id = &claims.tenant_id;
    sqlx::query::<sqlx::Sqlite>(
        r#"
        UPDATE nl2sql_column_masking_rules SET
          table_name      = COALESCE(?, table_name),
          column_name     = COALESCE(?, column_name),
          mask_type       = COALESCE(?, mask_type),
          pattern         = COALESCE(?, pattern),
          constant_value  = COALESCE(?, constant_value),
          priority        = COALESCE(?, priority),
          description     = COALESCE(?, description),
          enabled         = COALESCE(?, enabled)
        WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL
        "#,
    )
    .bind(&req.table_name)
    .bind(&req.column_name)
    .bind(&req.mask_type)
    .bind(&req.pattern)
    .bind(&req.constant_value)
    .bind(req.priority)
    .bind(&req.description)
    .bind(req.enabled)
    .bind(crate::sqlite_i64(id))
    .bind(tenant_id)
    .execute(&state.db)
    .await?;

    Ok(Json(GenericSuccessResponse { success: true }))
}

/// DELETE /nl2sql/masking-rules/{id} — soft-delete a rule. Admin only.
pub(crate) async fn delete_masking_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<u64>,
) -> Result<Json<GenericSuccessResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;

    sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_column_masking_rules SET deleted_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(crate::sqlite_i64(id))
    .bind(tenant_id)
    .execute(&state.db)
    .await?;

    Ok(Json(GenericSuccessResponse { success: true }))
}

// ── Apply engine ────────────────────────────────────────────────────────────

/// In-memory representation of a masking rule used when iterating rows.
#[derive(Debug, Clone)]
pub struct CompiledMaskingRule {
    pub table_pattern: String,
    pub column_pattern: String,
    pub mask_type: String,
    pub constant_value: Option<String>,
    pub priority: i32,
}

/// Loads the active rules for `(tenant_id, datasource_id)` ordered by priority
/// ascending (most specific first). Rules with `datasource_id = NULL` apply to
/// every datasource in the tenant. Disabled or soft-deleted rules are skipped.
pub(crate) async fn load_active_rules(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> Vec<CompiledMaskingRule> {
    let rows: Vec<(String, String, String, Option<String>, i32)> =
        sqlx::query_as::<sqlx::Sqlite, _>(
            r#"
        SELECT table_name, column_name, mask_type, constant_value, priority
        FROM nl2sql_column_masking_rules
        WHERE tenant_id = ?
          AND (datasource_id IS NULL OR datasource_id = ?)
          AND enabled = 1
          AND deleted_at IS NULL
        ORDER BY priority ASC, id ASC
        "#,
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();

    rows.into_iter()
        .map(
            |(table_pattern, column_pattern, mask_type, constant_value, priority)| {
                CompiledMaskingRule {
                    table_pattern,
                    column_pattern,
                    mask_type,
                    constant_value,
                    priority,
                }
            },
        )
        .collect()
}

/// SQL `LIKE` semantics — `%` matches any sequence, `_` matches a single char.
/// All other characters match literally (case-insensitive).
pub(crate) fn like_match(pattern: &str, value: &str) -> bool {
    let p = pattern.to_lowercase();
    let v = value.to_lowercase();
    let p_bytes = p.as_bytes();
    let v_bytes = v.as_bytes();
    like_match_inner(p_bytes, 0, v_bytes, 0)
}

fn like_match_inner(p: &[u8], pi: usize, v: &[u8], vi: usize) -> bool {
    if pi == p.len() {
        return vi == v.len();
    }
    match p[pi] {
        b'%' => {
            // Match the rest of `p` against any suffix of `v`.
            for k in vi..=v.len() {
                if like_match_inner(p, pi + 1, v, k) {
                    return true;
                }
            }
            false
        }
        b'_' => {
            if vi < v.len() {
                like_match_inner(p, pi + 1, v, vi + 1)
            } else {
                false
            }
        }
        c => vi < v.len() && v[vi] == c && like_match_inner(p, pi + 1, v, vi + 1),
    }
}

/// Apply the highest-priority matching rule to `value`, returning the masked
/// JSON value. If no rule matches, the value is returned unchanged.
pub(crate) fn apply_rules_to_value(
    rules: &[CompiledMaskingRule],
    tenant_id: &str,
    table_name: &str,
    column_name: &str,
    value: &serde_json::Value,
) -> serde_json::Value {
    // Rules already sorted by priority ASC.
    for rule in rules {
        if like_match(&rule.table_pattern, table_name)
            && like_match(&rule.column_pattern, column_name)
        {
            return apply_one(rule, tenant_id, value);
        }
    }
    value.clone()
}

/// Returns the first matching masking rule for `(table_name, column_name)` in
/// priority order. This is useful for observability/audit surfaces where we
/// need to explain *which* rule was applied.
pub(crate) fn find_first_matching_rule<'a>(
    rules: &'a [CompiledMaskingRule],
    table_name: &str,
    column_name: &str,
) -> Option<&'a CompiledMaskingRule> {
    rules.iter().find(|rule| {
        like_match(&rule.table_pattern, table_name) && like_match(&rule.column_pattern, column_name)
    })
}

fn apply_one(
    rule: &CompiledMaskingRule,
    tenant_id: &str,
    value: &serde_json::Value,
) -> serde_json::Value {
    use serde_json::Value;
    match rule.mask_type.as_str() {
        "null" => Value::Null,
        "constant" => Value::String(rule.constant_value.clone().unwrap_or_default()),
        "redact" => Value::String("***MASKED***".to_string()),
        "hash" => {
            let raw = stringify_for_hash(value);
            let mut h = Sha256::new();
            h.update(raw.as_bytes());
            let digest = h.finalize();
            let mut hex = String::with_capacity(16);
            for byte in digest.iter().take(8) {
                let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{:02x}", byte));
            }
            Value::String(hex)
        }
        "tokenize" => {
            let raw = stringify_for_hash(value);
            let mut h = Sha256::new();
            h.update(tenant_id.as_bytes());
            h.update(b"|");
            h.update(raw.as_bytes());
            let digest = h.finalize();
            let mut hex = String::with_capacity(32);
            for byte in digest.iter().take(16) {
                let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{:02x}", byte));
            }
            Value::String(format!("tok_{}", hex))
        }
        "partial" => {
            let s = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return value.clone(),
            };
            Value::String(partial_mask(&s))
        }
        _ => value.clone(),
    }
}

fn stringify_for_hash(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn partial_mask(s: &str) -> String {
    // Keep first 2 and last 2 chars, mask the rest. For very short strings
    // (<= 4 chars) we mask everything to avoid effectively leaking the whole
    // value.
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(2).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mid = "*".repeat(chars.len() - 4);
    format!("{head}{mid}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn like_pattern_matches() {
        assert!(like_match("%", "anything"));
        assert!(like_match("user_%", "user_orders"));
        assert!(like_match("%_password", "admin_password"));
        assert!(!like_match("user_%", "orders"));
        assert!(like_match("UPPER", "upper")); // case-insensitive
        assert!(like_match("a_c", "abc"));
        assert!(!like_match("a_c", "abbc"));
    }

    fn rule(table: &str, col: &str, t: &str) -> CompiledMaskingRule {
        CompiledMaskingRule {
            table_pattern: table.into(),
            column_pattern: col.into(),
            mask_type: t.into(),
            constant_value: None,
            priority: 100,
        }
    }

    #[test]
    fn redact_strings() {
        let rules = vec![rule("users", "%password%", "redact")];
        let out = apply_rules_to_value(&rules, "t1", "users", "user_password", &json!("hunter2"));
        assert_eq!(out, json!("***MASKED***"));
    }

    #[test]
    fn null_replaces_with_null() {
        let rules = vec![rule("users", "ssn", "null")];
        let out = apply_rules_to_value(&rules, "t1", "users", "ssn", &json!("123-45-6789"));
        assert_eq!(out, serde_json::Value::Null);
    }

    #[test]
    fn partial_keeps_edges() {
        let rules = vec![rule("users", "phone", "partial")];
        let out = apply_rules_to_value(&rules, "t1", "users", "phone", &json!("13800138000"));
        let s = out.as_str().unwrap();
        assert!(s.starts_with("13"));
        assert!(s.ends_with("00"));
        assert!(s.contains('*'));
    }

    #[test]
    fn tokenize_is_deterministic_per_tenant() {
        let rules = vec![rule("users", "email", "tokenize")];
        let a = apply_rules_to_value(&rules, "t1", "users", "email", &json!("a@b.com"));
        let b = apply_rules_to_value(&rules, "t1", "users", "email", &json!("a@b.com"));
        let c = apply_rules_to_value(&rules, "t2", "users", "email", &json!("a@b.com"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn no_rule_no_change() {
        let rules: Vec<CompiledMaskingRule> = Vec::new();
        let out = apply_rules_to_value(&rules, "t1", "users", "name", &json!("Alice"));
        assert_eq!(out, json!("Alice"));
    }

    #[test]
    fn priority_first_match_wins() {
        let rules = vec![
            CompiledMaskingRule {
                table_pattern: "users".into(),
                column_pattern: "ssn".into(),
                mask_type: "constant".into(),
                constant_value: Some("FIRST".into()),
                priority: 1,
            },
            CompiledMaskingRule {
                table_pattern: "%".into(),
                column_pattern: "%".into(),
                mask_type: "redact".into(),
                constant_value: None,
                priority: 100,
            },
        ];
        let out = apply_rules_to_value(&rules, "t1", "users", "ssn", &json!("123"));
        assert_eq!(out, json!("FIRST"));
    }
}
