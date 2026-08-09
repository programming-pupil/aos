use super::*;

fn format_pm_runtime_error_detail(error: &AppError) -> String {
    match error {
        AppError::Database(db_error) => match db_error {
            sqlx::Error::Database(db) => {
                let code = db.code().map(|c| c.to_string()).unwrap_or_default();
                format!(
                    "database error: code={} message={} debug={:?}",
                    code,
                    db.message(),
                    db_error
                )
            }
            _ => format!("database error: {db_error:?}"),
        },
        _ => format!("{error} ({error:?})"),
    }
}

pub(super) fn format_panic_payload(payload: &(dyn Any + Send)) -> String {
    if let Some(msg) = payload.downcast_ref::<&str>() {
        (*msg).to_string()
    } else if let Some(msg) = payload.downcast_ref::<String>() {
        msg.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

pub(super) async fn run_pm_background_runtime_cycle_impl(state: &AppState) -> Result<(), String> {
    let config = pm_task_research_config();
    let worker_id = pm_task_worker_id().to_string();
    let task_manager = pm_research_task_manager().clone();
    let manager = get_agent_manager(state).clone();

    task_manager.cleanup_expired().await;
    let candidates = load_claimable_pm_task_runtime_rows(
        state.control_db(),
        &worker_id,
        config.claim_batch_size.max(1),
        config.max_concurrent_per_tenant.max(1),
    )
    .await
    .map_err(|e| {
        format!(
            "load claimable pm tasks failed: {}",
            format_pm_runtime_error_detail(&e)
        )
    })?;

    if candidates.is_empty() {
        return Ok(());
    }

    let candidate_total = candidates.len();
    let mut started = 0usize;
    let mut skipped = 0usize;
    let mut recovered = 0usize;
    let mut skip_terminal_or_cancelled = 0usize;
    let mut skip_already_active = 0usize;
    let mut skip_session_active = 0usize;
    let mut skip_lease_conflict = 0usize;
    let mut skip_missing_runtime_row = 0usize;
    let mut skip_after_reload_terminal = 0usize;
    let mut skip_run_slot_exhausted = 0usize;

    for candidate in candidates {
        if pm_task_is_terminal_status(&candidate.status) || candidate.cancel_requested {
            skip_terminal_or_cancelled = skip_terminal_or_cancelled.saturating_add(1);
            skipped = skipped.saturating_add(1);
            continue;
        }

        if pm_task_deadline_elapsed(state.control_db(), &candidate.task_id)
            .await
            .map_err(|e| {
                format!(
                    "check pm task deadline failed: {}",
                    format_pm_runtime_error_detail(&e)
                )
            })?
        {
            let reason = "background PM research exceeded hard deadline before it could be resumed";
            let forced = force_finish_elapsed_pm_task_deadline(state, &candidate.task_id, reason)
                .await
                .map_err(|e| {
                    format!(
                        "force finish elapsed pm task deadline failed: {}",
                        format_pm_runtime_error_detail(&e)
                    )
                })?;
            task_manager
                .mark_execution_active(&candidate.task_id, false)
                .await;
            tracing::warn!(
                task_id = %candidate.task_id,
                session_id = %candidate.session_id,
                tenant_id = %candidate.tenant_id,
                forced,
                "pm runtime soft-delivered task whose hard deadline had already elapsed"
            );
            recovered = recovered.saturating_add(usize::from(forced));
            skipped = skipped.saturating_add(1);
            continue;
        }

        if task_manager.has_active_task(&candidate.task_id).await {
            if pm_task_deadline_elapsed(state.control_db(), &candidate.task_id)
                .await
                .map_err(|e| {
                    format!(
                        "check active pm task deadline failed: {}",
                        format_pm_runtime_error_detail(&e)
                    )
                })?
            {
                let reason =
                    "background PM research exceeded hard deadline while still marked active";
                let forced =
                    force_finish_elapsed_pm_task_deadline(state, &candidate.task_id, reason)
                        .await
                        .map_err(|e| {
                            format!(
                                "force finish active pm task deadline failed: {}",
                                format_pm_runtime_error_detail(&e)
                            )
                        })?;
                task_manager
                    .mark_execution_active(&candidate.task_id, false)
                    .await;
                tracing::warn!(
                    task_id = %candidate.task_id,
                    session_id = %candidate.session_id,
                    tenant_id = %candidate.tenant_id,
                    forced,
                    "pm runtime soft-delivered active task after hard deadline elapsed"
                );
                recovered = recovered.saturating_add(usize::from(forced));
                skipped = skipped.saturating_add(1);
                continue;
            }
            if let Some((status, stage, attempt, elapsed_ms, idle_ms)) =
                task_manager.active_task_diag(&candidate.task_id).await
            {
                tracing::debug!(
                    task_id = %candidate.task_id,
                    status = %status,
                    stage = ?stage,
                    attempt = ?attempt,
                    elapsed_ms,
                    idle_ms,
                    "skip pm task claim because task is still marked execution-active in memory"
                );
            }
            skip_already_active = skip_already_active.saturating_add(1);
            skipped = skipped.saturating_add(1);
            continue;
        }

        if task_manager
            .has_active_session_task(&candidate.session_id, &candidate.task_id)
            .await
            || has_running_pm_task_for_session(state.control_db(), &candidate)
                .await
                .map_err(|e| {
                    format!(
                        "check active pm session task failed: {}",
                        format_pm_runtime_error_detail(&e)
                    )
                })?
        {
            tracing::debug!(
                task_id = %candidate.task_id,
                session_id = %candidate.session_id,
                tenant_id = %candidate.tenant_id,
                user_id = %candidate.user_id,
                "skip pm task claim because another task for the same session is active"
            );
            skip_session_active = skip_session_active.saturating_add(1);
            skipped = skipped.saturating_add(1);
            continue;
        }

        let claimed = try_claim_pm_task_lease(
            state.control_db(),
            &candidate,
            &worker_id,
            config.lease_secs,
        )
        .await
        .map_err(|e| {
            format!(
                "claim task lease failed: {}",
                format_pm_runtime_error_detail(&e)
            )
        })?;
        if !claimed {
            skip_lease_conflict = skip_lease_conflict.saturating_add(1);
            skipped = skipped.saturating_add(1);
            continue;
        }

        let Some(row) = load_pm_task_runtime_row_from_db(state.control_db(), &candidate.task_id)
            .await
            .map_err(|e| {
                format!(
                    "reload claimed task failed: {}",
                    format_pm_runtime_error_detail(&e)
                )
            })?
        else {
            skip_missing_runtime_row = skip_missing_runtime_row.saturating_add(1);
            skipped = skipped.saturating_add(1);
            continue;
        };

        if row.cancel_requested || pm_task_is_terminal_status(&row.status) {
            let _ =
                release_pm_task_lease(state.control_db(), &row.task_id, &worker_id, false).await;
            skip_after_reload_terminal = skip_after_reload_terminal.saturating_add(1);
            skipped = skipped.saturating_add(1);
            continue;
        }

        task_manager.restore_task_from_runtime_row(&row).await;

        let Some(handle) = manager.get_session(&row.session_id).await else {
            let err = "session not found during pm runtime recovery".to_string();
            if let Err(error) = complete_pm_task_with_local_recovery(
                state,
                &row.task_id,
                &err,
                "runtime_session_missing_local_first_party_synthesis",
            )
            .await
            {
                tracing::warn!(
                    task_id = %row.task_id,
                    tenant_id = %row.tenant_id,
                    user_id = %row.user_id,
                    error = %format_pm_runtime_error_detail(&error),
                    "complete pm missing-session recovery failed"
                );
            }
            let _ =
                release_pm_task_lease(state.control_db(), &row.task_id, &worker_id, false).await;
            recovered = recovered.saturating_add(1);
            continue;
        };

        if handle.user_id != row.user_id || handle.tenant_id != row.tenant_id {
            let err = "session owner mismatch during pm runtime recovery".to_string();
            if let Err(error) = complete_pm_task_with_local_recovery(
                state,
                &row.task_id,
                &err,
                "runtime_owner_mismatch_local_first_party_synthesis",
            )
            .await
            {
                tracing::warn!(
                    task_id = %row.task_id,
                    tenant_id = %row.tenant_id,
                    user_id = %row.user_id,
                    error = %format_pm_runtime_error_detail(&error),
                    "complete pm owner-mismatch recovery failed"
                );
            }
            let _ =
                release_pm_task_lease(state.control_db(), &row.task_id, &worker_id, false).await;
            recovered = recovered.saturating_add(1);
            continue;
        }

        let run_slot = match task_manager.try_acquire_run_slot(&row.tenant_id).await {
            Ok(slot) => slot,
            Err(_) => {
                // Keep lease owner for a short grace window, so this node can pick it up
                // in the next cycle instead of competing with stale readers immediately.
                let _ =
                    release_pm_task_lease(state.control_db(), &row.task_id, &worker_id, true).await;
                skip_run_slot_exhausted = skip_run_slot_exhausted.saturating_add(1);
                skipped = skipped.saturating_add(1);
                continue;
            }
        };

        let resume_checkpoint = match load_pm_task_resume_context_from_db(
            state.control_db(),
            &row.task_id,
            &row.tenant_id,
            &row.user_id,
        )
        .await
        {
            Ok(opt) => opt.and_then(|(_, _, _, _, checkpoint)| checkpoint),
            Err(error) => {
                tracing::warn!(
                    task_id = %row.task_id,
                    tenant_id = %row.tenant_id,
                    user_id = %row.user_id,
                    error = %error,
                    error_debug = ?error,
                    "load resume checkpoint failed; continue without checkpoint"
                );
                None
            }
        };
        let claims = Claims::new(&row.user_id, "system@aos.local", "user", &row.tenant_id);
        let source = if handle.source.trim().is_empty() || handle.source.starts_with("pm_internal")
        {
            "pm".to_string()
        } else {
            handle.source.clone()
        };
        task_manager.mark_execution_active(&row.task_id, true).await;
        spawn_pm_research_task(
            state.clone(),
            claims,
            row.task_id.clone(),
            row.session_id.clone(),
            row.message.clone(),
            row.input_context.clone(),
            source,
            handle.model.clone(),
            run_slot,
            resume_checkpoint,
        );
        started = started.saturating_add(1);
    }

    tracing::info!(
        candidate_tasks = candidate_total,
        started_tasks = started,
        skipped_tasks = skipped,
        recovered_tasks = recovered,
        skip_terminal_or_cancelled,
        skip_already_active,
        skip_session_active,
        skip_lease_conflict,
        skip_missing_runtime_row,
        skip_after_reload_terminal,
        skip_run_slot_exhausted,
        worker_id = %worker_id,
        "pm runtime cycle completed"
    );
    Ok(())
}

pub(super) fn pm_flag_enabled(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}

pub(super) fn pm_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

pub(super) fn pm_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

pub(super) fn pm_preface_turn_timeout_secs() -> u64 {
    pm_env_u64(
        "PM_PREFACE_TURN_TIMEOUT_SECS",
        PM_PREFACE_TURN_TIMEOUT_DEFAULT_SECS,
    )
    .clamp(30, 300)
}

pub(super) fn pm_report_semantic_extract_timeout_secs() -> u64 {
    pm_env_u64("PM_REPORT_SEMANTIC_EXTRACT_TIMEOUT_SECS", 90).clamp(30, 240)
}

pub(super) fn pm_background_task_deadline_secs(budget: &PmTimeoutBudget) -> u64 {
    // pipeline_timeout_secs is the end-to-end budget, not one stage in a sum.
    // Adding preflight, retries, synthesis, editor, and another five-minute
    // grace made the advertised 480s profile run for 20+ minutes.
    let grace = pm_env_u64("PM_BACKGROUND_TASK_DEADLINE_GRACE_SECS", 30).clamp(30, 120);
    budget.pipeline_timeout_secs.saturating_add(grace).max(180)
}

pub(super) fn pm_direct_answer_turn_timeout_secs() -> u64 {
    pm_env_u64(
        "PM_DIRECT_ANSWER_TURN_TIMEOUT_SECS",
        PM_FORCE_SYNTH_TURN_TIMEOUT_DEFAULT_SECS,
    )
    .max(PM_FORCE_SYNTH_TURN_TIMEOUT_DEFAULT_SECS)
}

pub(super) fn pm_force_synth_turn_timeout_secs() -> u64 {
    pm_env_u64(
        "PM_FORCE_SYNTH_TURN_TIMEOUT_SECS",
        PM_FORCE_SYNTH_TURN_TIMEOUT_DEFAULT_SECS,
    )
}

pub(super) fn pm_contract_repair_turn_timeout_secs() -> u64 {
    pm_env_u64(
        "PM_CONTRACT_REPAIR_TURN_TIMEOUT_SECS",
        PM_CONTRACT_REPAIR_TURN_TIMEOUT_DEFAULT_SECS,
    )
    .max(PM_CONTRACT_REPAIR_TURN_TIMEOUT_DEFAULT_SECS)
}

pub(super) fn pm_contract_repair_max_retries() -> usize {
    pm_env_usize(
        "PM_CONTRACT_REPAIR_MAX_RETRIES",
        PM_CONTRACT_REPAIR_MAX_RETRIES_DEFAULT,
    )
    .clamp(5, 12)
}

pub(super) fn pm_timeout_recovery_wait_secs() -> u64 {
    pm_env_u64(
        "PM_TIMEOUT_RECOVERY_WAIT_SECS",
        PM_TIMEOUT_RECOVERY_WAIT_DEFAULT_SECS,
    )
    .clamp(3, 60)
}

#[derive(Debug, Clone)]
pub(super) struct PmEndpointCircuitState {
    pub(super) consecutive_failures: u32,
    pub(super) open_until: Option<Instant>,
}

impl PmEndpointCircuitState {
    pub(super) fn new() -> Self {
        Self {
            consecutive_failures: 0,
            open_until: None,
        }
    }
}

pub(super) fn pm_preflight_circuit_breakers(
) -> &'static Mutex<HashMap<String, PmEndpointCircuitState>> {
    static BREAKERS: OnceLock<Mutex<HashMap<String, PmEndpointCircuitState>>> = OnceLock::new();
    BREAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn pm_retrieve_circuit_breakers(
) -> &'static Mutex<HashMap<String, PmEndpointCircuitState>> {
    static BREAKERS: OnceLock<Mutex<HashMap<String, PmEndpointCircuitState>>> = OnceLock::new();
    BREAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn pm_domain_circuit_breakers() -> &'static Mutex<HashMap<String, PmEndpointCircuitState>>
{
    static BREAKERS: OnceLock<Mutex<HashMap<String, PmEndpointCircuitState>>> = OnceLock::new();
    BREAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn pm_retrieve_circuit_route_key(
    route_id: Option<&str>,
    route_channel: Option<&str>,
) -> Option<String> {
    pm_route_usage_key(route_id, route_channel).map(|key| format!("retrieve:{key}"))
}

pub(super) async fn pm_retrieve_circuit_allow(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    route_key: &str,
) -> Result<(), String> {
    let now = Instant::now();
    {
        let mut guard = pm_retrieve_circuit_breakers().lock().await;
        let state = guard
            .entry(route_key.to_string())
            .or_insert_with(PmEndpointCircuitState::new);
        if let Some(open_until) = state.open_until {
            if now < open_until {
                let remaining = open_until.duration_since(now).as_secs();
                return Err(format!("open_for_{}s", remaining));
            }
            // Cooldown elapsed: move to half-open (allow one probe).
            state.open_until = None;
            state.consecutive_failures = PM_RETRIEVE_CB_FAILURE_THRESHOLD.saturating_sub(1);
        }
    }
    if let Some(snapshot) = load_pm_route_circuit_state(db, tenant_id, route_key).await {
        if snapshot.remaining_open_secs > 0 {
            let mut guard = pm_retrieve_circuit_breakers().lock().await;
            let state = guard
                .entry(route_key.to_string())
                .or_insert_with(PmEndpointCircuitState::new);
            state.consecutive_failures = snapshot.consecutive_failures.max(1);
            state.open_until = Some(now + Duration::from_secs(snapshot.remaining_open_secs));
            return Err(format!("open_for_{}s", snapshot.remaining_open_secs));
        }
    }
    Ok(())
}

pub(super) async fn pm_retrieve_circuit_report(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    channel: Option<&str>,
    route_key: &str,
    success: bool,
    error_code: Option<&str>,
    error_message: Option<&str>,
) {
    let now = Instant::now();
    let mut guard = pm_retrieve_circuit_breakers().lock().await;
    let state = guard
        .entry(route_key.to_string())
        .or_insert_with(PmEndpointCircuitState::new);
    if success {
        state.consecutive_failures = 0;
        state.open_until = None;
        drop(guard);
        report_pm_route_circuit_success(db, tenant_id, route_key, channel).await;
        return;
    }
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    if state.consecutive_failures >= PM_RETRIEVE_CB_FAILURE_THRESHOLD {
        state.open_until = Some(now + Duration::from_secs(PM_RETRIEVE_CB_COOLDOWN_SECS));
    }
    drop(guard);
    report_pm_route_circuit_failure(
        db,
        tenant_id,
        route_key,
        channel,
        PM_RETRIEVE_CB_FAILURE_THRESHOLD,
        PM_RETRIEVE_CB_COOLDOWN_SECS,
        error_code,
        error_message,
    )
    .await;
}

pub(super) async fn pm_domain_circuit_report(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    domain_key: &str,
    success: bool,
    error_code: Option<&str>,
    error_message: Option<&str>,
) {
    let domain = domain_key.trim().to_ascii_lowercase();
    if domain.is_empty() {
        return;
    }
    let now = Instant::now();
    let mut guard = pm_domain_circuit_breakers().lock().await;
    let state = guard
        .entry(domain.clone())
        .or_insert_with(PmEndpointCircuitState::new);
    if success {
        state.consecutive_failures = 0;
        state.open_until = None;
        drop(guard);
        report_pm_domain_circuit_success(db, tenant_id, &domain).await;
        return;
    }
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    if state.consecutive_failures >= PM_DOMAIN_CB_FAILURE_THRESHOLD {
        state.open_until = Some(now + Duration::from_secs(PM_DOMAIN_CB_COOLDOWN_SECS));
    }
    drop(guard);
    report_pm_domain_circuit_failure(
        db,
        tenant_id,
        &domain,
        PM_DOMAIN_CB_FAILURE_THRESHOLD,
        PM_DOMAIN_CB_COOLDOWN_SECS,
        error_code,
        error_message,
    )
    .await;
}

fn unix_epoch_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn compute_retry_backoff_with_jitter_ms(run_id: &str, attempt: usize, now_ms: i64) -> (u64, u64) {
    let base_ms = pm_env_u64("PM_RETRY_BACKOFF_BASE_MS", PM_RETRY_BACKOFF_BASE_MS_DEFAULT).max(50);
    let max_ms =
        pm_env_u64("PM_RETRY_BACKOFF_MAX_MS", PM_RETRY_BACKOFF_MAX_MS_DEFAULT).max(base_ms);
    let jitter_window_ms = pm_env_u64("PM_RETRY_BACKOFF_JITTER_MS", 450);
    let exp_pow = u32::try_from(attempt.saturating_sub(2)).unwrap_or(u32::MAX);
    let capped_pow = exp_pow.min(6);
    let base_backoff = base_ms
        .saturating_mul(2u64.saturating_pow(capped_pow))
        .min(max_ms);
    let seed = format!("{}:{}:{}", run_id, attempt, now_ms);
    let digest = sha256_hex(&seed);
    let raw = u64::from_str_radix(&digest.chars().take(12).collect::<String>(), 16).unwrap_or(0);
    let jitter = if jitter_window_ms == 0 {
        0
    } else {
        raw % (jitter_window_ms + 1)
    };
    (base_backoff, jitter)
}

pub(super) async fn pm_apply_retry_governance_delay(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    run_id: &str,
    session_id: &str,
    attempt: usize,
) -> u64 {
    if attempt <= 1 {
        return 0;
    }
    let mut waited_ms: u64 = 0;
    let now_ms = unix_epoch_ms_now();
    if let Some(not_before_ms) = load_pm_retry_not_before_ms(db, tenant_id, run_id).await {
        if not_before_ms > now_ms {
            let delta = u64::try_from(not_before_ms.saturating_sub(now_ms)).unwrap_or(u64::MAX);
            let safe_delta = delta.min(15_000);
            if safe_delta > 0 {
                sleep(Duration::from_millis(safe_delta)).await;
                waited_ms = waited_ms.saturating_add(safe_delta);
            }
        }
    }
    let now_after_wait = unix_epoch_ms_now();
    let (base_backoff_ms, jitter_ms) =
        compute_retry_backoff_with_jitter_ms(run_id, attempt, now_after_wait);
    let delay_ms = base_backoff_ms.saturating_add(jitter_ms).min(20_000);
    let next_allowed_at_ms =
        now_after_wait.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX));
    upsert_pm_retry_not_before_ms(
        db,
        tenant_id,
        run_id,
        Some(session_id),
        attempt,
        base_backoff_ms,
        jitter_ms,
        next_allowed_at_ms,
    )
    .await;
    if delay_ms > 0 {
        sleep(Duration::from_millis(delay_ms)).await;
    }
    waited_ms.saturating_add(delay_ms)
}

pub(super) async fn resolve_pm_budget_snapshot(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
) -> (PmTimeoutBudget, PmRunConfigSnapshot) {
    let (profile_key, budget) =
        if let Some(profile_cfg) = get_pm_budget_profile_config(db, tenant_id).await {
            let profile = PmBudgetProfile::from_str(&profile_cfg.profile_key);
            let mut budget = PmTimeoutBudget::from_profile(profile);

            if profile_cfg.pipeline_timeout_secs > 0 {
                budget.pipeline_timeout_secs = profile_cfg.pipeline_timeout_secs;
            }
            if profile_cfg.max_attempts > 0 {
                budget.max_attempts = profile_cfg.max_attempts;
            }
            if profile_cfg.retrieve_max_tool_calls > 0 {
                budget.retrieve_max_tool_calls = profile_cfg.retrieve_max_tool_calls;
            }
            if profile_cfg.max_calls_per_source > 0 {
                budget.max_calls_per_source = profile_cfg.max_calls_per_source;
            }
            if profile_cfg.source_slot_search_secs > 0 {
                budget.source_slot_search_secs = profile_cfg.source_slot_search_secs;
            }
            if profile_cfg.source_slot_browser_secs > 0 {
                budget.source_slot_browser_secs = profile_cfg.source_slot_browser_secs;
            }
            if profile_cfg.source_slot_api_fetch_secs > 0 {
                budget.source_slot_api_fetch_secs = profile_cfg.source_slot_api_fetch_secs;
            }
            if profile_cfg.preflight_model_timeout_secs > 0 {
                budget.preflight_model_timeout_secs = profile_cfg.preflight_model_timeout_secs;
            }
            if profile_cfg.preflight_probe_timeout_secs > 0 {
                budget.preflight_probe_timeout_secs = profile_cfg.preflight_probe_timeout_secs;
            }
            if profile_cfg.preflight_overall_timeout_secs > 0 {
                budget.preflight_overall_timeout_secs = profile_cfg.preflight_overall_timeout_secs;
            }
            if profile_cfg.retry_step_budget_secs > 0 {
                budget.retry_step_budget_secs = profile_cfg.retry_step_budget_secs;
            }
            if profile_cfg.retry_total_budget_secs > 0 {
                budget.retry_total_budget_secs = profile_cfg.retry_total_budget_secs;
            }

            (profile_cfg.profile_key, budget)
        } else {
            let profile_key = get_pm_budget_profile(db, tenant_id)
                .await
                .unwrap_or_else(|| "normal".to_string());
            let profile = PmBudgetProfile::from_str(&profile_key);
            (profile_key, PmTimeoutBudget::from_profile(profile))
        };

    (
        budget,
        PmRunConfigSnapshot {
            budget_profile: profile_key,
            pipeline_timeout_secs: budget.pipeline_timeout_secs,
            deadline_timeout_secs: pm_background_task_deadline_secs(&budget),
            max_attempts: budget.max_attempts,
            source_slot_search_secs: budget.source_slot_search_secs,
            source_slot_browser_secs: budget.source_slot_browser_secs,
            source_slot_api_fetch_secs: budget.source_slot_api_fetch_secs,
            retrieve_max_tool_calls: budget.retrieve_max_tool_calls,
            max_calls_per_source: budget.max_calls_per_source,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_deadline_treats_pipeline_timeout_as_total_budget() {
        let budget = PmTimeoutBudget::baseline_for_profile(PmBudgetProfile::DeepResearch);
        let deadline = pm_background_task_deadline_secs(&budget);
        assert!(deadline >= budget.pipeline_timeout_secs + 30);
        assert!(deadline <= budget.pipeline_timeout_secs + 180);
        assert!(deadline < 900);
    }
}
