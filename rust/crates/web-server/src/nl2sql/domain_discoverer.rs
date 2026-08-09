//! DomainDiscoverer — auto-infers business domains from schema structure using the LLM.
//!
//! Works alongside [`SchemaDescriber`]: after the schema is discovered, this module
//! clusters tables into business domains (e.g. "销售订单", "客户管理") so the
//! routing engine can narrow candidates before doing column-level matching.
//!
//! Output: rows in `nl2sql_business_domains` + `nl2sql_table_domain_mapping`.

use std::collections::HashSet;

use api::{InputContentBlock, InputMessage, MessageRequest, OutputContentBlock};
use sqlx::SqlitePool;

use crate::nl2sql::ChatTenantConfig;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DomainCluster {
    pub domain: String,
    pub description: String,
    pub tables: Vec<String>,
    pub confidence: f32,
}

#[derive(Clone)]
pub struct DomainDiscoverer {
    db: SqlitePool,
    chat_config: ChatTenantConfig,
}

impl DomainDiscoverer {
    pub fn new(db: SqlitePool, chat_config: ChatTenantConfig) -> Self {
        Self { db, chat_config }
    }

    /// Auto-discover business domains for a datasource by feeding all table names
    /// + column descriptions to the LLM and asking it to cluster them.
    ///
    /// Returns a list of [`DomainCluster`] sorted by number of tables descending.
    /// Results are written to `nl2sql_business_domains` + `nl2sql_table_domain_mapping`.
    ///
    /// This is called at the end of a full datasource refresh. Partial refresh
    /// (individual table updates) does NOT re-run discovery.
    pub async fn discover_domains(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        schema_json: &serde_json::Value,
        created_by: Option<&str>,
    ) -> anyhow::Result<Vec<DomainCluster>> {
        let tables = self.extract_tables(schema_json);
        if tables.is_empty() {
            return Ok(Vec::new());
        }

        let prompt = self.build_discovery_prompt(&tables);
        let clusters = self.normalize_clusters(self.call_llm_clusters(&prompt).await?, &tables);

        self.persist_clusters(
            tenant_id,
            datasource_id,
            &clusters.iter().collect::<Vec<_>>(),
            created_by,
            false,
        )
        .await?;
        Ok(clusters)
    }

    /// Force re-discovery (e.g. user clicked "重新发现" in UI).
    /// Deletes existing auto-discovered domains and re-runs discovery.
    pub async fn rediscover(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        schema_json: &serde_json::Value,
        created_by: Option<&str>,
    ) -> anyhow::Result<Vec<DomainCluster>> {
        let tables = self.extract_tables(schema_json);
        if tables.is_empty() {
            return Ok(Vec::new());
        }

        // Parse and validate the replacement before opening the transaction. A
        // truncated or invalid model response must never erase the last usable
        // auto-discovered domain set.
        let prompt = self.build_discovery_prompt(&tables);
        let clusters = self.normalize_clusters(self.call_llm_clusters(&prompt).await?, &tables);
        self.persist_clusters(
            tenant_id,
            datasource_id,
            &clusters.iter().collect::<Vec<_>>(),
            created_by,
            true,
        )
        .await?;
        Ok(clusters)
    }

    // ─── Prompt construction ──────────────────────────────────────────────────

    fn extract_tables<'a>(&self, schema_json: &'a serde_json::Value) -> Vec<&'a serde_json::Value> {
        let tables = match schema_json {
            serde_json::Value::Array(arr) => arr.as_slice(),
            serde_json::Value::Object(obj) => {
                if let Some(arr) = obj.get("tables").and_then(|v| v.as_array()) {
                    arr.as_slice()
                } else if let Some(arr) = obj.get("data_sources").and_then(|v| v.as_array()) {
                    arr.as_slice()
                } else {
                    return vec![];
                }
            }
            _ => return vec![],
        };
        tables.iter().collect()
    }

    fn build_discovery_prompt(&self, tables: &[&serde_json::Value]) -> String {
        let mut lines = vec![
            "You are a data architect. Analyze the following database tables and group them into 3-8 business domains.".to_string(),
            "Each domain should represent a distinct business area (e.g. '销售订单', '客户管理', '财务对账').".to_string(),
            "".to_string(),
            "Return ONLY valid JSON array (no markdown fences, no explanation):".to_string(),
            "[".to_string(),
            "  { \"domain\": \"domain name\", \"description\": \"1-sentence description\", \"tables\": [\"table1\", \"table2\"], \"confidence\": 0.8 },".to_string(),
            "  ...".to_string(),
            "]".to_string(),
            "".to_string(),
            "Rules:".to_string(),
            "- Domain names should be in Chinese or English (match the codebase language)".to_string(),
            "- Put tables that serve the same business function together".to_string(),
            "- At least 3 domains, at most 8".to_string(),
            "- Each table belongs to exactly one domain".to_string(),
            "- Uncategorizable tables → put in a '其他' domain".to_string(),
            "- confidence: 0.7-1.0 if you are sure, 0.4-0.6 if uncertain".to_string(),
            "".to_string(),
            "Tables:".to_string(),
        ];

        for table in tables {
            let name = table
                .get("table_name")
                .or_else(|| table.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let desc = table
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let mut cols = Vec::new();
            if let Some(arr) = table.get("columns").and_then(|v| v.as_array()) {
                for col in arr {
                    let cn = col.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let ct = col.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                    let cd = col
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !cd.is_empty() {
                        cols.push(format!("  {cn} ({ct}): {cd}"));
                    } else {
                        cols.push(format!("  {cn} ({ct})"));
                    }
                }
            }

            lines.push(format!("## {name}"));
            if !desc.is_empty() {
                lines.push(format!("  {desc}"));
            }
            if !cols.is_empty() {
                lines.push("  Columns:".to_string());
                lines.extend(cols);
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }

    async fn call_llm_clusters(&self, prompt: &str) -> anyhow::Result<Vec<DomainCluster>> {
        let request = MessageRequest {
            model: self.chat_config.model.clone(),
            // 1K is routinely exhausted by reasoning-capable providers before
            // the closing JSON bracket. The result is still bounded to 3-8
            // compact clusters, so 4K is ample without allowing runaway output.
            max_tokens: 4096,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: prompt.to_string(),
                }],
            }],
            temperature: Some(0.3),
            system: Some(
                "You are a data architect. Always respond with valid JSON only.".to_string(),
            ),
            frequency_penalty: None,
            presence_penalty: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
            stream: false,
            stop: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };

        let resp = self.chat_config.client.send_message(&request).await?;
        let raw = resp
            .content
            .iter()
            .filter_map(|b| match b {
                OutputContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let cleaned = raw
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let parsed = parse_domain_cluster_values(cleaned)?;

        let clusters = parsed
            .into_iter()
            .filter_map(|v| {
                let domain = v.get("domain")?.as_str()?.to_string();
                let description = v
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let tables: Vec<String> = v
                    .get("tables")
                    .and_then(|t| t.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let confidence = v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.5) as f32;
                Some(DomainCluster {
                    domain,
                    description,
                    tables,
                    confidence,
                })
            })
            .collect();

        Ok(clusters)
    }

    fn normalize_clusters(
        &self,
        clusters: Vec<DomainCluster>,
        tables: &[&serde_json::Value],
    ) -> Vec<DomainCluster> {
        let table_names = tables
            .iter()
            .filter_map(|table| {
                table
                    .get("table_name")
                    .or_else(|| table.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();
        let allowed = table_names.iter().cloned().collect::<HashSet<_>>();
        let mut assigned = HashSet::new();
        let mut normalized: Vec<DomainCluster> = Vec::new();

        for mut cluster in clusters {
            cluster.domain = cluster.domain.trim().to_string();
            cluster.description = cluster.description.trim().to_string();
            cluster.confidence = cluster.confidence.clamp(0.0, 1.0);
            cluster.tables.retain(|table| {
                let table = table.trim();
                !table.is_empty() && allowed.contains(table) && assigned.insert(table.to_string())
            });
            cluster.tables = cluster
                .tables
                .into_iter()
                .map(|table| table.trim().to_string())
                .collect();
            if cluster.domain.is_empty() || cluster.tables.is_empty() {
                continue;
            }
            if let Some(existing) = normalized
                .iter_mut()
                .find(|existing| existing.domain == cluster.domain)
            {
                existing.tables.extend(cluster.tables);
                existing.confidence = existing.confidence.max(cluster.confidence);
                if existing.description.is_empty() {
                    existing.description = cluster.description;
                }
            } else {
                normalized.push(cluster);
            }
        }

        let missing = table_names
            .into_iter()
            .filter(|table| !assigned.contains(table))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            if let Some(other) = normalized
                .iter_mut()
                .find(|cluster| matches!(cluster.domain.as_str(), "其他" | "其它" | "Other"))
            {
                other.tables.extend(missing);
                other.confidence = other.confidence.min(0.5);
            } else {
                normalized.push(DomainCluster {
                    domain: "其他".to_string(),
                    description: "模型未完整分类的其余数据表".to_string(),
                    tables: missing,
                    confidence: 0.4,
                });
            }
        }

        if normalized.is_empty() {
            normalized.push(DomainCluster {
                domain: "默认".to_string(),
                description: "所有表的默认分类".to_string(),
                tables: allowed.into_iter().collect(),
                confidence: 0.3,
            });
        }
        normalized.sort_by(|left, right| right.tables.len().cmp(&left.tables.len()));
        normalized
    }

    async fn persist_clusters(
        &self,
        tenant_id: &str,
        datasource_id: &str,
        clusters: &[&DomainCluster],
        created_by: Option<&str>,
        replace_existing_auto: bool,
    ) -> anyhow::Result<()> {
        let mut tx = sqlx::Acquire::begin(&self.db).await?;

        if replace_existing_auto {
            sqlx::query(
                "DELETE FROM nl2sql_table_domain_mapping \
                 WHERE domain_id IN (\
                     SELECT id FROM nl2sql_business_domains \
                     WHERE tenant_id = ? AND datasource_id = ? AND source = 'auto'\
                 )",
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM nl2sql_business_domains \
                 WHERE tenant_id = ? AND datasource_id = ? AND source = 'auto'",
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .execute(&mut *tx)
            .await?;
        }

        for cluster in clusters {
            sqlx::query(
                r#"
                INSERT INTO nl2sql_business_domains
                  (tenant_id, datasource_id, domain_name, domain_description, table_count, confidence_score, source, created_by)
                VALUES (?, ?, ?, ?, ?, ?, 'auto', ?)
                ON CONFLICT DO UPDATE SET
                  domain_description = excluded.domain_description,
                  table_count = excluded.table_count,
                  confidence_score = excluded.confidence_score,
                  updated_at = CURRENT_TIMESTAMP
                "#,
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(&cluster.domain)
            .bind(&cluster.description)
            .bind(cluster.tables.len() as i32)
            .bind(cluster.confidence)
            .bind(created_by)
            .execute(&mut *tx)
            .await?;

            let domain_id = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM nl2sql_business_domains \
                 WHERE tenant_id = ? AND datasource_id = ? AND domain_name = ?",
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(&cluster.domain)
            .fetch_one(&mut *tx)
            .await?;

            for table in &cluster.tables {
                sqlx::query(
                    r#"
                    INSERT INTO nl2sql_table_domain_mapping
                      (tenant_id, datasource_id, table_name, domain_id, confidence_score)
                    VALUES (?, ?, ?, ?, ?)
                    ON CONFLICT DO UPDATE SET
                      domain_id = excluded.domain_id,
                      confidence_score = excluded.confidence_score
                    "#,
                )
                .bind(tenant_id)
                .bind(datasource_id)
                .bind(table)
                .bind(domain_id)
                .bind(cluster.confidence)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }
}

fn parse_domain_cluster_values(cleaned: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    match serde_json::from_str(cleaned) {
        Ok(parsed) => Ok(parsed),
        Err(first_error) => {
            let Some(recovered) = complete_domain_cluster_array_prefix(cleaned) else {
                return Err(anyhow::anyhow!(
                    "failed to parse domain clusters: {first_error}: {}",
                    cleaned.chars().take(2_000).collect::<String>()
                ));
            };
            serde_json::from_str(&recovered).map_err(|recovery_error| {
                anyhow::anyhow!(
                    "failed to parse domain clusters: {first_error}; prefix recovery failed: {recovery_error}: {}",
                    cleaned.chars().take(2_000).collect::<String>()
                )
            })
        }
    }
}

fn complete_domain_cluster_array_prefix(input: &str) -> Option<String> {
    let input = input.trim();
    if !input.starts_with('[') {
        return None;
    }

    let mut in_string = false;
    let mut escaped = false;
    let mut array_depth = 0usize;
    let mut object_depth = 0usize;
    let mut last_complete_object_end = None;
    for (index, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '[' => array_depth = array_depth.saturating_add(1),
            ']' => array_depth = array_depth.saturating_sub(1),
            '{' => object_depth = object_depth.saturating_add(1),
            '}' => {
                object_depth = object_depth.saturating_sub(1);
                if array_depth == 1 && object_depth == 0 {
                    last_complete_object_end = Some(index + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    let end = last_complete_object_end?;
    Some(format!(
        "{}]",
        input[..end].trim_end_matches([',', ' ', '\n', '\r', '\t'])
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_domain_array_recovers_only_complete_objects() {
        let raw = r#"[
          {"domain":"订单","description":"包含 } 字符也安全","tables":["orders"],"confidence":0.9},
          {"domain":"告警","description":"任务告警","tables":["alerts"],"confidence":0.8},
          {"domain":"未完成""#;
        let parsed = parse_domain_cluster_values(raw).expect("recover complete JSON objects");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["domain"], "订单");
        assert_eq!(parsed[1]["tables"][0], "alerts");
    }

    #[test]
    fn invalid_non_array_response_is_not_guessed() {
        assert!(parse_domain_cluster_values("not json").is_err());
    }
}
