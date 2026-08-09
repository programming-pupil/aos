use super::*;

#[derive(Debug, Clone)]
pub(super) struct PmStartupPreflightOutcome {
    pub(super) checked_at: Instant,
    pub(super) cached: bool,
    pub(super) model_probe_skipped: bool,
    pub(super) retrieval_probe_skipped: bool,
    pub(super) model_stream_ok: bool,
    pub(super) retrieval_egress_ok: bool,
    pub(super) retrieval_search_ok: bool,
    pub(super) retrieval_browser_ok: bool,
    pub(super) model_latency_ms: Option<u64>,
    pub(super) retrieval_latency_ms: Option<u64>,
    pub(super) retrieval_search_latency_ms: Option<u64>,
    pub(super) retrieval_browser_latency_ms: Option<u64>,
    pub(super) model_error: Option<String>,
    pub(super) retrieval_error: Option<String>,
    pub(super) retrieval_search_error: Option<String>,
    pub(super) retrieval_browser_error: Option<String>,
}

impl PmStartupPreflightOutcome {
    fn is_model_timeout_soft_failure(&self) -> bool {
        self.model_error.as_deref().is_some_and(|error| {
            error.contains("timed out after") || error.contains("model preflight channel closed")
        })
    }

    pub(super) fn passed(&self, require_retrieval: bool) -> bool {
        let model_gate = self.model_probe_skipped
            || self.model_stream_ok
            || self.is_model_timeout_soft_failure();
        if !require_retrieval {
            return model_gate;
        }
        // PM search-only mode degrades retrieval probe failures by design:
        // as long as model path is healthy enough, continue to understand/retrieve stages.
        model_gate
    }

    fn is_retrieval_soft_failure(&self, require_retrieval: bool) -> bool {
        let retrieval_gate = if self.retrieval_egress_ok {
            true
        } else {
            self.retrieval_search_ok || self.retrieval_browser_ok
        };
        if !require_retrieval || retrieval_gate {
            return false;
        }
        self.retrieval_error.as_deref().is_some_and(|error| {
            error.contains("request_error=")
                || error.contains("timed out after")
                || error.contains("retrieval preflight failed on all endpoints")
                || error.contains("circuit_open=")
        })
    }

    pub(super) fn search_channel_score(&self) -> f64 {
        if self.retrieval_probe_skipped {
            return 0.6;
        }
        if self.retrieval_search_ok {
            let latency = self.retrieval_search_latency_ms.unwrap_or(3000).max(1) as f64;
            (1.0 / (1.0 + latency / 2200.0)).clamp(0.35, 1.0)
        } else if self.is_retrieval_soft_failure(true) {
            0.45
        } else {
            0.15
        }
    }

    pub(super) fn browser_channel_score(&self) -> f64 {
        if self.retrieval_probe_skipped {
            return 0.6;
        }
        if self.retrieval_browser_ok {
            let latency = self.retrieval_browser_latency_ms.unwrap_or(3000).max(1) as f64;
            (1.0 / (1.0 + latency / 2200.0)).clamp(0.35, 1.0)
        } else if self.is_retrieval_soft_failure(true) {
            0.45
        } else {
            0.15
        }
    }

    pub(super) fn to_stage_detail(&self, require_retrieval: bool) -> serde_json::Value {
        let search_only_mode = pm_flag_enabled("PM_RETRIEVE_SEARCH_ONLY", true);
        let retrieval_browser_ok = if require_retrieval && search_only_mode {
            false
        } else if require_retrieval {
            self.retrieval_browser_ok
        } else {
            true
        };
        let retrieval_browser_error = if require_retrieval && search_only_mode {
            Some("disabled_in_search_only_mode".to_string())
        } else if require_retrieval {
            self.retrieval_browser_error.clone()
        } else {
            None
        };
        let channel_scores = if require_retrieval {
            if search_only_mode {
                serde_json::json!({
                    "search": self.search_channel_score(),
                })
            } else {
                serde_json::json!({
                    "search": self.search_channel_score(),
                    "browser": self.browser_channel_score(),
                })
            }
        } else {
            serde_json::json!({
                "search": 1.0,
                "browser": 1.0,
            })
        };
        serde_json::json!({
            "cached": self.cached,
            "requireRetrieval": require_retrieval,
            "searchOnlyMode": search_only_mode,
            "modelProbeSkipped": self.model_probe_skipped,
            "retrievalProbeSkipped": self.retrieval_probe_skipped,
            "modelStreamOk": self.model_stream_ok,
            "modelSoftTimeoutAllowed": self.is_model_timeout_soft_failure(),
            "retrievalEgressOk": if require_retrieval {
                self.retrieval_egress_ok || self.retrieval_search_ok || self.retrieval_browser_ok
            } else { true },
            "retrievalSearchOk": if require_retrieval { self.retrieval_search_ok } else { true },
            "retrievalBrowserOk": retrieval_browser_ok,
            "channelScores": channel_scores,
            "retrievalSoftFailureAllowed": self.is_retrieval_soft_failure(require_retrieval),
            "modelLatencyMs": self.model_latency_ms,
            "retrievalLatencyMs": self.retrieval_latency_ms,
            "retrievalSearchLatencyMs": self.retrieval_search_latency_ms,
            "retrievalBrowserLatencyMs": self.retrieval_browser_latency_ms,
            "modelError": self.model_error,
            "retrievalError": if require_retrieval { self.retrieval_error.clone() } else { None::<String> },
            "retrievalSearchError": if require_retrieval { self.retrieval_search_error.clone() } else { None::<String> },
            "retrievalBrowserError": retrieval_browser_error,
        })
    }

    pub(super) fn user_facing_error(&self, require_retrieval: bool) -> String {
        if !self.model_stream_ok && !self.is_model_timeout_soft_failure() {
            return format!(
                "preflight blocked: model stream unavailable ({})",
                self.model_error
                    .as_deref()
                    .unwrap_or("unknown model stream error")
            );
        }
        if require_retrieval {
            return "preflight retrieval probe degraded; continue in search-only mode".to_string();
        }
        "preflight blocked: unknown".to_string()
    }
}

#[derive(Debug, Clone)]
struct PmStartupPreflightCacheEntry {
    checked_at: Instant,
    outcome: PmStartupPreflightOutcome,
}

fn pm_startup_preflight_cache() -> &'static Mutex<HashMap<String, PmStartupPreflightCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PmStartupPreflightCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pm_startup_preflight_session_cache(
) -> &'static Mutex<HashMap<String, PmStartupPreflightCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PmStartupPreflightCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn pm_preflight_circuit_allow(endpoint: &str) -> Result<(), String> {
    let now = Instant::now();
    let mut guard = pm_preflight_circuit_breakers().lock().await;
    let state = guard
        .entry(endpoint.to_string())
        .or_insert_with(PmEndpointCircuitState::new);
    if let Some(open_until) = state.open_until {
        if now < open_until {
            let remaining = open_until.duration_since(now).as_secs();
            return Err(format!("open_for_{}s", remaining));
        }
        state.open_until = None;
        state.consecutive_failures = PM_PREFLIGHT_CB_FAILURE_THRESHOLD.saturating_sub(1);
    }
    Ok(())
}

async fn pm_preflight_circuit_report(endpoint: &str, success: bool) {
    let now = Instant::now();
    let mut guard = pm_preflight_circuit_breakers().lock().await;
    let state = guard
        .entry(endpoint.to_string())
        .or_insert_with(PmEndpointCircuitState::new);
    if success {
        state.consecutive_failures = 0;
        state.open_until = None;
        return;
    }
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    if state.consecutive_failures >= PM_PREFLIGHT_CB_FAILURE_THRESHOLD {
        state.open_until = Some(now + Duration::from_secs(PM_PREFLIGHT_CB_COOLDOWN_SECS));
    }
}

async fn probe_pm_model_stream_once(
    manager: Arc<AgentSessionManager>,
    user_id: &str,
    tenant_id: &str,
    model: &str,
    timeout_secs: u64,
) -> Result<u64, String> {
    let started = Instant::now();
    let session = manager
        .create_session(
            user_id,
            tenant_id,
            None,
            Some(model),
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            Some("pm"),
            None,
            None,
        )
        .await
        .map_err(|e| format!("create preflight session failed: {e}"))?;
    let session_id = session.session_id.clone();
    let session_guard = PmTransientSessionGuard::new(manager.clone(), session_id.clone());
    let prompt = wrap_pm_research_prompt(
        "pm",
        format!(
            "{PM_ORCH_INTERNAL_BEGIN}\n\
PREFLIGHT CHECK: verify model streaming path only.\n\
Do NOT call any tools.\n\
Reply exactly with: OK\n\
{PM_ORCH_INTERNAL_END}\n\n\
Input: health check"
        ),
    );

    let (tx, rx) = tokio::sync::mpsc::channel::<agent_gateway::AgentEvent>(256);
    let (result_tx, mut result_rx) =
        tokio::sync::mpsc::channel::<std::result::Result<TurnResult, GatewayError>>(1);
    let manager_for_turn = manager.clone();
    let session_id_for_turn = session_id.clone();
    tokio::spawn(async move {
        let _drain = tokio::spawn(async move {
            let mut local_rx = rx;
            while local_rx.recv().await.is_some() {}
        });
        let r = manager_for_turn
            .run_turn_streaming(&session_id_for_turn, prompt, tx)
            .await;
        let _ = result_tx.send(r).await;
    });

    let turn_result = timeout(Duration::from_secs(timeout_secs), result_rx.recv()).await;
    session_guard.finish().await;

    let turn = match turn_result {
        Ok(Some(Ok(turn))) => turn,
        Ok(Some(Err(e))) => return Err(e.to_string()),
        Ok(None) => return Err("model preflight channel closed".to_string()),
        Err(_) => return Err(format!("model preflight timed out after {}s", timeout_secs)),
    };

    let has_text = !turn.text.trim().is_empty();
    let has_thinking = turn
        .thinking
        .as_deref()
        .is_some_and(|thinking| !thinking.trim().is_empty());
    if !has_text && !has_thinking {
        return Err("model preflight returned empty content".to_string());
    }
    Ok(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
}

async fn probe_pm_search_egress_once(
    request_timeout_secs: u64,
    overall_timeout_secs: u64,
) -> Result<u64, String> {
    let started = Instant::now();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(request_timeout_secs))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 AOS-Research/1.0",
        )
        .build()
        .map_err(|e| format!("retrieval preflight client error: {e}"))?;

    fn parse_search_provider_entry(entry: &str) -> Option<(String, String)> {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some((base, param)) = trimmed.rsplit_once('|') {
            let base = base.trim();
            let param = param.trim();
            if base.is_empty() {
                return None;
            }
            if param.is_empty() {
                return Some((base.to_string(), "q".to_string()));
            }
            return Some((base.to_string(), param.to_string()));
        }
        Some((trimmed.to_string(), "q".to_string()))
    }

    fn append_search_entries_from_env(
        entries: &mut Vec<(String, String)>,
        env_name: &str,
        parser: fn(&str) -> Option<(String, String)>,
    ) {
        let Ok(raw) = std::env::var(env_name) else {
            return;
        };
        let parsed = raw.split([',', ';']).filter_map(parser).collect::<Vec<_>>();
        if parsed.is_empty() {
            return;
        }
        entries.extend(parsed);
    }

    fn dedupe_search_entries(entries: Vec<(String, String)>) -> Vec<(String, String)> {
        let mut seen = HashSet::<String>::new();
        let mut deduped = Vec::<(String, String)>::new();
        for (endpoint, param) in entries {
            let key = format!(
                "{}|{}",
                endpoint.trim().to_ascii_lowercase(),
                param.trim().to_ascii_lowercase()
            );
            if !seen.insert(key) {
                continue;
            }
            deduped.push((endpoint, param));
        }
        deduped
    }

    let query = "AOS PM startup health check retrieval".to_string();
    let mut provider_entries: Vec<(String, String)> = Vec::new();
    // Optional egress smoke probe. PM search itself goes through the v5
    // orchestrator/provider registry, so no public search engine is hardcoded here.
    append_search_entries_from_env(
        &mut provider_entries,
        "PM_PREFLIGHT_SEARCH_BASE_URLS",
        parse_search_provider_entry,
    );
    append_search_entries_from_env(
        &mut provider_entries,
        "PM_PREFLIGHT_SEARCH_BASE_URLS_APPEND",
        parse_search_provider_entry,
    );
    let provider_entries = dedupe_search_entries(provider_entries);
    if provider_entries.is_empty() {
        return Err(
            "retrieval preflight has no configured endpoints; set PM_PREFLIGHT_SEARCH_BASE_URLS or leave PM_PREFLIGHT_ENABLE_RETRIEVAL_PROBE=false"
                .to_string(),
        );
    }
    let probes = provider_entries.into_iter().map(|(endpoint, query_param)| {
        let client = client.clone();
        let query = query.clone();
        async move {
            if let Err(reason) = pm_preflight_circuit_allow(&endpoint).await {
                return Err(format!("{endpoint}: circuit_open={reason}"));
            }
            let mut url = reqwest::Url::parse(&endpoint)
                .map_err(|e| format!("{endpoint}: invalid_url={e}"))?;
            url.query_pairs_mut().append_pair(&query_param, &query);
            let response = match client
                .get(url)
                .header(
                    reqwest::header::ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                )
                .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    pm_preflight_circuit_report(&endpoint, false).await;
                    return Err(format!("{endpoint}: request_error={e}"));
                }
            };
            let status = response.status();
            if !status.is_success() {
                pm_preflight_circuit_report(&endpoint, false).await;
                return Err(format!("{endpoint}: http_status={status}"));
            }
            let body = match response.text().await {
                Ok(body) => body,
                Err(e) => {
                    pm_preflight_circuit_report(&endpoint, false).await;
                    return Err(format!("{endpoint}: read_body_error={e}"));
                }
            };
            if body.trim().is_empty() {
                pm_preflight_circuit_report(&endpoint, false).await;
                return Err(format!("{endpoint}: empty_body"));
            }
            pm_preflight_circuit_report(&endpoint, true).await;
            Ok(endpoint.to_string())
        }
    });

    let joined = timeout(
        Duration::from_secs(overall_timeout_secs),
        futures_util::future::join_all(probes),
    )
    .await
    .map_err(|_| {
        format!(
            "retrieval preflight timed out after {}s (parallel probe)",
            overall_timeout_secs
        )
    })?;

    let mut attempts: Vec<String> = Vec::new();
    for result in joined {
        match result {
            Ok(_endpoint) => {
                return Ok(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
            }
            Err(error) => attempts.push(error),
        }
    }

    Err(format!(
        "search retrieval preflight failed on all endpoints: {}",
        attempts.join(" | ")
    ))
}

pub(super) async fn run_pm_startup_preflight(
    manager: Arc<AgentSessionManager>,
    user_id: &str,
    tenant_id: &str,
    session_id: Option<&str>,
    model: &str,
    require_retrieval: bool,
    budget: &PmTimeoutBudget,
) -> PmStartupPreflightOutcome {
    let retrieval_probe_enabled =
        require_retrieval && pm_flag_enabled("PM_PREFLIGHT_ENABLE_RETRIEVAL_PROBE", false);
    // The real planning turn immediately exercises the same model path and already has a
    // bounded deterministic fallback. An extra inline LLM health call only adds first-token
    // latency, so keep it opt-in for diagnostics.
    let model_probe_enabled = pm_flag_enabled("PM_PREFLIGHT_ENABLE_MODEL_PROBE", false);
    if let Some(session_scope) = session_id {
        let cache_key = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            tenant_id,
            user_id,
            session_scope,
            model,
            require_retrieval,
            retrieval_probe_enabled,
            model_probe_enabled,
            budget.preflight_model_timeout_secs,
            budget.preflight_probe_timeout_secs,
            budget.preflight_overall_timeout_secs
        );
        let now = Instant::now();
        if let Some(cached) = {
            let guard = pm_startup_preflight_session_cache().lock().await;
            guard.get(&cache_key).cloned()
        } {
            let ttl = if cached.outcome.passed(require_retrieval) {
                PM_PREFLIGHT_SESSION_CACHE_TTL_SECS
            } else {
                PM_PREFLIGHT_FAILURE_CACHE_TTL_SECS
            };
            if now.duration_since(cached.checked_at).as_secs() < ttl {
                let mut outcome = cached.outcome.clone();
                outcome.cached = true;
                return outcome;
            }
        }
    }

    let cache_key = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}",
        tenant_id,
        user_id,
        model,
        require_retrieval,
        retrieval_probe_enabled,
        model_probe_enabled,
        budget.preflight_model_timeout_secs,
        budget.preflight_probe_timeout_secs,
        budget.preflight_overall_timeout_secs
    );
    let now = Instant::now();
    if let Some(cached) = {
        let guard = pm_startup_preflight_cache().lock().await;
        guard.get(&cache_key).cloned()
    } {
        let ttl = if cached.outcome.passed(require_retrieval) {
            PM_PREFLIGHT_CACHE_TTL_SECS
        } else {
            PM_PREFLIGHT_FAILURE_CACHE_TTL_SECS
        };
        if now.duration_since(cached.checked_at).as_secs() < ttl {
            let mut outcome = cached.outcome.clone();
            outcome.cached = true;
            return outcome;
        }
    }

    let mut outcome = PmStartupPreflightOutcome {
        checked_at: now,
        cached: false,
        model_probe_skipped: !model_probe_enabled,
        retrieval_probe_skipped: !retrieval_probe_enabled,
        model_stream_ok: false,
        retrieval_egress_ok: !require_retrieval || !retrieval_probe_enabled,
        retrieval_search_ok: !require_retrieval || !retrieval_probe_enabled,
        retrieval_browser_ok: false,
        model_latency_ms: None,
        retrieval_latency_ms: None,
        retrieval_search_latency_ms: None,
        retrieval_browser_latency_ms: None,
        model_error: None,
        retrieval_error: None,
        retrieval_search_error: None,
        retrieval_browser_error: Some("disabled_in_search_only_mode".to_string()),
    };

    let model_probe = async {
        if model_probe_enabled {
            Some(
                probe_pm_model_stream_once(
                    manager.clone(),
                    user_id,
                    tenant_id,
                    model,
                    budget.preflight_model_timeout_secs,
                )
                .await,
            )
        } else {
            None
        }
    };
    let retrieval_search_probe = async {
        if require_retrieval && retrieval_probe_enabled {
            Some(
                probe_pm_search_egress_once(
                    budget.preflight_probe_timeout_secs,
                    budget.preflight_overall_timeout_secs,
                )
                .await,
            )
        } else {
            None
        }
    };
    let (model_probe, retrieval_search_probe) = tokio::join!(model_probe, retrieval_search_probe);

    if let Some(model_probe) = model_probe {
        match model_probe {
            Ok(latency) => {
                outcome.model_stream_ok = true;
                outcome.model_latency_ms = Some(latency);
            }
            Err(error) => {
                outcome.model_error = Some(error);
            }
        }
    }

    if let Some(retrieval_search_probe) = retrieval_search_probe {
        let mut retrieval_errors: Vec<String> = Vec::new();
        match retrieval_search_probe {
            Ok(latency) => {
                outcome.retrieval_search_ok = true;
                outcome.retrieval_search_latency_ms = Some(latency);
            }
            Err(error) => {
                outcome.retrieval_search_error = Some(error.clone());
                retrieval_errors.push(format!("search:{error}"));
            }
        };
        outcome.retrieval_egress_ok = outcome.retrieval_search_ok;
        outcome.retrieval_latency_ms = match (
            outcome.retrieval_search_latency_ms,
            outcome.retrieval_browser_latency_ms,
        ) {
            (Some(search), Some(browser)) => Some(search.min(browser)),
            (Some(search), None) => Some(search),
            (None, Some(browser)) => Some(browser),
            (None, None) => None,
        };
        if !outcome.retrieval_egress_ok {
            outcome.retrieval_error = Some(format!(
                "retrieval preflight failed on all channels: {}",
                retrieval_errors.join(" | ")
            ));
        }
    }

    {
        let mut guard = pm_startup_preflight_cache().lock().await;
        guard.insert(
            cache_key,
            PmStartupPreflightCacheEntry {
                checked_at: outcome.checked_at,
                outcome: outcome.clone(),
            },
        );
    }

    if let Some(session_scope) = session_id {
        let session_cache_key = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            tenant_id,
            user_id,
            session_scope,
            model,
            require_retrieval,
            retrieval_probe_enabled,
            model_probe_enabled,
            budget.preflight_model_timeout_secs,
            budget.preflight_probe_timeout_secs,
            budget.preflight_overall_timeout_secs
        );
        let mut guard = pm_startup_preflight_session_cache().lock().await;
        if outcome.passed(require_retrieval) {
            guard.insert(
                session_cache_key,
                PmStartupPreflightCacheEntry {
                    checked_at: outcome.checked_at,
                    outcome: outcome.clone(),
                },
            );
        } else {
            guard.remove(&session_cache_key);
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_preflight_is_neutral_and_does_not_block_the_real_turn() {
        let outcome = PmStartupPreflightOutcome {
            checked_at: Instant::now(),
            cached: false,
            model_probe_skipped: true,
            retrieval_probe_skipped: true,
            model_stream_ok: false,
            retrieval_egress_ok: true,
            retrieval_search_ok: true,
            retrieval_browser_ok: false,
            model_latency_ms: None,
            retrieval_latency_ms: None,
            retrieval_search_latency_ms: None,
            retrieval_browser_latency_ms: None,
            model_error: None,
            retrieval_error: None,
            retrieval_search_error: None,
            retrieval_browser_error: None,
        };

        assert!(outcome.passed(true));
        assert_eq!(outcome.search_channel_score(), 0.6);
        assert_eq!(outcome.browser_channel_score(), 0.6);
        let detail = outcome.to_stage_detail(true);
        assert_eq!(
            detail
                .get("modelProbeSkipped")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            detail
                .get("retrievalProbeSkipped")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }
}
