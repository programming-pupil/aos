//! Periodic PM scheduler for mission dispatch and runtime polling.

use std::time::Duration;

use sqlx::Row;
use tokio::time::{interval, MissedTickBehavior};

use crate::state::AppState;

const DEFAULT_PM_SCHEDULER_INTERVAL_SECS: u64 = 120;

pub fn start_periodic_pm_scheduler(
    state: AppState,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let interval_secs = std::env::var("PM_SCHEDULER_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PM_SCHEDULER_INTERVAL_SECS);
    let runtime_interval_secs = std::env::var("PM_RESEARCH_TASK_RUNTIME_POLL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);
    tracing::info!(
        "pm scheduler: governance_interval={}s runtime_interval={}s governance_enabled={} runtime_enabled={}",
        interval_secs,
        runtime_interval_secs,
        interval_secs > 0,
        runtime_interval_secs > 0
    );

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move {
        if interval_secs == 0 && runtime_interval_secs == 0 {
            tracing::warn!("PM_SCHEDULER_INTERVAL_SECS=0 and PM_RESEARCH_TASK_RUNTIME_POLL_SECS=0, PM scheduler disabled");
            let _ = shutdown_rx.changed().await;
            return;
        }
        let mut collection_ticker = interval(Duration::from_secs(interval_secs.max(1)));
        collection_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        collection_ticker.tick().await;
        let mut runtime_ticker = interval(Duration::from_secs(runtime_interval_secs.max(1)));
        runtime_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        runtime_ticker.tick().await;
        loop {
            tokio::select! {
                _ = collection_ticker.tick(), if interval_secs > 0 => {
                    if let Err(e) = run_collection_due_cycle(&state).await {
                        tracing::warn!(
                            error = %e,
                            error_debug = ?e,
                            "pm scheduler governance cycle failed"
                        );
                    }
                }
                _ = runtime_ticker.tick(), if runtime_interval_secs > 0 => {
                    if let Err(e) = crate::routes::agent::run_pm_background_runtime_cycle(&state).await {
                        tracing::warn!(
                            error = %e,
                            error_debug = ?e,
                            "pm background runtime cycle failed"
                        );
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("pm scheduler shutdown received");
                        break;
                    }
                }
            }
        }
    });

    (shutdown_tx, handle)
}

async fn run_collection_due_cycle(state: &AppState) -> Result<(), String> {
    let tenants = sqlx::query(
        "SELECT DISTINCT tenant_id
         FROM pm_missions
         WHERE tenant_id IS NOT NULL AND tenant_id <> ''",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("load tenants for governance rollup failed: {e}"))?;

    for t in tenants {
        let tenant_id = t.get::<String, _>(0);
        if let Err(e) = crate::routes::pm::dispatch_due_pm_missions(state, &tenant_id).await {
            tracing::warn!(
                tenant_id = %tenant_id,
                error = %e,
                "pm scheduler mission dispatch cycle failed"
            );
        }
    }
    Ok(())
}
