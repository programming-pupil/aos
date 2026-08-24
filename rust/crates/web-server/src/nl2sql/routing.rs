//! Semantic routing engine — selects the best data source/table based on cosine similarity + LLM reranking.
//!
//! Two routing strategies:
//! - **`route`**: Legacy embedding-only fusion routing (three-level cosine similarity).
//! - **`route_hybrid`**: Embedding coarse filter + LLM structured-output reranking.
//!
//! Hybrid routing flow:
//!   1. **Embedding coarse search**: global_table_search across ALL datasources → Top-20 tables.
//!   2. **LLM reranking** (performed by the caller / route handler): LLM receives Top-20
//!      candidates as structured text and picks best datasource(s), tables, and columns.
//!   3. **Clarification**: if LLM confidence < threshold → return ambiguity question.
//!
//! Cost per request: ~$0.003/req (1 embedding + 1 LLM call).

use crate::nl2sql::embedding::{cosine_similarity, EmbeddingStore, GlobalTableMatch};
use crate::nl2sql::{
    top_k_tables_for_llm, ClarificationOption, LlmRoutingDecision, MatchedTable, RoutingMethod,
    RoutingResult, MIN_CONFIDENCE,
};
use api;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct RouteStageSignal {
    pub stage: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_tables: Option<Vec<MatchedTable>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_method: Option<String>,
}

type RouteStageEmitter = Arc<dyn Fn(RouteStageSignal) + Send + Sync>;

tokio::task_local! {
    static ROUTE_STAGE_EMITTER: RouteStageEmitter;
}

pub(crate) async fn with_route_stage_emitter<F, T>(emitter: RouteStageEmitter, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    ROUTE_STAGE_EMITTER.scope(emitter, fut).await
}

pub(crate) fn emit_route_stage(
    stage: &str,
    message: &str,
    matched_tables: Option<Vec<MatchedTable>>,
    route_confidence: Option<f32>,
    routing_method: Option<String>,
) {
    let signal = RouteStageSignal {
        stage: stage.to_string(),
        message: message.to_string(),
        matched_tables,
        route_confidence,
        routing_method,
    };
    if let Ok(cb) = ROUTE_STAGE_EMITTER.try_with(|c| c.clone()) {
        cb(signal);
    }
}

/// Number of candidate tables for the legacy `route` fallback path.
/// The primary path (`route_hybrid`) uses `top_k_tables_for_llm()` (default 20) instead.
const TOP_K_TABLES: usize = 20;
const LLM_HIGH_CONFIDENCE: f32 = 0.75;
const ROUTING_LLM_TIMEOUT_DEFAULT_SECS: u64 = 180;

fn routing_llm_timeout_secs() -> u64 {
    std::env::var("NL2SQL_ROUTING_LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(ROUTING_LLM_TIMEOUT_DEFAULT_SECS)
}

fn mask_api_key(key: &str) -> String {
    if key.len() <= 12 {
        return "****".to_string();
    }
    let first = &key[..8];
    let last = &key[key.len() - 4..];
    format!("{first}****{last}")
}

#[derive(Debug, Clone)]
pub struct RoutingLlmMeta {
    pub provider: String,
    pub key_id: Option<String>,
    pub masked_api_key: Option<String>,
    pub base_url: String,
}

/// RoutingEngine — thread-safe, clonable.
#[derive(Clone)]
pub struct RoutingEngine {
    store: Arc<EmbeddingStore>,
    embed_model: Arc<str>,
    embed_url: Option<String>,
    embed_api_key: Option<String>,
}

/// Per-candidate scoring breakdown for diagnostics.
#[derive(Debug)]
#[allow(dead_code)]
struct CandidateScore {
    datasource_id: String,
    /// Datasource-level similarity score [-1, 1].
    ds_sim: f32,
    /// Table-level best similarity.
    table_sim: f32,
    /// Column-level best similarity.
    col_sim: f32,
    /// Final fused score [0, 1].
    fused: f32,
    /// Matched tables with column descriptions.
    matched_tables: Vec<MatchedTable>,
}

impl RoutingEngine {
    pub fn new(
        store: Arc<EmbeddingStore>,
        embed_model: Option<&str>,
        embed_url: Option<String>,
        embed_api_key: Option<String>,
    ) -> Self {
        Self {
            store,
            embed_model: Arc::from(embed_model.unwrap_or("text-embedding-3-small")),
            embed_url,
            embed_api_key,
        }
    }

    /// Route a user question to the best data source/table.
    /// `embed_api_key` comes from the per-tenant config in the database.
    /// Returns the top candidate with matched tables and confidence score.
    #[deprecated(
        since = "0.1.0",
        note = "replaced by route_hybrid(); this method is no longer invoked"
    )]
    pub async fn route(
        &self,
        question: &str,
        available_sources: &[(String, String, String)],
        embed_api_key: Option<String>,
    ) -> anyhow::Result<Option<RoutingResult>> {
        if available_sources.is_empty() {
            return Ok(None);
        }

        // 1. Embed the question once (uses per-tenant API key from database)
        let question_vec = self.embed_single(question, embed_api_key.clone()).await?;

        // 2. Delegate to the vector-based overload
        self.route_with_vector(question, &question_vec, available_sources)
            .await
    }

    /// Route using a pre-computed question embedding. Reuses the vector
    /// instead of re-embedding, avoiding redundant API calls and ensuring
    /// consistency between routing decision and detailed table scoring.
    pub async fn route_with_vector(
        &self,
        question: &str,
        question_vec: &[f32],
        available_sources: &[(String, String, String)],
    ) -> anyhow::Result<Option<RoutingResult>> {
        if available_sources.is_empty() {
            return Ok(None);
        }

        // ── Coarse pre-filter: text-level keyword overlap ─────────────────────────
        // Fast rejection of datasources whose description shares no significant
        // tokens with the question. This avoids loading embeddings from SQLite
        // for obviously irrelevant datasources. The question mark / "about" tokens
        // are stripped to avoid false exclusions.
        let question_lower = question.to_lowercase();
        let question_tokens: std::collections::HashSet<&str> = question_lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .collect();

        let mut coarse_pass_ids: Vec<&str> = Vec::new();
        for (ds_id, _ds_name, ds_desc) in available_sources {
            if ds_desc.trim().is_empty() {
                // No description → include by default (can't rule it out)
                coarse_pass_ids.push(ds_id);
            } else {
                let desc_lower = ds_desc.to_lowercase();
                let desc_tokens: std::collections::HashSet<&str> = desc_lower
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| t.len() > 2)
                    .collect();
                let overlap: usize = question_tokens.intersection(&desc_tokens).count();
                // Keep datasource if it shares at least 1 non-trivial token with the question,
                // or if the question is short (few tokens to begin with).
                let threshold = if question_tokens.len() <= 4 { 0 } else { 1 };
                if overlap > threshold {
                    coarse_pass_ids.push(ds_id);
                }
            }
        }

        if coarse_pass_ids.len() < available_sources.len() {
            tracing::debug!(
                coarse_pass = coarse_pass_ids.len(),
                total = available_sources.len(),
                "routing: coarse text filter cut {} datasources",
                available_sources.len() - coarse_pass_ids.len()
            );
        }

        // ── Detailed 3-level scoring ─────────────────────────────────────────
        let mut candidates: Vec<CandidateScore> = Vec::new();

        for (ds_id, _ds_name, _ds_desc) in available_sources {
            if !coarse_pass_ids.contains(&ds_id.as_str()) {
                continue;
            }
            match self.score_datasource(question_vec, ds_id).await {
                Ok(score) => candidates.push(score),
                Err(e) => {
                    tracing::warn!("failed to score datasource {ds_id}: {e}");
                }
            }
        }

        if candidates.is_empty() {
            return Ok(None);
        }

        // Sort by fused score descending
        candidates.sort_by(|a, b| {
            b.fused
                .partial_cmp(&a.fused)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let best = candidates.remove(0);

        // If fused confidence is very low, return None (let LLM suggest)
        if best.fused < MIN_CONFIDENCE {
            tracing::debug!(
                "routing confidence {} below threshold {} for question '{}'",
                best.fused,
                MIN_CONFIDENCE,
                question
            );
            return Ok(None);
        }

        Ok(Some(RoutingResult {
            data_source_id: best.datasource_id,
            matched_tables: best.matched_tables,
            confidence: best.fused,
            method: RoutingMethod::Auto,
        }))
    }

    /// Score a single datasource using 3-level fusion.
    #[allow(dead_code, clippy::unused_async)]
    async fn score_datasource(
        &self,
        question_vec: &[f32],
        datasource_id: &str,
    ) -> anyhow::Result<CandidateScore> {
        // ── Datasource-level score ─────────────────────────────────────────
        let ds_sim = match self.store.get_typed(
            datasource_id,
            "__datasource__",
            "__datasource__",
            "datasource",
        ) {
            Ok(Some(vec)) => cosine_similarity(question_vec, &vec),
            Ok(None) => -1.0,
            Err(e) => {
                tracing::debug!("no datasource-level embedding for {datasource_id}: {e}");
                -1.0
            }
        };

        // ── Table + Column-level scores ───────────────────────────────────
        let all_embeddings = self.store.get_for_datasource(datasource_id)?;

        if all_embeddings.is_empty() {
            return Ok(CandidateScore {
                datasource_id: datasource_id.to_owned(),
                ds_sim,
                table_sim: 0.0,
                col_sim: 0.0,
                fused: ((ds_sim + 1.0) * 0.5).max(0.0),
                matched_tables: Vec::new(),
            });
        }

        let (table_best_sim, col_sim, matched) =
            self.score_tables_columns(question_vec, &all_embeddings);

        // ── Weighted fusion ────────────────────────────────────────────────
        // Datasource: 25%, Table: 35%, Column: 40%
        let fused = (ds_sim + 1.0) * 0.125_f32
            + (table_best_sim + 1.0) * 0.175_f32
            + (col_sim + 1.0) * 0.200_f32;

        Ok(CandidateScore {
            datasource_id: datasource_id.to_owned(),
            ds_sim,
            table_sim: table_best_sim,
            col_sim,
            fused: fused.clamp(0.0, 1.0),
            matched_tables: matched,
        })
    }

    /// Score all tables/columns within a datasource against the question.
    /// Returns (best_table_sim, best_col_sim, matched_tables).
    fn score_tables_columns(
        &self,
        question_vec: &[f32],
        columns: &[(String, String, Arc<Vec<f32>>)],
    ) -> (f32, f32, Vec<MatchedTable>) {
        // Aggregate: per-table best column score
        let mut table_scores: std::collections::BTreeMap<String, (f32, String)> =
            std::collections::BTreeMap::new();

        for (table_name, column_name, col_vec) in columns {
            let sim = cosine_similarity(question_vec, col_vec.as_ref());
            let entry = table_scores
                .entry(table_name.clone())
                .or_insert_with(|| (sim, column_name.clone()));

            if sim > entry.0 {
                *entry = (sim, column_name.clone());
            }
        }

        // Sort by best column similarity
        let mut sorted: Vec<_> = table_scores.into_iter().collect();
        sorted.sort_by(|a, b| {
            b.1 .0
                .partial_cmp(&a.1 .0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let top_tables: Vec<_> = sorted.iter().take(TOP_K_TABLES).cloned().collect();
        let max_col_sim = top_tables.first().map(|(_, (s, _))| *s).unwrap_or(-1.0);
        let max_table_sim = max_col_sim;

        // Column description is populated by the route handler from nl2sql_table_semantics
        let matched: Vec<MatchedTable> = top_tables
            .into_iter()
            .map(|(table_name, (best_col_sim, best_col))| MatchedTable {
                table_name,
                best_column: best_col,
                column_description: String::new(),
                similarity_score: (best_col_sim + 1.0) / 2.0,
            })
            .collect();

        (max_table_sim, max_col_sim, matched)
    }

    /// Generate embedding for a single text.
    /// `api_key` overrides the static key set at construction time (used for per-tenant keys).
    async fn embed_single(&self, text: &str, api_key: Option<String>) -> anyhow::Result<Vec<f32>> {
        let model = crate::nl2sql::embedding::EmbeddingModel::new(
            &self.embed_model,
            self.embed_url.clone(),
            api_key.or_else(|| self.embed_api_key.clone()),
        );
        let results = model.embed_batch(&[text.to_owned()]).await?;
        Ok(results.into_iter().next().unwrap_or_default())
    }
}

// ── Hybrid LLM routing ─────────────────────────────────────────────────────────

/// Default cosine-similarity threshold for datasource-level embedding pre-filter.
/// Datasources below this threshold are excluded before coarse table search.
fn datasource_embed_threshold() -> f32 {
    std::env::var("NL2SQL_DS_EMBED_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.3)
}

/// Filter datasources by embedding similarity — pre-filter before coarse table search.
///
/// Computes cosine similarity between the question embedding and each datasource's
/// stored AI description embedding. Skips datasources whose similarity falls below
/// `NL2SQL_DS_EMBED_THRESHOLD` (default 0.3), reducing coarse search cost and
/// improving routing precision on multi-datasource deployments.
pub fn filter_datasources_by_embedding(
    store: &Arc<EmbeddingStore>,
    question_vec: &[f32],
    available_sources: &[(String, String, String)],
) -> Vec<String> {
    use crate::nl2sql::embedding::cosine_similarity;

    let threshold = datasource_embed_threshold();
    let has_any_datasource_embeddings = available_sources.iter().any(|(ds_id, _, _)| {
        store
            .get_typed(ds_id, "__datasource__", "__datasource__", "datasource")
            .ok()
            .flatten()
            .is_some()
    });

    if !has_any_datasource_embeddings {
        tracing::debug!(
            total = available_sources.len(),
            "no datasource-level embeddings found; skipping datasource pre-filter"
        );
        return available_sources
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect();
    }

    let mut passed = Vec::new();

    for (ds_id, _, _) in available_sources {
        let sim = match store.get_typed(ds_id, "__datasource__", "__datasource__", "datasource") {
            Ok(Some(vec)) => cosine_similarity(question_vec, &vec),
            _ => -1.0,
        };

        if sim >= threshold {
            tracing::debug!(datasource_id = %ds_id, sim = sim, "datasource passed embedding pre-filter");
            passed.push(ds_id.clone());
        } else {
            tracing::debug!(datasource_id = %ds_id, sim = sim, threshold = threshold, "datasource filtered out by embedding pre-filter");
        }
    }

    // If all datasources are filtered out, fall back to including all (don't block routing)
    if passed.is_empty() && !available_sources.is_empty() {
        tracing::debug!("all datasources filtered out; falling back to include all");
        return available_sources
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect();
    }

    if !passed.is_empty() {
        tracing::debug!(passed = ?passed, "datasource embedding pre-filter passed datasource ids");
    }

    tracing::info!(
        passed = passed.len(),
        total = available_sources.len(),
        threshold = threshold,
        "datasource embedding pre-filter passed {} of {} datasources",
        passed.len(),
        available_sources.len()
    );

    passed
}

/// Boost tables by historical statistics (row_count, size_bytes).
///
/// Reads `nl2sql_table_stats.row_count` and `nl2sql_table_stats.size_bytes`
/// to generate a popularity signal: `log(1 + row_count) / log(1 + max_row_count)`.
/// The signal is normalised per-datasource so a large table and small table
/// within the same datasource are comparable.
pub async fn stats_boost(
    datasource_ids: &[String],
    db: &sqlx::SqlitePool,
) -> anyhow::Result<std::collections::HashMap<String, f32>> {
    if datasource_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let placeholders: String = datasource_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        r#"
        SELECT datasource_id, table_name, row_count, size_bytes
        FROM nl2sql_table_stats
        WHERE datasource_id IN ({})
          AND deleted_at IS NULL
        ORDER BY row_count DESC
        LIMIT 200
        "#,
        placeholders
    );

    let mut q = sqlx::query_as::<_, (String, String, u64, u64)>(sqlx::AssertSqlSafe(query));
    for id in datasource_ids {
        q = q.bind(id);
    }
    let rows: Vec<(String, String, u64, u64)> = q.fetch_all(db).await?;

    // Find per-datasource max row_count for normalisation
    let mut per_ds_max_row: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    for (ds_id, _, row_count, _) in &rows {
        per_ds_max_row
            .entry(ds_id.clone())
            .and_modify(|e| *e = (*e).max(*row_count))
            .or_insert(*row_count);
    }

    let mut scores: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    for (ds_id, table_name, row_count, _) in &rows {
        let max_row = per_ds_max_row.get(ds_id).copied().unwrap_or(1).max(1) as f32;
        let score = (1.0_f32 + (*row_count as f32)).ln() / (1.0_f32 + max_row).ln().max(0.001);
        scores.insert(format!("{}:{}", ds_id, table_name), score.min(1.0));
    }

    Ok(scores)
}

/// Embedding-based coarse search: global Top-K tables across ALL datasources.
/// Used as the first phase of `route_hybrid`.
///
/// Optionally filters datasources by embedding similarity before the table-level search,
/// if `NL2SQL_DS_EMBED_PRE_FILTER` is true (default: true).
///
/// Returns (matches, filtered_datasource_ids) so callers can reuse the datasource filter.
pub async fn global_coarse_search(
    store: &Arc<EmbeddingStore>,
    question: &str,
    question_vec: Option<&[f32]>,
    embed_api_key: Option<String>,
    use_ann_index: bool,
    available_sources: Option<&[(String, String, String)]>,
) -> anyhow::Result<(
    Vec<GlobalTableMatch>,
    Option<std::collections::HashSet<String>>,
)> {
    let question_vec_owned;
    let question_vec = if let Some(vec) = question_vec {
        vec
    } else {
        let embed_model = store.embed_model();
        let embed_url = store.embed_url();
        let model =
            crate::nl2sql::embedding::EmbeddingModel::new(&embed_model, embed_url, embed_api_key);
        question_vec_owned = model
            .embed_batch(&[question.to_owned()])
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        &question_vec_owned
    };

    // Datasource-level embedding pre-filter: skip datasources with low similarity to question.
    // Default: true (P2-6 — enables embedding-level pre-filtering in the standard routing path).
    let mut allowed_ds: Option<std::collections::HashSet<String>> =
        if let Some(sources) = available_sources {
            let do_prefilter = std::env::var("NL2SQL_DS_EMBED_PRE_FILTER")
                .ok()
                .and_then(|v| v.parse::<bool>().ok())
                .unwrap_or(true); // was: false

            if do_prefilter {
                let passed_ds = filter_datasources_by_embedding(store, question_vec, sources);
                if !passed_ds.is_empty() && passed_ds.len() < sources.len() {
                    tracing::info!(
                        prefiltered = sources.len() - passed_ds.len(),
                        remaining = passed_ds.len(),
                        "datasource pre-filter applied"
                    );
                }
                Some(passed_ds.into_iter().collect())
            } else {
                None
            }
        } else {
            None
        };

    let mut matches = store.global_table_search(
        question_vec,
        top_k_tables_for_llm(),
        use_ann_index,
        allowed_ds.as_ref(),
    )?;

    // Enterprise-safe fallback:
    // datasource pre-filter is only a narrowing heuristic.
    // If it yields zero table candidates, we must continue with table-level search
    // across all datasources instead of terminating routing early.
    if matches.is_empty() && allowed_ds.is_some() {
        let fallback_matches =
            store.global_table_search(question_vec, top_k_tables_for_llm(), use_ann_index, None)?;
        if !fallback_matches.is_empty() {
            tracing::warn!(
                prefiltered_datasources = allowed_ds.as_ref().map(|s| s.len()).unwrap_or(0),
                fallback_table_candidates = fallback_matches.len(),
                "datasource pre-filter produced no table candidates; falling back to table-level search across all datasources"
            );
            matches = fallback_matches;
            // Disable datasource allowlist in downstream RRFS/LLM phases so table hits can proceed.
            allowed_ds = None;
        } else {
            tracing::info!(
                prefiltered_datasources = allowed_ds.as_ref().map(|s| s.len()).unwrap_or(0),
                "datasource pre-filter and global table fallback both produced no candidates"
            );
        }
    }

    tracing::info!(
        datasource_prefilter_enabled = allowed_ds.is_some(),
        datasource_prefilter_count = allowed_ds.as_ref().map(|s| s.len()).unwrap_or(0),
        table_candidate_count = matches.len(),
        "routing coarse search result summary"
    );

    Ok((matches, allowed_ds))
}

/// Build the LLM routing prompt for picking from Top-K candidate tables.
///
/// The LLM receives:
/// - User's question
/// - Top-K candidate tables (datasource, table name, best column, column sim)
/// - Available data source metadata (name, description)
///
/// The LLM outputs JSON selecting the best datasource, tables, and columns.
fn build_llm_routing_prompt(
    question: &str,
    candidates: &[GlobalTableMatch],
    available_sources: &[(String, String, String)],
    domain_context: Option<&str>,
) -> String {
    let candidate_lines: Vec<String> = candidates
        .iter()
        .map(|c| {
            let desc_suffix = if !c.column_description.is_empty() {
                format!(
                    " (desc: {})",
                    c.column_description.chars().take(80).collect::<String>()
                )
            } else {
                String::new()
            };
            format!(
                "  - ds={} table={} best_column={} column_sim={:.3}{}",
                c.datasource_id, c.table_name, c.best_column, c.column_sim, desc_suffix
            )
        })
        .collect();
    let source_lines: Vec<String> = available_sources
        .iter()
        .map(|(id, name, desc)| {
            format!(
                "  - datasource_id={} name={} description={}",
                id,
                name,
                desc.chars().take(200).collect::<String>()
            )
        })
        .collect();

    let domain_section = if let Some(ctx) = domain_context {
        format!(
            "\nBusiness Domain Context (use this to narrow candidate tables):\n{}\n",
            ctx
        )
    } else {
        String::new()
    };

    let prompt = format!(
        r#"You are a NL2SQL routing expert. Given a user's question and a list of candidate tables ranked by embedding similarity, select the best data source and tables.

Respond with ONLY valid JSON (no markdown, no explanation):
{{
  "selected_datasource_id": "datasource_id string or null if multi-datasource",
  "additional_datasources": ["datasource_id2", ...],
  "confidence": 0.0-1.0,
  "reason": "brief explanation of the choice",
  "needs_clarification": false,
  "clarification_question": null,
  "selected_tables": [
    {{
      "datasource_id": "datasource_id string",
      "table_name": "string",
      "reason": "why this table is selected",
      "columns": ["relevant column names"]
    }}
  ]
}}

User question: {question}{domain_section}
Candidate tables (ranked by embedding similarity, top={top_k}):
{candidate_lines}

Available data sources:
{source_lines}

Rules:
- If the question mentions a domain matching exactly ONE table → confidence >= 0.75, single datasource
- If the question requires data from MULTIPLE datasources (e.g. "compare orders from ERP with customers from CRM") → set selected_datasource_id to the primary one, add others to additional_datasources, set confidence >= 0.70
- If the question could match MULTIPLE tables (e.g. "订单" matches both "orders" and "order_items") → needs_clarification=true, set clarification_question
- If the question is vague ("show me data", "sales info") → needs_clarification=true
- If confidence < 0.45 → fallback to best embedding match
- Use the column_sim scores as context, not as the final word
- Always include datasource_id for each selected table"#,
        question = question,
        domain_section = domain_section,
        top_k = candidates.len(),
        candidate_lines = candidate_lines.join("\n"),
        source_lines = source_lines.join("\n"),
    );
    prompt
}

/// Parse the LLM routing response. Returns Ok(None) if parsing fails.
fn parse_llm_routing_response(text: &str) -> Option<LlmRoutingResponse> {
    // Strip markdown code fences if present
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(text);
    let text = text.trim();

    serde_json::from_str::<LlmRoutingResponse>(text).ok()
}

#[derive(serde::Deserialize)]
struct LlmRoutingResponse {
    /// Primary datasource (None if cross-datasource).
    #[serde(rename = "selected_datasource_id")]
    selected_datasource_id: Option<String>,
    /// Additional datasources needed when cross_datasource = true.
    #[serde(rename = "additional_datasources", default)]
    additional_datasources: Vec<String>,
    confidence: f32,
    reason: String,
    #[serde(rename = "needs_clarification")]
    needs_clarification: bool,
    #[serde(rename = "clarification_question")]
    clarification_question: Option<String>,
    #[serde(rename = "selected_tables")]
    selected_tables: Vec<LlmSelectedTable>,
}

#[derive(serde::Deserialize)]
struct LlmSelectedTable {
    /// Datasource this table belongs to.
    #[serde(rename = "datasource_id", default)]
    datasource_id: Option<String>,
    #[serde(rename = "table_name")]
    table_name: String,
    #[allow(dead_code)]
    reason: String,
    columns: Vec<String>,
}

/// Resolve the LLM client and model name for routing using the per-tenant config registry.
/// This mirrors `resolve_chat_config` from nl2sql/mod.rs.
pub async fn resolve_routing_llm(
    config_registry: &Option<Arc<agent_gateway::TenantConfigRegistry>>,
    tenant_id: &str,
    user_id: &str,
    default_model: &str,
) -> anyhow::Result<(
    crate::governed_provider::GovernedProviderClient,
    String,
    RoutingLlmMeta,
)> {
    let registry = match config_registry.as_ref() {
        Some(r) => r.as_ref(),
        None => anyhow::bail!(
            "no tenant config registry is available for durable routing-model dispatch"
        ),
    };
    let runtime_config = registry
        .load_user_config(tenant_id, user_id, None)
        .await
        .map_err(|e| anyhow::anyhow!("failed to load config for tenant {}: {}", tenant_id, e))?;

    if !runtime_config.api_keys.is_empty() {
        for entry in &runtime_config.api_keys {
            let model_fallback = default_model.to_string();
            let effective_model = entry.model.as_ref().unwrap_or(&model_fallback);
            match api::build_provider(
                &entry.provider,
                effective_model,
                &entry.key,
                entry.base_url.as_deref(),
            ) {
                Ok(client) => {
                    tracing::info!(
                        tenant_id = %tenant_id,
                        key_id = %entry.id,
                        model = %effective_model,
                        "routing: using DB chat key"
                    );
                    let meta = RoutingLlmMeta {
                        provider: entry.provider.clone(),
                        key_id: Some(entry.id.clone()),
                        masked_api_key: Some(mask_api_key(&entry.key)),
                        base_url: client.base_url().to_string(),
                    };
                    return Ok((
                        crate::governed_provider::GovernedProviderClient::new(
                            client,
                            registry.database(),
                            tenant_id,
                            user_id,
                            "nl2sql:routing",
                        ),
                        effective_model.clone(),
                        meta,
                    ));
                }
                Err(e) => {
                    tracing::warn!(tenant_id = %tenant_id, key_id = %entry.id, error = %e, "routing LLM key failed, trying next");
                }
            }
        }
    }

    if !runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK") {
        anyhow::bail!("no approved tenant chat model is configured for NL2SQL routing");
    }
    // Explicit development compatibility fallback.
    api::ProviderClient::from_model(default_model)
        .map(|client| {
            let meta = RoutingLlmMeta {
                provider: "env-fallback".to_string(),
                key_id: None,
                masked_api_key: None,
                base_url: client.base_url().to_string(),
            };
            (
                crate::governed_provider::GovernedProviderClient::new(
                    client,
                    registry.database(),
                    tenant_id,
                    user_id,
                    "nl2sql:routing",
                ),
                default_model.to_string(),
                meta,
            )
        })
        .map_err(|e| anyhow::anyhow!("routing LLM env fallback failed: {}", e))
}

/// Hybrid routing: embedding coarse search + LLM tool-calling precision reranking.
///
/// Phase 1: Embedding global table search → Top-20 tables across ALL datasources
/// Phase 2: LLM tool-calling → pick best datasource, tables, columns from Top-20
/// Phase 3: Return RoutingResult or clarification
///
/// Returns the resolved RoutingResult or a clarification decision.
pub async fn route_hybrid(
    // Reserved for future use (e.g. direct column description lookups).
    #[allow(dead_code)] _store: &Arc<EmbeddingStore>,
    question: &str,
    candidates: Vec<GlobalTableMatch>,
    available_sources: &[(String, String, String)],
    llm_client: &crate::governed_provider::GovernedProviderClient,
    llm_model: &str,
    llm_meta: &RoutingLlmMeta,
    col_descriptions: Option<&std::collections::HashMap<(String, String), String>>,
    domain_context: Option<&str>,
) -> anyhow::Result<LlmRoutingDecision> {
    let prompt = build_llm_routing_prompt(question, &candidates, available_sources, domain_context);
    let timeout_secs = routing_llm_timeout_secs();

    let request = api::MessageRequest {
        model: llm_model.to_string(),
        max_tokens: 1024,
        messages: vec![api::InputMessage {
            role: "user".to_string(),
            content: vec![api::InputContentBlock::Text { text: prompt }],
        }],
        system: Some("You are a precise NL2SQL routing assistant. Your job is to select the correct data source and tables based on a user's natural language question. Respond with ONLY valid JSON.".to_string()),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.1),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        llm_client.send_message(&request),
    )
    .await
    {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => return Err(anyhow::anyhow!("LLM routing call failed: {}", e)),
        Err(_) => {
            tracing::warn!(
                timeout_secs,
                provider = %llm_meta.provider,
                model = %llm_model,
                base_url = %llm_meta.base_url,
                api_key_id = %llm_meta.key_id.as_deref().unwrap_or("env"),
                api_key = %llm_meta.masked_api_key.as_deref().unwrap_or("env"),
                "LLM routing call timed out; falling back to best embedding match"
            );
            emit_route_stage(
                "manual_continue",
                "LLM 路由超时，已回退到 embedding 路由",
                None,
                None,
                Some("embedding-fallback".to_string()),
            );
            return Ok(fallback_to_best_embedding(&candidates, available_sources));
        }
    };

    let text_content = response
        .content
        .iter()
        .find_map(|block| match block {
            api::OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let parsed = parse_llm_routing_response(&text_content);

    let Some(parsed) = parsed else {
        tracing::warn!(
            "failed to parse LLM routing response: {}",
            text_content.chars().take(200).collect::<String>()
        );
        // Fall back to best embedding match
        return Ok(fallback_to_best_embedding(&candidates, available_sources));
    };

    if parsed.needs_clarification {
        let options: Vec<ClarificationOption> = candidates
            .iter()
            .enumerate()
            .take(5)
            .map(|(i, c)| ClarificationOption {
                option_index: i,
                data_source_id: c.datasource_id.clone(),
                table_name: c.table_name.clone(),
                column_name: c.best_column.clone(),
                reason: format!(
                    "相似度 {:.2}，最佳匹配列: {}",
                    (c.column_sim + 1.0) / 2.0,
                    c.best_column
                ),
                sim_score: c.column_sim,
                business_meaning: col_descriptions
                    .and_then(|m| m.get(&(c.table_name.clone(), c.best_column.clone())))
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();

        return Ok(LlmRoutingDecision::NeedsClarification {
            clarification_question: parsed
                .clarification_question
                .unwrap_or_else(|| "您指的是哪一个数据？".to_string()),
            options,
            domain_context: domain_context.map(String::from),
        });
    }

    if parsed.confidence >= LLM_HIGH_CONFIDENCE {
        // Check if the LLM detected a cross-datasource query:
        // Tables span multiple datasources, or additional_datasources is non-empty.
        let all_ds_ids: std::collections::HashSet<String> = parsed
            .selected_tables
            .iter()
            .filter_map(|t| t.datasource_id.clone())
            .chain(parsed.additional_datasources.iter().cloned())
            .collect();

        let primary_ds = parsed
            .selected_datasource_id
            .as_ref()
            .or_else(|| {
                parsed
                    .selected_tables
                    .first()
                    .and_then(|t| t.datasource_id.as_ref())
            })
            .cloned()
            .unwrap_or_default();

        if all_ds_ids.len() > 1 || parsed.additional_datasources.len() > 1 {
            // Cross-datasource: group tables by datasource and return CrossDatasource decision.
            let mut ds_to_tables: std::collections::BTreeMap<String, Vec<MatchedTable>> =
                std::collections::BTreeMap::new();
            for table in &parsed.selected_tables {
                let ds_id = table.datasource_id.as_ref().unwrap_or(&primary_ds).clone();
                ds_to_tables.entry(ds_id).or_default().push(MatchedTable {
                    table_name: table.table_name.clone(),
                    best_column: table.columns.first().cloned().unwrap_or_default(),
                    column_description: String::new(),
                    similarity_score: parsed.confidence,
                });
            }

            let mut results: Vec<RoutingResult> = Vec::new();
            for (ds_id, matched_tables) in ds_to_tables {
                results.push(RoutingResult {
                    data_source_id: ds_id,
                    matched_tables,
                    confidence: parsed.confidence,
                    method: RoutingMethod::LLM,
                });
            }

            return Ok(LlmRoutingDecision::CrossDatasource {
                datasources: results,
                reason: parsed.reason,
            });
        }

        // Single-datasource: proceed as normal.
        let matched_tables: Vec<MatchedTable> = parsed
            .selected_tables
            .iter()
            .map(|t| MatchedTable {
                table_name: t.table_name.clone(),
                best_column: t.columns.first().cloned().unwrap_or_default(),
                column_description: String::new(),
                similarity_score: parsed.confidence,
            })
            .collect();

        return Ok(LlmRoutingDecision::HighConfidence(RoutingResult {
            data_source_id: primary_ds,
            matched_tables,
            confidence: parsed.confidence,
            method: RoutingMethod::LLM,
        }));
    }

    // Low confidence: fall back to best embedding match
    Ok(fallback_to_best_embedding(&candidates, available_sources))
}

/// Fall back to the best embedding match when LLM confidence is low.
fn fallback_to_best_embedding(
    candidates: &[GlobalTableMatch],
    available_sources: &[(String, String, String)],
) -> LlmRoutingDecision {
    let Some(best) = candidates.first() else {
        return LlmRoutingDecision::LowConfidence(RoutingResult {
            data_source_id: String::new(),
            matched_tables: vec![],
            confidence: 0.0,
            method: RoutingMethod::Auto,
        });
    };

    let _ds_desc = available_sources
        .iter()
        .find(|(id, _, _)| id == &best.datasource_id)
        .map(|(_, _, desc)| desc.clone())
        .unwrap_or_default();

    let mut matches = vec![];
    for c in candidates.iter().take(TOP_K_TABLES) {
        if c.datasource_id == best.datasource_id {
            matches.push(MatchedTable {
                table_name: c.table_name.clone(),
                best_column: c.best_column.clone(),
                column_description: String::new(),
                similarity_score: (c.column_sim + 1.0) / 2.0,
            });
        }
    }

    LlmRoutingDecision::LowConfidence(RoutingResult {
        data_source_id: best.datasource_id.clone(),
        matched_tables: matches,
        confidence: (best.column_sim + 1.0) / 2.0,
        method: RoutingMethod::Auto,
    })
}

pub async fn expand_question_with_synonyms(
    question: &str,
    datasource_id: &str,
    tenant_id: &str,
    db: &sqlx::SqlitePool,
) -> anyhow::Result<String> {
    #[derive(Debug, sqlx::FromRow)]
    struct SynonymRow {
        term: String,
        canonical_column: String,
    }

    let rows: Vec<SynonymRow> = sqlx::query_as(
        "SELECT term, canonical_column FROM nl2sql_synonyms \
         WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if rows.is_empty() {
        return Ok(question.to_string());
    }

    // Sort by term length descending — replace longer terms first to avoid partial matches.
    let mut terms: Vec<(String, String)> = rows
        .into_iter()
        .map(|r| (r.term, r.canonical_column))
        .collect();
    terms.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut expanded = question.to_string();
    for (term, canonical) in &terms {
        // Case-insensitive replacement.
        let re = regex::Regex::new(&format!("(?i){}", regex::escape(term))).ok();
        if let Some(re) = re {
            let replacement = format!("{} ({})", canonical, term);
            expanded = re.replace_all(&expanded, replacement.as_str()).to_string();
        }
    }

    if expanded != question {
        tracing::debug!(
            original_chars = question.chars().count(),
            expanded_chars = expanded.chars().count(),
            "P1-1: question expanded with {} synonym(s)",
            terms.len()
        );
    }

    Ok(expanded)
}

// ── RRFS Multi-Path Fusion Routing ─────────────────────────────────────────────

/// RRFS (Reciprocal Rank Fusion) — combines multiple retrieval paths into a unified ranking.
///
/// Five parallel retrieval paths:
///   1. Embedding similarity (vector cosine) — coarse semantic match
///   2. Text match (BM25-like token overlap) — exact keyword match
///   3. Frequency boost — query history popularity
///   4. Stats boost — row_count/size_bytes signal
///   5. Business domain boost — P0-3 domain match bonus
///
/// RRF formula: score = Σ 1/(k + rank_i), k=60
///
/// Returns tables sorted by fused RRF score, along with per-path signals.
pub async fn route_rrfs(
    question: &str,
    store: &Arc<EmbeddingStore>,
    question_vec: Option<&[f32]>,
    prefetched_embed_matches: Option<&[GlobalTableMatch]>,
    datasource_ids: &[String],
    embed_api_key: Option<String>,
    use_ann_index: bool,
    db: &sqlx::SqlitePool,
    allowed_datasources: Option<&std::collections::HashSet<String>>,
    #[allow(dead_code)] domain_table_filter: Option<
        &std::collections::HashMap<(String, String), f32>,
    >,
    // P1-1: Synonym terms to include in BM25 text matching.
    synonym_terms: Option<&[String]>,
) -> anyhow::Result<Vec<RRFSMatch>> {
    let question_lower = question.to_lowercase();
    let question_tokens: Vec<&str> = question_lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() > 1)
        .collect();

    // ── Path 1: Embedding similarity ────────────────────────────────────────
    let embed_matches = if let Some(prefetched) = prefetched_embed_matches {
        prefetched.to_vec()
    } else if let Some(question_vec) = question_vec {
        store.global_table_search(
            question_vec,
            top_k_tables_for_llm(),
            use_ann_index,
            allowed_datasources,
        )?
    } else {
        store
            .global_table_search_top_k(question, embed_api_key, use_ann_index, allowed_datasources)
            .await?
    };

    // RRFS incremental short-circuit:
    // if embedding path already has a dominant winner, skip BM25/frequency/stats DB reads.
    // This preserves quality by requiring a strict confidence + margin condition.
    let rrfs_short_circuit = std::env::var("NL2SQL_RRFS_SHORT_CIRCUIT")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(true);
    if rrfs_short_circuit && !embed_matches.is_empty() {
        let mut ranked = embed_matches.clone();
        ranked.sort_by(|a, b| {
            b.candidate_score
                .partial_cmp(&a.candidate_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let top = &ranked[0];
        let second_score = ranked.get(1).map(|m| m.candidate_score).unwrap_or(0.0_f32);
        let margin = top.candidate_score - second_score;
        let top_ds = top.datasource_id.clone();
        let top3_same_ds = ranked.iter().take(3).all(|m| m.datasource_id == top_ds);

        // Conservative gate: only bypass fusion when winner is obviously dominant.
        if ranked.len() <= 8 && top.candidate_score >= 0.86 && margin >= 0.12 && top3_same_ds {
            tracing::info!(
                top_table = %top.table_name,
                top_datasource = %top.datasource_id,
                top_score = top.candidate_score,
                margin = margin,
                candidate_count = ranked.len(),
                "route_rrfs short-circuit: embedding winner is dominant; skip bm25/frequency/stats paths"
            );
            let mut compact = ranked
                .into_iter()
                .take(20)
                .map(|m| RRFSMatch {
                    table_name: m.table_name,
                    rrf_score: m.candidate_score,
                    embed_similarity: m.column_sim,
                    bm25_score: 0.0,
                    frequency_score: 0.0,
                    stats_score: 0.0,
                    domain_boost: 0.0,
                    datasource_id: m.datasource_id,
                })
                .collect::<Vec<_>>();
            compact.sort_by(|a, b| {
                b.rrf_score
                    .partial_cmp(&a.rrf_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            return Ok(compact);
        }
    }

    // ── Path 2: Text match (BM25-like) ───────────────────────────────────
    let bm25_scores = text_match(
        question_tokens,
        datasource_ids,
        db,
        synonym_terms.unwrap_or(&[]),
    )
    .await?;

    // ── Path 3: Frequency boost ────────────────────────────────────────────
    let freq_scores = frequency_boost(datasource_ids, db).await?;

    // ── Path 4: Stats-based boost ─────────────────────────────────────────
    let stats_scores = stats_boost(datasource_ids, db).await?;

    // B-13: Make RRF K configurable via environment variable. Default 60.0 is standard.
    let k_value: f32 = std::env::var("NL2SQL_RRFS_K")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(60.0);

    let mut table_scores: std::collections::HashMap<String, f32> = std::collections::HashMap::new();

    // Embedding rank (sorted descending by similarity)
    let mut sorted_embed: Vec<_> = embed_matches.iter().enumerate().collect();
    sorted_embed.sort_by(|(_, a), (_, b)| {
        b.column_sim
            .partial_cmp(&a.column_sim)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (rank, m) in sorted_embed {
        let entry = table_scores.entry(m.table_name.clone()).or_insert(0.0);
        *entry += 1.0 / (k_value + rank as f32);
    }

    // BM25 rank
    let mut sorted_bm25: Vec<_> = bm25_scores.iter().collect();
    sorted_bm25.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    for (rank, (table_name, _)) in sorted_bm25.into_iter().enumerate() {
        let entry = table_scores.entry(table_name.clone()).or_insert(0.0);
        *entry += 1.0 / (k_value + rank as f32);
    }

    // Frequency rank
    let mut sorted_freq: Vec<_> = freq_scores.iter().collect();
    sorted_freq.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    for (rank, (table_name, _)) in sorted_freq.into_iter().enumerate() {
        let entry = table_scores.entry(table_name.clone()).or_insert(0.0);
        *entry += 1.0 / (k_value + rank as f32);
    }

    // Stats rank (row_count / size_bytes signal)
    let mut sorted_stats: Vec<_> = stats_scores.iter().collect();
    sorted_stats.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    for (rank, (table_name, _)) in sorted_stats.into_iter().enumerate() {
        let entry = table_scores.entry(table_name.clone()).or_insert(0.0);
        *entry += 1.0 / (k_value + rank as f32);
    }

    // ── Path 5: Business Domain boost (P0-3) ──────────────────────────────
    // Tables in the detected business domain get an additional boost score.
    if let Some(domain_filter) = domain_table_filter {
        for ((_ds_id, table_name), boost) in domain_filter.iter() {
            if *boost <= 0.0 {
                continue;
            }
            // Only boost if this table appears in embed_matches
            if embed_matches.iter().any(|m| &m.table_name == table_name) {
                let entry = table_scores.entry(table_name.clone()).or_insert(0.0);
                // Scale boost: domain boost contributes a fraction of a top-rank position
                *entry += boost * (1.0 / k_value) * 0.5;
            }
        }
    }

    // Sort by fused score and return top 20
    let mut results: Vec<_> = table_scores.into_iter().collect();
    results.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(20);

    // Apply minimum embedding similarity filter after fusion.
    // Even after RRF fusion, a table that ranks high across BM25/frequency signals
    // could pollute results if its embedding similarity is near zero. We require
    // a minimum cosine similarity to participate in the final result set.
    let min_sim = crate::nl2sql::min_table_sim_threshold();
    results.retain(|(table_name, _)| {
        embed_matches
            .iter()
            .find(|m| &m.table_name == table_name)
            .map(|m| m.column_sim >= min_sim)
            .unwrap_or(false)
    });

    tracing::debug!(
        total_embed_candidates = embed_matches.len(),
        rrfs_candidates_after_filter = results.len(),
        min_table_sim = min_sim,
        "route_rrfs: candidate counts after embedding floor"
    );

    // Enrich with metadata
    let mut enriched = Vec::new();
    for (table_name, rrf_score) in results {
        // Look up embed metadata once per table
        let embed_meta = embed_matches.iter().find(|m| &m.table_name == &table_name);
        let embed_sim = embed_meta.map(|m| m.column_sim).unwrap_or(0.0);
        let bm25_sim = bm25_scores.get(&table_name).copied().unwrap_or(0.0);
        let freq_score = freq_scores.get(&table_name).copied().unwrap_or(0.0);
        let stats_score = stats_scores.get(&table_name).copied().unwrap_or(0.0);
        let domain_boost = domain_table_filter
            .and_then(|f| {
                f.get(&(
                    embed_meta
                        .map(|m| m.datasource_id.clone())
                        .unwrap_or_default(),
                    table_name.clone(),
                ))
            })
            .copied()
            .unwrap_or(0.0);

        enriched.push(RRFSMatch {
            table_name,
            rrf_score,
            embed_similarity: embed_sim,
            bm25_score: bm25_sim,
            frequency_score: freq_score,
            stats_score,
            domain_boost,
            datasource_id: embed_meta
                .map(|m| m.datasource_id.clone())
                .unwrap_or_default(),
        });
    }

    if !enriched.is_empty() {
        tracing::debug!(
            candidate_count = enriched.len(),
            top_rrf_score = enriched.first().map(|item| item.rrf_score),
            "route_rrfs produced ranked candidates"
        );
    } else {
        tracing::warn!("route_rrfs produced no candidates after fusion and filters");
    }

    Ok(enriched)
}

/// BM25-like token overlap scoring.
/// P1-1: Also accepts synonym terms loaded from `nl2sql_synonyms` and matches them
/// against the `ai_description` field alongside the original question tokens.
async fn text_match(
    question_tokens: Vec<&str>,
    datasource_ids: &[String],
    db: &sqlx::SqlitePool,
    synonym_terms: &[String],
) -> anyhow::Result<std::collections::HashMap<String, f32>> {
    if datasource_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    // P1-1: Merge question tokens with synonym terms for broader BM25 coverage.
    let all_tokens: Vec<&str> = question_tokens
        .iter()
        .map(|s| *s)
        .chain(synonym_terms.iter().map(|s| s.as_str()))
        .collect();

    if all_tokens.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let placeholders: String = datasource_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        r#"
        SELECT table_name,
               CAST(SUM(CASE
                   WHEN column_name = 'table_name' THEN 3.0
                   WHEN column_name = 'table_description' THEN 2.0
                   ELSE 1.0
               END) AS DOUBLE) AS score
        FROM (
            SELECT table_name, 'table_description' AS column_name FROM nl2sql_table_desc_semantics
            WHERE datasource_id IN ({}) AND deleted_at IS NULL AND ai_description LIKE ?
            UNION ALL
            SELECT table_name, 'table_name' AS column_name FROM nl2sql_table_desc_semantics
            WHERE datasource_id IN ({}) AND deleted_at IS NULL
        ) t
        GROUP BY table_name
        ORDER BY score DESC
        LIMIT 50
        "#,
        placeholders, placeholders
    );

    // B-03: Escape LIKE wildcards in tokens before building the pattern.
    // Order matters: escape backslashes first, then underscore, then percent.
    let escaped_tokens: Vec<String> = all_tokens
        .iter()
        .map(|t| {
            t.replace('\\', "\\\\")
                .replace('_', "\\_")
                .replace('%', "\\%")
        })
        .collect();
    let pattern = format!("%{}%", escaped_tokens.join("%"));
    // Decode the aggregate as f64 and narrow it to f32 only after normalization.
    let mut q = sqlx::query_as::<_, (String, f64)>(sqlx::AssertSqlSafe(query));
    for id in datasource_ids {
        q = q.bind(id);
    }
    q = q.bind(&pattern);
    for id in datasource_ids {
        q = q.bind(id);
    }
    let rows: Vec<(String, f64)> = q.fetch_all(db).await?;

    let total: f64 = rows.iter().map(|(_, s)| *s).sum::<f64>().max(1.0);
    let mut scores = std::collections::HashMap::new();
    for (table, s) in rows {
        scores.insert(table, (s / total) as f32);
    }
    Ok(scores)
}

/// Boost tables by historical query frequency.
/// Tables that have been queried successfully get a higher base score.
async fn frequency_boost(
    datasource_ids: &[String],
    db: &sqlx::SqlitePool,
) -> anyhow::Result<std::collections::HashMap<String, f32>> {
    if datasource_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let placeholders: String = datasource_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        r#"
        SELECT table_name, query_count
        FROM nl2sql_table_routing_features
        WHERE datasource_id IN ({})
          AND last_query_at > datetime(CURRENT_TIMESTAMP, '-90 days')
          AND deleted_at IS NULL
        ORDER BY query_count DESC
        LIMIT 100
        "#,
        placeholders
    );

    let mut q = sqlx::query_as::<_, (String, u64)>(sqlx::AssertSqlSafe(query));
    for id in datasource_ids {
        q = q.bind(id);
    }
    let rows: Vec<(String, u64)> = q.fetch_all(db).await?;

    let mut scores = std::collections::HashMap::new();
    for (table, count) in rows {
        // Normalized log-scale boost: higher query_count -> higher score.
        // Capped at ln(1+100) ≈ 4.6 so extreme popularity doesn't dominate.
        let capped_ln = (count as f32 + 1.0).ln().min(4.6);
        let freq_score = capped_ln / 4.6;
        scores.insert(table, freq_score);
    }
    Ok(scores)
}

/// Load business domains for a datasource from DB.
/// Pass `None` or an empty string to skip domain resolution.
pub async fn resolve_business_domains(
    db: &sqlx::SqlitePool,
    datasource_id: Option<&str>,
) -> anyhow::Result<Vec<BusinessDomain>> {
    let ds_id = match datasource_id {
        Some(id) if !id.is_empty() => id,
        _ => return Ok(Vec::new()),
    };
    let rows: Vec<(String, String, String, String, f64, String)> = sqlx::query_as(
        r#"
        SELECT d.datasource_id, d.domain_name, d.domain_description, COALESCE(GROUP_CONCAT(m.table_name), ''), d.confidence_score, d.domain_routing_mode
        FROM nl2sql_business_domains d
        LEFT JOIN nl2sql_table_domain_mapping m ON m.domain_id = d.id AND m.deleted_at IS NULL
        WHERE d.datasource_id = ? AND d.deleted_at IS NULL
        GROUP BY d.id
        ORDER BY d.table_count DESC
        "#,
    )
    .bind(ds_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                datasource_id,
                domain_name,
                domain_description,
                tables_str,
                confidence_score,
                routing_mode,
            )| {
                let confidence_score: f32 = confidence_score as f32;
                let tables: Vec<String> = tables_str
                    .split(',')
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                BusinessDomain {
                    datasource_id: Some(datasource_id),
                    domain_name,
                    domain_description,
                    tables,
                    confidence_score,
                    routing_mode: if routing_mode.eq_ignore_ascii_case("strict") {
                        "strict".to_string()
                    } else {
                        "assist".to_string()
                    },
                }
            },
        )
        .collect())
}

/// P0-2: Load all business domains across ALL datasources for a tenant.
/// Used as the input to `classify_question_to_domains` before routing decisions.
pub async fn resolve_all_business_domains_for_tenant(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
) -> anyhow::Result<Vec<BusinessDomain>> {
    let rows: Vec<(String, String, String, String, f64, String)> = sqlx::query_as(
        r#"
        SELECT d.datasource_id, d.domain_name, d.domain_description, COALESCE(GROUP_CONCAT(m.table_name), ''), d.confidence_score, d.domain_routing_mode
        FROM nl2sql_business_domains d
        LEFT JOIN nl2sql_table_domain_mapping m ON m.domain_id = d.id AND m.deleted_at IS NULL
        WHERE d.tenant_id = ? AND d.deleted_at IS NULL
        GROUP BY d.id
        ORDER BY d.confidence_score DESC
        "#,
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                datasource_id,
                domain_name,
                domain_description,
                tables_str,
                confidence_score,
                routing_mode,
            )| {
                let confidence_score: f32 = confidence_score as f32;
                let tables: Vec<String> = tables_str
                    .split(',')
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                BusinessDomain {
                    domain_name,
                    domain_description,
                    tables,
                    confidence_score,
                    datasource_id: Some(datasource_id),
                    routing_mode: if routing_mode.eq_ignore_ascii_case("strict") {
                        "strict".to_string()
                    } else {
                        "assist".to_string()
                    },
                }
            },
        )
        .collect())
}

/// P0-2: Classify a user question into relevant business domains using LLM.
///
/// Calls the LLM with the question and all known domains, asking it to score each
/// domain's relevance. Returns only domains with confidence > MIN_DOMAIN_CONFIDENCE (0.3).
///
/// Returns an empty vector when:
/// - No domains are registered
/// - LLM call fails or returns no confident matches (falls back to global search)
pub async fn classify_question_to_domains(
    question: &str,
    domains: &[BusinessDomain],
    llm_client: &crate::governed_provider::GovernedProviderClient,
    llm_model: &str,
    llm_meta: &RoutingLlmMeta,
) -> anyhow::Result<Vec<(String, f32)>> {
    if domains.is_empty() {
        return Ok(Vec::new());
    }

    const MIN_DOMAIN_CONFIDENCE: f32 = 0.3;
    const MAX_RETURNED: usize = 5;

    let domain_lines: Vec<String> = domains
        .iter()
        .map(|d| {
            let ds_suffix = d
                .datasource_id
                .as_ref()
                .map(|id| format!(" (datasource: {})", id))
                .unwrap_or_default();
            let table_list = if d.tables.len() <= 8 {
                d.tables.join(", ")
            } else {
                format!(
                    "{} ... ({} tables)",
                    d.tables[..6].join(", "),
                    d.tables.len()
                )
            };
            format!(
                "  - [{}] {}{}: {} | tables: {}",
                d.confidence_score, d.domain_name, ds_suffix, d.domain_description, table_list
            )
        })
        .collect();

    let prompt = format!(
        r#"Given a user's question, score how relevant each business domain is.
Return ONLY valid JSON array (no markdown, no explanation):
[{{ "domain": "domain_name", "confidence": 0.0-1.0 }}]

Only include domains with confidence >= {MIN_DOMAIN_CONFIDENCE}.
Return empty array if no domain is relevant.

User question: {question}

Available business domains:
{domain_lines}
"#,
        question = question,
        domain_lines = domain_lines.join("\n"),
    );

    let request = api::MessageRequest {
        model: llm_model.to_string(),
        max_tokens: 512,
        messages: vec![api::InputMessage {
            role: "user".to_string(),
            content: vec![api::InputContentBlock::Text { text: prompt }],
        }],
        system: Some(
            "You are a precise business domain classifier. Respond with ONLY valid JSON array."
                .to_string(),
        ),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.1),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let timeout_secs = routing_llm_timeout_secs();
    let response = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        llm_client.send_message(&request),
    )
    .await
    {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => return Err(anyhow::anyhow!("LLM domain classification failed: {}", e)),
        Err(_) => {
            tracing::warn!(
                timeout_secs,
                provider = %llm_meta.provider,
                model = %llm_model,
                base_url = %llm_meta.base_url,
                api_key_id = %llm_meta.key_id.as_deref().unwrap_or("env"),
                api_key = %llm_meta.masked_api_key.as_deref().unwrap_or("env"),
                "LLM domain classification timed out; falling back to static domain context"
            );
            return Ok(Vec::new());
        }
    };

    let text_content = response
        .content
        .iter()
        .find_map(|block| match block {
            api::OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    #[derive(serde::Deserialize)]
    struct DomainScore {
        domain: String,
        confidence: f32,
    }

    let scores: Vec<DomainScore> = serde_json::from_str(&text_content).unwrap_or_default();

    let mut results: Vec<(String, f32)> = scores
        .into_iter()
        .filter(|s| s.confidence >= MIN_DOMAIN_CONFIDENCE)
        .map(|s| (s.domain, s.confidence))
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(MAX_RETURNED);

    tracing::debug!(
        question_chars = question.chars().count(),
        domains_classified = results.len(),
        "P0-2: classify_question_to_domains returned {} relevant domains",
        results.len()
    );

    Ok(results)
}

/// Build domain context for LLM routing prompt.
pub fn build_domain_context(domains: &[BusinessDomain], _question: &str) -> String {
    if domains.is_empty() {
        return String::new();
    }

    let domain_lines: Vec<String> = domains
        .iter()
        .map(|d| {
            format!(
                "  - [{}] {}: {}",
                d.tables.len(),
                d.domain_name,
                if d.tables.len() <= 5 {
                    d.tables.join(", ")
                } else {
                    format!("{} ... (共{}张)", d.tables[..3].join(", "), d.tables.len())
                }
            )
        })
        .collect();

    format!(
        "\n\nBusiness Domains for this datasource:\n{}\n\nWhen routing, prefer tables from the domain that best matches the user's question.\n",
        domain_lines.join("\n")
    )
}

#[derive(Debug, Clone)]
pub struct RRFSMatch {
    pub table_name: String,
    pub rrf_score: f32,
    pub embed_similarity: f32,
    pub bm25_score: f32,
    pub frequency_score: f32,
    pub stats_score: f32,
    pub domain_boost: f32, // P0-3: business domain boost contribution
    pub datasource_id: String,
}

#[derive(Debug, Clone)]
pub struct BusinessDomain {
    pub domain_name: String,
    pub domain_description: String,
    pub tables: Vec<String>,
    /// LLM-assigned confidence from `classify_question_to_domains` (P0-2).
    /// DB-stored confidence is preserved in `resolve_business_domains`.
    pub confidence_score: f32,
    /// Which datasource this domain belongs to.
    /// `None` for domains loaded via `resolve_all_business_domains_for_tenant` (cross-ds).
    pub datasource_id: Option<String>,
    /// Per-domain routing policy.
    /// - assist: soft boost + prompt context
    /// - strict: hard table filtering for this domain when matched
    pub routing_mode: String,
}

#[derive(Debug)]
pub struct TableScoreDetail {
    pub table_name: String,
    pub datasource_level_sim: f32,
    pub table_level_sim: f32,
    pub column_level_sim: f32,
    pub fused_score: f32,
    pub best_column: String,
    pub column_description: String,
}

/// Score tables within a datasource with full 3-level breakdown.
/// Used by the route handler to return enriched MatchedTableInfo with descriptions.
#[allow(dead_code, clippy::unused_async)]
pub async fn score_tables_detailed(
    store: &EmbeddingStore,
    question_vec: &[f32],
    datasource_id: &str,
    col_descriptions: &std::collections::HashMap<(String, String), String>,
) -> anyhow::Result<Vec<TableScoreDetail>> {
    // Datasource-level
    let ds_sim = match store.get_typed(
        datasource_id,
        "__datasource__",
        "__datasource__",
        "datasource",
    ) {
        Ok(Some(vec)) => cosine_similarity(question_vec, &vec),
        _ => -1.0,
    };

    let all_embeddings = store.get_for_datasource(datasource_id)?;

    // Aggregate per table
    let mut table_map: std::collections::BTreeMap<String, (f32, String)> =
        std::collections::BTreeMap::new();

    for (table_name, column_name, col_vec) in &all_embeddings {
        let sim = cosine_similarity(question_vec, col_vec.as_ref());
        let entry = table_map
            .entry(table_name.clone())
            .or_insert_with(|| (sim, column_name.clone()));

        if sim > entry.0 {
            *entry = (sim, column_name.clone());
        }
    }

    let mut results: Vec<TableScoreDetail> = Vec::new();
    for (table_name, (best_col_sim, best_col)) in table_map.into_iter().take(TOP_K_TABLES) {
        let col_desc = col_descriptions
            .get(&(table_name.clone(), best_col.clone()))
            .cloned()
            .unwrap_or_default();

        // Table-level embedding
        let table_sim = match store.get_typed(datasource_id, &table_name, "__table__", "table") {
            Ok(Some(vec)) => cosine_similarity(question_vec, &vec),
            _ => best_col_sim,
        };

        let fused = (ds_sim + 1.0) * 0.125_f32
            + (table_sim + 1.0) * 0.175_f32
            + (best_col_sim + 1.0) * 0.200_f32;

        results.push(TableScoreDetail {
            table_name,
            datasource_level_sim: ds_sim,
            table_level_sim: table_sim,
            column_level_sim: best_col_sim,
            fused_score: fused.clamp(0.0, 1.0),
            best_column: best_col,
            column_description: col_desc,
        });
    }

    results.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(results)
}
