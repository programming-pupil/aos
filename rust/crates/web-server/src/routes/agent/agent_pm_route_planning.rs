use super::*;
pub(super) fn collect_pm_turn_domains(tool_calls: &[agent_gateway::ToolCallRecord]) -> Vec<String> {
    let mut out = Vec::new();
    for call in tool_calls {
        if let Some(url) = pick_primary_tool_url(call) {
            if let Some(domain) = extract_url_domain(&url) {
                if !domain.trim().is_empty() {
                    out.push(domain.to_ascii_lowercase());
                }
            }
        }
    }
    out
}

#[derive(Debug, Default, Clone)]
pub(super) struct PmDomainToolOutcome {
    pub(super) success_count: usize,
    pub(super) error_count: usize,
    pub(super) last_error_code: Option<String>,
    pub(super) last_error_message: Option<String>,
}

pub(super) fn collect_pm_domain_tool_outcomes(
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> HashMap<String, PmDomainToolOutcome> {
    let mut by_domain: HashMap<String, PmDomainToolOutcome> = HashMap::new();
    for call in tool_calls {
        let mut domains = Vec::new();
        if let Some(url) = pick_primary_tool_url(call) {
            if let Some(domain) = extract_url_domain(&url) {
                domains.push(domain.to_ascii_lowercase());
            }
        }
        if domains.is_empty() {
            let text = format!("{}\n{}", call.input, call.output);
            for url in extract_http_urls(&text) {
                if let Some(domain) = extract_url_domain(&url) {
                    domains.push(domain.to_ascii_lowercase());
                }
            }
        }
        domains.sort();
        domains.dedup();
        if domains.is_empty() {
            continue;
        }

        for domain in domains {
            let entry = by_domain.entry(domain).or_default();
            if call.is_error {
                entry.error_count = entry.error_count.saturating_add(1);
                if let Some(code) = classify_pm_tool_error_code(&call.output) {
                    entry.last_error_code = Some(code);
                }
                if !call.output.trim().is_empty() {
                    entry.last_error_message = Some(truncate_for_log(&call.output, 220));
                } else if !call.input.trim().is_empty() {
                    entry.last_error_message = Some(truncate_for_log(&call.input, 220));
                }
            } else {
                entry.success_count = entry.success_count.saturating_add(1);
            }
        }
    }
    by_domain
}

pub(super) fn blocked_domains_from_usage(
    domain_usage_counts: &HashMap<String, usize>,
    per_domain_quota: usize,
) -> Vec<String> {
    let mut blocked = domain_usage_counts
        .iter()
        .filter_map(|(domain, count)| {
            if *count >= per_domain_quota {
                Some(domain.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    blocked.sort();
    blocked
}

pub(super) fn merge_blocked_domains(
    mut blocked_domains: Vec<String>,
    additional_blocked_domains: &[String],
) -> Vec<String> {
    for domain in additional_blocked_domains {
        let normalized = domain.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            blocked_domains.push(normalized);
        }
    }
    blocked_domains.sort();
    blocked_domains.dedup();
    blocked_domains
}
pub(super) async fn load_pm_route_health_scores(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
) -> HashMap<String, PmRouteHealthSignal> {
    let rows = match sqlx::query(
        "SELECT route_key, COALESCE(channel, ''),
                CAST(COALESCE(score, success_rate, 0.0) AS DOUBLE),
                CAST(COALESCE(run_count, 0) AS INTEGER),
                CAST(COALESCE(failure_count, 0) AS INTEGER),
                CAST(0 AS INTEGER),
                CAST(COALESCE(avg_retrieve_duration_ms, 0.0) AS DOUBLE),
                NULL
         FROM pm_research_route_stats
         WHERE tenant_id = ?
         ORDER BY updated_at DESC
         LIMIT 120",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                "load_pm_route_health_scores failed: {}",
                error
            );
            return HashMap::new();
        }
    };

    let mut out = HashMap::<String, PmRouteHealthSignal>::new();
    for row in rows {
        let route_key = row.get::<String, _>(0);
        let channel = row.get::<String, _>(1);
        let health_score = row.get::<f64, _>(2).clamp(0.0, 1.0);
        let run_count = row.get::<i64, _>(3).max(0) as u64;
        let failure_count = row.get::<i64, _>(4).max(0) as u64;
        let timeout_count = row.get::<i64, _>(5).max(0) as u64;
        let avg_latency = row.get::<f64, _>(6);
        let last_error = row.get::<Option<String>, _>(7);
        let key = pm_route_health_key(&route_key, &channel);
        out.insert(
            key,
            PmRouteHealthSignal {
                health_score,
                run_count,
                failure_count,
                timeout_count,
                avg_latency_ms: if avg_latency > 0.0 {
                    Some(avg_latency)
                } else {
                    None
                },
                last_error_code: last_error,
            },
        );
    }
    out
}

pub(super) fn rank_pm_plan_routes(
    plan: &mut serde_json::Value,
    preflight: Option<&PmStartupPreflightOutcome>,
    learned_scores: &HashMap<String, f64>,
    route_health_scores: &HashMap<String, PmRouteHealthSignal>,
    user_question: &str,
) {
    let search_score = preflight.map(|x| x.search_channel_score()).unwrap_or(0.6);
    let browser_score = preflight.map(|x| x.browser_channel_score()).unwrap_or(0.6);
    rank_pm_plan_routes_with_scores(
        plan,
        search_score,
        browser_score,
        learned_scores,
        route_health_scores,
        user_question,
    );
}

pub(super) async fn load_pm_route_scores(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
) -> HashMap<String, f64> {
    fn push_weighted_score(
        acc: &mut HashMap<String, (f64, f64)>,
        route_key: &str,
        score: f64,
        weight: f64,
    ) {
        let route = route_key.trim();
        if route.is_empty() || weight <= 0.0 {
            return;
        }
        let entry = acc.entry(route.to_string()).or_insert((0.0, 0.0));
        entry.0 += score.clamp(0.0, 1.0) * weight;
        entry.1 += weight;
    }

    let mut blended_scores = HashMap::<String, (f64, f64)>::new();

    // Legacy route score table (historical strategy stats).
    match sqlx::query(
        "SELECT route_key, CAST(score AS DOUBLE)
         FROM pm_research_route_stats
         WHERE tenant_id = ?
         ORDER BY score DESC
         LIMIT 60",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            for row in rows {
                let route = row.get::<String, _>(0);
                let score = row.get::<f64, _>(1).clamp(0.0, 1.0);
                push_weighted_score(&mut blended_scores, &route, score, 0.55);
            }
        }
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                "load_pm_route_scores: pm_research_route_stats unavailable: {}",
                error
            );
        }
    }

    // Online learning features auto-written by main PM flows.
    match sqlx::query(
        "SELECT route_key,
                CAST(COALESCE(total_runs, 0) AS INTEGER),
                CAST(COALESCE(ema_success_rate, 0.0) AS DOUBLE),
                CAST(COALESCE(ema_quality, 0.0) AS DOUBLE),
                CAST(COALESCE(ema_latency_ms, 0.0) AS DOUBLE),
                CAST(COALESCE(ema_cost_usd, 0.0) AS DOUBLE)
         FROM pm_route_learning_features
         WHERE tenant_id = ?
         ORDER BY updated_at DESC
         LIMIT 120",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            for row in rows {
                let route = row.get::<String, _>(0);
                let runs = row.get::<i64, _>(1).max(0) as f64;
                let success_rate = row.get::<f64, _>(2).clamp(0.0, 1.0);
                let quality = row.get::<f64, _>(3).clamp(0.0, 1.0);
                let latency_ms = row.get::<f64, _>(4).max(0.0);
                let cost_usd = row.get::<f64, _>(5).max(0.0);

                let latency_score = if latency_ms > 0.0 {
                    (1.0 / (1.0 + latency_ms / 3500.0)).clamp(0.20, 1.0)
                } else {
                    0.5
                };
                let cost_score = if cost_usd > 0.0 {
                    (1.0 / (1.0 + cost_usd * 25.0)).clamp(0.20, 1.0)
                } else {
                    0.5
                };
                let learning_score = (success_rate * 0.50
                    + quality * 0.30
                    + latency_score * 0.12
                    + cost_score * 0.08)
                    .clamp(0.0, 1.0);

                let confidence = if runs >= 20.0 {
                    1.0
                } else if runs >= 5.0 {
                    0.70
                } else if runs >= 1.0 {
                    0.45
                } else {
                    0.25
                };
                push_weighted_score(
                    &mut blended_scores,
                    &route,
                    learning_score,
                    0.30 * confidence,
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                "load_pm_route_scores: pm_route_learning_features unavailable: {}",
                error
            );
        }
    }

    // Bandit score state auto-written by main PM flows.
    match sqlx::query(
        "SELECT route_key,
                CAST(COALESCE(score, 0.0) AS DOUBLE),
                CAST(COALESCE(exploration_bonus, 0.0) AS DOUBLE),
                CAST(COALESCE(exploitation_score, 0.0) AS DOUBLE)
         FROM pm_route_bandit_state
         WHERE tenant_id = ?
         ORDER BY updated_at DESC
         LIMIT 120",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            for row in rows {
                let route = row.get::<String, _>(0);
                let score = row.get::<f64, _>(1).clamp(0.0, 1.0);
                let exploration = row.get::<f64, _>(2).clamp(0.0, 1.0);
                let exploitation = row.get::<f64, _>(3).clamp(0.0, 1.0);
                let bandit_score =
                    (score * 0.75 + exploitation * 0.20 + exploration * 0.05).clamp(0.0, 1.0);
                push_weighted_score(&mut blended_scores, &route, bandit_score, 0.15);
            }
        }
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                "load_pm_route_scores: pm_route_bandit_state unavailable: {}",
                error
            );
        }
    }

    let mut out = HashMap::new();
    for (route, (weighted_sum, weight_sum)) in blended_scores {
        if weight_sum <= 0.0 {
            continue;
        }
        out.insert(route, (weighted_sum / weight_sum).clamp(0.0, 1.0));
    }

    out
}

pub(super) async fn load_pm_historical_evidence_hints(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    question: &str,
) -> Vec<serde_json::Value> {
    let tokens: Vec<String> = tokenize_for_match(question)
        .into_iter()
        .filter(|token| token.chars().count() >= 3)
        .take(4)
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut sql = String::from(
        "SELECT claim_text, url, domain, relation, CAST(avg_confidence AS DOUBLE), CAST(run_count AS INTEGER)
         FROM pm_research_evidence_graph
         WHERE tenant_id = ? AND (",
    );
    for idx in 0..tokens.len() {
        if idx > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str("claim_text LIKE ?");
    }
    sql.push_str(") ORDER BY avg_confidence DESC, run_count DESC, updated_at DESC LIMIT 12");

    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(tenant_id);
    for token in tokens {
        query = query.bind(format!("%{}%", token));
    }

    let rows = match query.fetch_all(db).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                "load_pm_historical_evidence_hints failed: {}",
                error
            );
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for row in rows {
        let claim_text = row.get::<String, _>(0);
        let url = row.get::<String, _>(1);
        let domain = row.get::<Option<String>, _>(2);
        let relation = row.get::<String, _>(3);
        let confidence = row.get::<f64, _>(4).clamp(0.0, 1.0);
        let run_count = row.get::<i64, _>(5).max(0);
        out.push(serde_json::json!({
            "claim": claim_text,
            "url": url,
            "domain": domain,
            "relation": relation,
            "confidence": confidence,
            "runCount": run_count,
        }));
    }
    out
}

pub(super) fn pick_pm_attempt_preferences_for_strategy(
    query_variants: &[String],
    enabled_routes: &[PmEnabledRoute],
    strategy: PmRepairStrategy,
    next_attempt: usize,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match strategy {
        PmRepairStrategy::SwitchSource => {
            let (_, route, channel, exec_channel) =
                pick_pm_attempt_preferences(query_variants, enabled_routes, next_attempt + 1);
            let variant = if query_variants.is_empty() {
                None
            } else {
                Some(
                    query_variants[(next_attempt.saturating_sub(1)) % query_variants.len()].clone(),
                )
            };
            (variant, route, channel, exec_channel)
        }
        PmRepairStrategy::SwitchQuery => {
            pick_pm_attempt_preferences(query_variants, enabled_routes, next_attempt)
        }
        PmRepairStrategy::BrowserFallback => {
            pick_pm_attempt_preferences(query_variants, enabled_routes, next_attempt + 1)
        }
        PmRepairStrategy::DegradedSummary => (None, None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_domain::probe_plan::pick_pm_subtask_gap_retry_variant;
    use pm_domain::task_graph::pm_should_bypass_retrieval;
    use std::sync::Mutex;

    static PM_ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarRestore {
        values: Vec<(String, Option<String>)>,
    }

    impl EnvVarRestore {
        fn capture(keys: &[&str]) -> Self {
            Self {
                values: keys
                    .iter()
                    .map(|key| ((*key).to_string(), std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            for (key, value) in &self.values {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn with_pm_env_vars<T>(vars: &[(&str, &str)], f: impl FnOnce() -> T) -> T {
        let _lock = PM_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let keys = vars.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let _restore = EnvVarRestore::capture(&keys);
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
        f()
    }

    #[test]
    fn extract_pm_task_graph_accepts_dynamic_subtasks() {
        let preface = r#"
任务理解：做印尼网赚游戏 app 的市场评估。
TASK_GRAPH {"intent":"research","decompositionMode":"full","subtasks":[{"id":"size","title":"市场规模","goal":"估算印尼网赚游戏市场规模","queries":["indonesia reward game market size"],"deliverable":"TAM/SAM 估算","priority":"high"},{"id":"monetization","title":"变现路径","goal":"提炼主流变现模式","queries":["reward app monetization indonesia"],"deliverable":"变现优先级","priority":"medium"}],"parallelism":{"maxConcurrentSubtasks":4,"maxProbePerSubtask":2}}
EXEC_CONSTRAINTS {"routeAllowlist":["web.search.general"],"routePriority":["web.search.general"],"sourceSlotBudgetSecs":30,"toolBudgetPerAttempt":8,"pipelineTimeoutSecs":300,"stopConditions":["enough_cross_source_citations"]}
"#;
        let graph = extract_pm_task_graph(preface).expect("task graph should parse");
        assert_eq!(
            graph.get("intent").and_then(|v| v.as_str()),
            Some("research")
        );
        assert_eq!(
            graph.get("decompositionMode").and_then(|v| v.as_str()),
            Some("full")
        );
        let subtasks = graph
            .get("subtasks")
            .and_then(|v| v.as_array())
            .expect("subtasks should exist");
        assert_eq!(subtasks.len(), 2);
    }

    #[test]
    fn extract_pm_task_graph_rejects_invalid_required_decomposition() {
        let preface = r#"
TASK_GRAPH {"intent":"analysis","decompositionMode":"light","subtasks":[]}
EXEC_CONSTRAINTS {"routeAllowlist":["web.search.general"],"routePriority":["web.search.general"],"sourceSlotBudgetSecs":20,"toolBudgetPerAttempt":6,"pipelineTimeoutSecs":180,"stopConditions":["budget_exhausted"]}
"#;
        assert!(extract_pm_task_graph(preface).is_none());
    }

    #[test]
    fn extract_pm_task_graph_allows_none_mode_without_subtasks() {
        let preface = r#"
TASK_GRAPH {"intent":"chat","decompositionMode":"none","subtasks":[]}
EXEC_CONSTRAINTS {"routeAllowlist":["web.search.general"],"routePriority":["web.search.general"],"sourceSlotBudgetSecs":20,"toolBudgetPerAttempt":6,"pipelineTimeoutSecs":180,"stopConditions":["budget_exhausted"]}
"#;
        let graph = extract_pm_task_graph(preface).expect("none mode should be allowed");
        assert_eq!(
            graph.get("decompositionMode").and_then(|v| v.as_str()),
            Some("none")
        );
        assert_eq!(
            graph
                .get("subtasks")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len()),
            Some(0)
        );
    }

    #[test]
    fn extract_pm_task_graph_normalizes_decimal_complexity_score() {
        let preface = r#"
TASK_GRAPH_V2 {"intent":"research","complexityScore":8.4,"decompositionMode":"full","subtasks":[{"id":"s1","title":"市场规模","goal":"估算规模","queries":["indonesia reward app market size"],"deliverable":"规模结论","priority":"high"}]}
EXEC_CONSTRAINTS {"routeAllowlist":["web.search.general"],"routePriority":["web.search.general"],"sourceSlotBudgetSecs":20,"toolBudgetPerAttempt":6,"pipelineTimeoutSecs":180,"stopConditions":["budget_exhausted"]}
"#;
        let graph = extract_pm_task_graph(preface).expect("task graph should parse");
        assert_eq!(
            graph.get("complexityScore").and_then(|v| v.as_u64()),
            Some(84)
        );
        assert_eq!(
            graph.get("complexityScoreRaw").and_then(|v| v.as_f64()),
            Some(8.4)
        );
    }

    #[test]
    fn build_pm_fallback_task_graph_generates_subtasks_from_question_and_variants() {
        let plan = serde_json::json!({
            "queryVariants": [
                "B2B SaaS onboarding activation benchmark",
                "B2B SaaS churn reduction product experiment",
                "B2B SaaS support ticket guardrail"
            ],
            "parallelism": {
                "maxConcurrentSubtasks": 6,
                "maxProbePerSubtask": 3,
                "minSourcesPerSubtask": 2
            }
        });
        let graph = build_pm_fallback_task_graph("B2B SaaS onboarding activation", &plan)
            .expect("fallback task graph should be generated");
        assert_eq!(
            graph.get("decompositionMode").and_then(|v| v.as_str()),
            Some("light")
        );
        let subtasks = graph
            .get("subtasks")
            .and_then(|v| v.as_array())
            .expect("fallback subtasks should exist");
        assert_eq!(subtasks.len(), 3);
        assert!(subtasks.iter().all(|item| {
            item.get("queries")
                .and_then(|v| v.as_array())
                .map(|arr| !arr.is_empty())
                .unwrap_or(false)
        }));
        let graph_text = graph.to_string().to_ascii_lowercase();
        assert!(!graph_text.contains("ecpm"));
        assert!(!graph_text.contains("rewarded"));
        assert!(!graph_text.contains("广告"));
    }

    #[test]
    fn apply_pm_task_graph_merges_plan_variants_and_parallelism() {
        let mut plan = build_pm_stage_plan("网赚游戏app印尼市场", &[], &[]);
        let task_graph = serde_json::json!({
            "intent": "research",
            "decompositionMode": "full",
            "subtasks": [
                {
                    "id": "s1",
                    "title": "市场规模",
                    "goal": "估算市场规模",
                    "queries": ["indonesia reward app market size"],
                    "deliverable": "规模结论",
                    "priority": "high"
                },
                {
                    "id": "s2",
                    "title": "核心竞品",
                    "goal": "识别头部产品",
                    "queries": ["top reward apps indonesia"],
                    "deliverable": "竞品清单",
                    "priority": "high"
                }
            ],
            "parallelism": {
                "maxConcurrentSubtasks": 5,
                "maxProbePerSubtask": 3
            }
        });

        apply_pm_task_graph_to_plan(&mut plan, &task_graph);

        let variants = plan
            .get("queryVariants")
            .and_then(|v| v.as_array())
            .expect("query variants should exist");
        assert!(variants.iter().any(|v| {
            v.as_str()
                .is_some_and(|s| s.contains("indonesia reward app market size"))
        }));
        let parallel = plan
            .get("parallelism")
            .and_then(|v| v.as_object())
            .expect("parallelism should exist");
        assert_eq!(
            parallel
                .get("maxConcurrentSubtasks")
                .and_then(|v| v.as_u64()),
            Some(5)
        );
        assert_eq!(
            parallel.get("maxProbePerSubtask").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[test]
    fn build_pm_probe_candidates_prefers_task_graph_subtasks() {
        let plan = serde_json::json!({
            "queryVariants": ["fallback variant"],
            "sourceRoutes": [
                {
                    "routeId": "web.search.general",
                    "channel": "web_search",
                    "executionChannel": "search",
                    "enabled": true
                }
            ],
            "parallelism": {
                "probeRouteFanoutMax": 1,
                "probeCandidateMax": 4,
                "maxConcurrentSubtasks": 2,
                "maxProbePerSubtask": 2
            },
            "taskGraph": {
                "intent": "research",
                "decompositionMode": "full",
                "subtasks": [
                    {
                        "id": "size",
                        "title": "市场规模",
                        "goal": "估算规模",
                        "queries": ["indonesia reward app market size"],
                        "deliverable": "规模结论",
                        "priority": "high"
                    }
                ]
            }
        });

        let candidates = build_pm_probe_candidates(&plan);
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].subtask_id.as_deref(), Some("size"));
        assert_eq!(candidates[0].subtask_title.as_deref(), Some("市场规模"));
        assert_eq!(
            candidates[0].variant,
            "indonesia reward app market size".to_string()
        );
    }

    #[test]
    fn build_pm_probe_candidates_returns_empty_for_none_mode() {
        let plan = serde_json::json!({
            "queryVariants": ["fallback variant"],
            "sourceRoutes": [
                {
                    "routeId": "web.search.general",
                    "channel": "web_search",
                    "executionChannel": "search",
                    "enabled": true
                }
            ],
            "taskGraph": {
                "intent": "chat",
                "decompositionMode": "none",
                "subtasks": []
            }
        });
        let candidates = build_pm_probe_candidates(&plan);
        assert!(candidates.is_empty());
    }

    #[test]
    fn build_pm_probe_candidates_ensures_multi_source_per_subtask() {
        let plan = serde_json::json!({
            "sourceRoutes": [
                {
                    "routeId": "web.search.general",
                    "channel": "web_search",
                    "executionChannel": "search",
                    "enabled": true
                },
                {
                    "routeId": "community.forums.search",
                    "channel": "forum",
                    "executionChannel": "search",
                    "enabled": true
                }
            ],
            "parallelism": {
                "probeRouteFanoutMax": 2,
                "probeCandidateMax": 1,
                "maxConcurrentSubtasks": 1,
                "maxProbePerSubtask": 1,
                "minSourcesPerSubtask": 2
            },
            "taskGraph": {
                "intent": "research",
                "decompositionMode": "full",
                "subtasks": [
                    {
                        "id": "users",
                        "title": "用户画像",
                        "goal": "提炼用户画像",
                        "queries": ["indonesia reward app user segments"],
                        "deliverable": "画像结论",
                        "priority": "high"
                    }
                ]
            }
        });

        let candidates = build_pm_probe_candidates(&plan);
        assert!(candidates.len() >= 2);
        let mut routes = HashSet::<String>::new();
        for candidate in &candidates {
            let route_id = candidate
                .route
                .as_ref()
                .and_then(|route| route.get("routeId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !route_id.is_empty() {
                routes.insert(route_id.to_string());
            }
        }
        assert!(routes.len() >= 2);
    }

    #[test]
    fn build_pm_probe_candidates_hybrid_keeps_query_only_backfill() {
        with_pm_env_vars(
            &[
                ("PM_SUBTASK_CANDIDATE_CAP", "10"),
                ("PM_SUBTASK_PROBE_COVERAGE_PERCENT", "70"),
                ("PM_QUERY_ONLY_VARIANT_FANOUT", "12"),
            ],
            || {
                let plan = serde_json::json!({
                    "queryVariants": [
                        "q0","q1","q2","q3","q4","q5","q6","q7","q8","q9","q10"
                    ],
                    "sourceRoutes": [
                        {
                            "routeId": "web.search.general",
                            "channel": "web_search",
                            "executionChannel": "search",
                            "enabled": true
                        }
                    ],
                    "parallelism": {
                        "probeRouteFanoutMax": 1,
                        "probeCandidateMax": 10,
                        "maxConcurrentSubtasks": 8,
                        "maxProbePerSubtask": 1,
                        "minSourcesPerSubtask": 1
                    },
                    "taskGraph": {
                        "intent": "research",
                        "decompositionMode": "full",
                        "subtasks": [
                            {"id":"s1","title":"t1","goal":"g1","queries":["sq1"],"priority":"high"},
                            {"id":"s2","title":"t2","goal":"g2","queries":["sq2"],"priority":"high"},
                            {"id":"s3","title":"t3","goal":"g3","queries":["sq3"],"priority":"high"},
                            {"id":"s4","title":"t4","goal":"g4","queries":["sq4"],"priority":"high"},
                            {"id":"s5","title":"t5","goal":"g5","queries":["sq5"],"priority":"medium"},
                            {"id":"s6","title":"t6","goal":"g6","queries":["sq6"],"priority":"medium"},
                            {"id":"s7","title":"t7","goal":"g7","queries":["sq7"],"priority":"medium"},
                            {"id":"s8","title":"t8","goal":"g8","queries":["sq8"],"priority":"low"}
                        ]
                    }
                });

                let candidates = build_pm_probe_candidates(&plan);
                assert!(!candidates.is_empty());
                assert!(candidates.len() <= 10);
                let query_only = candidates
                    .iter()
                    .filter(|c| c.subtask_id.is_none() && c.subtask_key.is_none())
                    .count();
                assert!(
                    query_only > 0,
                    "should reserve query-only backfill candidates"
                );
            },
        );
    }

    #[test]
    fn build_pm_probe_candidates_query_only_when_no_subtasks() {
        with_pm_env_vars(&[("PM_QUERY_ONLY_VARIANT_FANOUT", "6")], || {
            let plan = serde_json::json!({
                "queryVariants": ["a","b","c","d","e","f","g"],
                "sourceRoutes": [
                    {
                        "routeId": "web.search.general",
                        "channel": "web_search",
                        "executionChannel": "search",
                        "enabled": true
                    }
                ],
                "parallelism": {
                    "probeRouteFanoutMax": 1,
                    "probeCandidateMax": 4
                },
                "taskGraph": {
                    "intent": "chat",
                    "decompositionMode": "light",
                    "subtasks": []
                }
            });
            let candidates = build_pm_probe_candidates(&plan);
            assert_eq!(candidates.len(), 4);
            assert!(candidates.iter().all(|c| c.subtask_id.is_none()));
        });
    }

    #[test]
    fn apply_pm_exec_constraints_to_plan_filters_and_reorders_routes() {
        let mut plan = serde_json::json!({
            "sourceRoutes": [
                {
                    "routeId": "web.search.general",
                    "channel": "web_search",
                    "executionChannel": "search",
                    "enabled": true
                },
                {
                    "routeId": "community.forums.search",
                    "channel": "forum",
                    "executionChannel": "search",
                    "enabled": true
                },
                {
                    "routeId": "reviews.evidence.search",
                    "channel": "review_evidence",
                    "executionChannel": "search",
                    "enabled": true
                }
            ]
        });
        let constraints = PmExecConstraints {
            route_allowlist: vec![
                "reviews.evidence.search".to_string(),
                "web.search.general".to_string(),
            ],
            route_priority: vec![
                "reviews.evidence.search".to_string(),
                "web.search.general".to_string(),
            ],
            stop_conditions: vec!["enough_cross_source_citations".to_string()],
            source_slot_budget_secs: 40,
            tool_budget_per_attempt: 8,
            pipeline_timeout_secs: 360,
        };

        apply_pm_exec_constraints_to_plan(&mut plan, &constraints);

        let routes = plan
            .get("sourceRoutes")
            .and_then(|v| v.as_array())
            .expect("sourceRoutes should exist");
        assert_eq!(
            routes[0].get("routeId").and_then(|v| v.as_str()),
            Some("reviews.evidence.search")
        );
        let forum_enabled = routes
            .iter()
            .find(|route| {
                route.get("routeId").and_then(|v| v.as_str()) == Some("community.forums.search")
            })
            .and_then(|route| route.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        assert!(!forum_enabled);
        let selected = plan
            .get("selectedRouteIds")
            .and_then(|v| v.as_array())
            .expect("selectedRouteIds should exist");
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].as_str(), Some("reviews.evidence.search"));
        assert_eq!(selected[1].as_str(), Some("web.search.general"));
    }

    #[test]
    fn build_pm_stage_plan_defaults_probe_per_subtask_to_two() {
        let plan = build_pm_stage_plan("印尼网赚游戏市场", &[], &[]);
        let parallel = plan
            .get("parallelism")
            .and_then(|v| v.as_object())
            .expect("parallelism should exist");
        assert_eq!(
            parallel.get("maxProbePerSubtask").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[test]
    fn build_pm_stage_plan_supports_manual_route_channels_override() {
        with_pm_env_vars(
            &[
                ("PM_ROUTE_CHANNELS", "web_search,news,forum,reviews"),
                ("PM_ROUTE_MAX_CHANNELS", "3"),
            ],
            || {
                let plan = build_pm_stage_plan("任意问题", &[], &[]);
                let channels = plan
                    .get("channels")
                    .and_then(|v| v.as_array())
                    .expect("channels should exist")
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(channels, vec!["web_search", "news_sites", "forum"]);
            },
        );
    }

    #[test]
    fn build_pm_stage_plan_auto_channels_respect_max_guard() {
        with_pm_env_vars(
            &[
                ("PM_ROUTE_CHANNELS", "auto"),
                ("PM_ROUTE_MAX_CHANNELS", "3"),
            ],
            || {
                let plan =
                    build_pm_stage_plan("印尼市场最新政策、用户画像和应用评分趋势", &[], &[]);
                let channels = plan
                    .get("channels")
                    .and_then(|v| v.as_array())
                    .expect("channels should exist")
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>();
                assert!(!channels.is_empty());
                assert!(channels.len() <= 3);
                assert_eq!(channels[0], "web_search");
            },
        );
    }

    #[test]
    fn pm_should_bypass_retrieval_for_none_decomposition_without_subtasks() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "analysis",
                "complexityScore": 42,
                "decompositionMode": "none",
                "subtasks": []
            }
        });
        assert!(pm_should_bypass_retrieval(
            &plan,
            "ewma 和模型训练估计 ecpm 差异能提升多少 roi"
        ));
    }

    #[test]
    fn build_pm_stage_plan_attaches_report_hint_without_forcing_report_strategy() {
        let report = "我们是 B2B SaaS onboarding 产品，过去 30 天 trial 用户 18,420，activation 31%，MRR $120k，churn 7.2%，CAC $86。按用户分层：solo trial activation 18%，team trial activation 44%。之前试过 mandatory demo wall，activation 下降。我的诉求是基于这份报告给产品运营策略。";
        let plan = build_pm_stage_plan(report, &[], &[]);
        assert_eq!(plan.get("mode").and_then(|v| v.as_str()), Some("auto"));
        assert!(plan.get("reportStrategyHint").is_some());
        assert!(plan.get("reportStrategy").is_none());
        assert!(plan.get("taskGraph").is_none());
    }

    #[test]
    fn pm_should_not_bypass_retrieval_for_research_subtasks() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "research",
                "decompositionMode": "light",
                "subtasks": [
                    {"id": "s1", "title": "bench", "goal": "collect benchmarks", "queries": ["query"], "deliverable": "d", "priority": "high"}
                ]
            }
        });
        assert!(!pm_should_bypass_retrieval(
            &plan,
            "indonesia market benchmark"
        ));
    }

    #[test]
    fn pm_should_not_bypass_when_question_likely_needs_external_evidence() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "analysis",
                "complexityScore": 40,
                "decompositionMode": "none",
                "subtasks": []
            }
        });
        assert!(!pm_should_bypass_retrieval(
            &plan,
            "现在印尼网赚游戏市场规模和最新用户画像趋势如何"
        ));
    }

    #[test]
    fn pm_should_not_bypass_live_search_even_if_planner_calls_it_chat() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "chat",
                "complexityScore": 10,
                "decompositionMode": "none",
                "subtasks": []
            }
        });
        assert!(!pm_should_bypass_retrieval(&plan, "查一下北京天气"));
        assert!(!pm_should_bypass_retrieval(&plan, "你上网查一下"));
    }

    #[test]
    fn pm_should_not_bypass_high_complexity_even_without_subtasks() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "analysis",
                "complexityScore": 78,
                "decompositionMode": "none",
                "subtasks": []
            }
        });
        assert!(!pm_should_bypass_retrieval(
            &plan,
            "评估增长路径与风险并给出策略"
        ));
    }

    #[test]
    fn pick_pm_subtask_gap_retry_variant_prefers_gap_subtask_query() {
        let plan = serde_json::json!({
            "taskGraph": {
                "subtasks": [
                    {
                        "id": "size",
                        "title": "市场规模",
                        "goal": "估算规模",
                        "queries": ["indonesia reward game market size"]
                    },
                    {
                        "id": "persona",
                        "title": "用户画像",
                        "goal": "识别核心用户",
                        "queries": ["indonesia reward game app user persona"]
                    }
                ]
            }
        });
        let picked = pick_pm_subtask_gap_retry_variant(&plan, &["用户画像".to_string()]);
        assert_eq!(
            picked.as_deref(),
            Some("indonesia reward game app user persona")
        );
    }

    #[test]
    fn pick_pm_subtask_gap_retry_variant_for_attempt_rotates_targets() {
        let plan = serde_json::json!({
            "taskGraph": {
                "subtasks": [
                    {
                        "id": "size",
                        "title": "市场规模",
                        "goal": "估算规模",
                        "queries": ["indonesia reward game market size"]
                    },
                    {
                        "id": "persona",
                        "title": "用户画像",
                        "goal": "识别核心用户",
                        "queries": ["indonesia reward game app user persona"]
                    }
                ]
            }
        });
        let attempt1 = pick_pm_subtask_gap_retry_variant_for_attempt(
            &plan,
            &["市场规模".to_string(), "用户画像".to_string()],
            1,
        );
        let attempt2 = pick_pm_subtask_gap_retry_variant_for_attempt(
            &plan,
            &["市场规模".to_string(), "用户画像".to_string()],
            2,
        );
        assert_eq!(
            attempt1.as_deref(),
            Some("indonesia reward game market size")
        );
        assert_eq!(
            attempt2.as_deref(),
            Some("indonesia reward game app user persona")
        );
    }

    #[test]
    fn prioritize_pm_probe_candidates_for_subtasks_keeps_target_only_when_strict() {
        let candidates = vec![
            PmProbeCandidate {
                variant: "q-size".to_string(),
                route: None,
                subtask_key: Some("size".to_string()),
                subtask_id: Some("size".to_string()),
                subtask_title: Some("市场规模".to_string()),
                subtask_goal: Some("估算规模".to_string()),
                subtask_deliverable: None,
                subtask_required_evidence_type: None,
                subtask_priority: None,
            },
            PmProbeCandidate {
                variant: "q-persona".to_string(),
                route: None,
                subtask_key: Some("persona".to_string()),
                subtask_id: Some("persona".to_string()),
                subtask_title: Some("用户画像".to_string()),
                subtask_goal: Some("识别用户".to_string()),
                subtask_deliverable: None,
                subtask_required_evidence_type: None,
                subtask_priority: None,
            },
        ];
        let focused = prioritize_pm_probe_candidates_for_subtasks(
            candidates,
            &["用户画像".to_string()],
            true,
        );
        assert_eq!(focused.len(), 1);
        assert_eq!(focused[0].variant, "q-persona");
    }

    #[test]
    fn pick_pm_subtask_focus_for_repair_respects_attempt_limit() {
        let mut queue = Vec::<String>::new();
        let mut attempts = HashMap::<String, usize>::new();
        let first = pick_pm_subtask_focus_for_repair(
            &mut queue,
            &mut attempts,
            &["市场规模".to_string(), "用户画像".to_string()],
            1,
        );
        assert_eq!(first.as_deref(), Some("市场规模"));
        attempts.insert(normalize_claim_key("市场规模"), 1);
        let second = pick_pm_subtask_focus_for_repair(
            &mut queue,
            &mut attempts,
            &["市场规模".to_string(), "用户画像".to_string()],
            1,
        );
        assert_eq!(second.as_deref(), Some("用户画像"));
    }

    #[test]
    fn pm_should_consume_source_quota_respects_probe_only_turns() {
        assert!(!pm_should_consume_source_quota(true, false));
        assert!(pm_should_consume_source_quota(true, true));
        assert!(pm_should_consume_source_quota(false, false));
        assert!(pm_should_consume_source_quota(false, true));
    }
}
