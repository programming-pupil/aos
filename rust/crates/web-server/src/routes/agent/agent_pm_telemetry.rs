use super::*;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;

use super::agent_pm_persist::{
    persist_pm_source_tool_ledger_batch_direct, PmSourceToolLedgerBatch,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PmTelemetryEvent {
    AnswerDelta {
        tenant_id: String,
        user_id: String,
        event: PmResearchTaskStreamEvent,
    },
    TaskEvent {
        tenant_id: String,
        user_id: String,
        sequence: u64,
        event: PmResearchTaskEvent,
    },
    StageAttempt {
        run_id: String,
        stage: String,
        attempt: usize,
        status: String,
        detail: Option<serde_json::Value>,
        elapsed_ms: Option<u64>,
        strategy: Option<String>,
        route: Option<String>,
        channel: Option<String>,
        variant: Option<String>,
    },
    SourceToolLedger {
        batch: PmSourceToolLedgerBatch,
    },
}

impl PmTelemetryEvent {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn stage_attempt(
        run_id: &str,
        stage: &str,
        attempt: usize,
        status: &str,
        detail: Option<serde_json::Value>,
        elapsed_ms: Option<u64>,
        strategy: Option<&str>,
        route: Option<&str>,
        channel: Option<&str>,
        variant: Option<&str>,
    ) -> Self {
        Self::StageAttempt {
            run_id: run_id.to_string(),
            stage: stage.to_string(),
            attempt,
            status: status.to_string(),
            detail,
            elapsed_ms,
            strategy: strategy.map(str::to_string),
            route: route.map(str::to_string),
            channel: channel.map(str::to_string),
            variant: variant.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalMode {
    Off,
    Buffered,
    Durable,
}

#[derive(Debug, Clone)]
struct WalRuntime {
    max_bytes: u64,
    max_segments: usize,
    replay_batch_segments: usize,
    io_lock: Arc<StdMutex<()>>,
}

impl WalRuntime {
    fn from_env() -> Self {
        Self {
            max_bytes: pm_env_u64("PM_TELEMETRY_WAL_MAX_BYTES", 256 * 1024 * 1024)
                .clamp(1024 * 1024, 100 * 1024 * 1024 * 1024),
            max_segments: pm_env_usize("PM_TELEMETRY_WAL_MAX_SEGMENTS", 4_096).clamp(32, 100_000),
            replay_batch_segments: pm_env_usize("PM_TELEMETRY_WAL_REPLAY_BATCH_SEGMENTS", 32)
                .clamp(1, 1_024),
            io_lock: Arc::new(StdMutex::new(())),
        }
    }
}

impl WalMode {
    fn from_env() -> Self {
        match env::var("PM_TELEMETRY_WAL_MODE")
            .unwrap_or_else(|_| "durable".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "none" | "disabled" => Self::Off,
            "buffered" | "async" => Self::Buffered,
            _ => Self::Durable,
        }
    }
}

#[derive(Default)]
struct PmTelemetryCounters {
    submitted: AtomicU64,
    committed: AtomicU64,
    overflow_spooled: AtomicU64,
    retry_count: AtomicU64,
    invalid_segments: AtomicU64,
    wal_pruned_segments: AtomicU64,
    wal_pruned_bytes: AtomicU64,
    wal_write_failures: AtomicU64,
    shed: AtomicU64,
}

pub(crate) struct PmTelemetrySink {
    sender: mpsc::Sender<PmTelemetryEvent>,
    overflow_sender: Option<mpsc::Sender<PmTelemetryEvent>>,
    counters: Arc<PmTelemetryCounters>,
}

impl PmTelemetrySink {
    pub(crate) async fn start(db: sqlx::SqlitePool, data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let queue_capacity = pm_env_usize("PM_TELEMETRY_QUEUE_CAPACITY", 2_048).clamp(128, 65_536);
        let flush_ms = pm_env_u64("PM_TELEMETRY_FLUSH_MS", 500).clamp(100, 5_000);
        let batch_size = pm_env_usize("PM_TELEMETRY_BATCH_SIZE", 128).clamp(8, 1_024);
        let wal_queue_capacity =
            pm_env_usize("PM_TELEMETRY_WAL_QUEUE_CAPACITY", 512).clamp(32, 16_384);
        let wal_mode = WalMode::from_env();
        let wal_runtime = WalRuntime::from_env();
        let spool_dir = data_dir.join("telemetry").join("pm-write-behind");
        if wal_mode != WalMode::Off {
            tokio::fs::create_dir_all(&spool_dir).await?;
        }
        let (sender, receiver) = mpsc::channel(queue_capacity);
        let counters = Arc::new(PmTelemetryCounters::default());
        let overflow_sender = if wal_mode == WalMode::Off {
            None
        } else {
            let (wal_sender, wal_receiver) = mpsc::channel(wal_queue_capacity);
            tokio::spawn(run_pm_wal_spooler(
                wal_receiver,
                spool_dir.clone(),
                wal_mode,
                batch_size,
                wal_runtime.clone(),
                counters.clone(),
            ));
            Some(wal_sender)
        };
        let sink = Arc::new(Self {
            sender,
            overflow_sender,
            counters: counters.clone(),
        });
        let retention_db = db.clone();
        tokio::spawn(run_pm_telemetry_writer(
            db,
            receiver,
            spool_dir,
            wal_mode,
            Duration::from_millis(flush_ms),
            batch_size,
            wal_runtime.clone(),
            counters,
        ));
        tokio::spawn(run_pm_telemetry_retention(retention_db));
        tracing::info!(
            queue_capacity,
            flush_ms,
            batch_size,
            wal_max_bytes = wal_runtime.max_bytes,
            wal_max_segments = wal_runtime.max_segments,
            wal_replay_batch_segments = wal_runtime.replay_batch_segments,
            wal_mode = ?wal_mode,
            "PM bounded telemetry writer started"
        );
        Ok(sink)
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Arc<Self> {
        let (sender, receiver) = mpsc::channel(8);
        drop(receiver);
        Arc::new(Self {
            sender,
            overflow_sender: None,
            counters: Arc::new(PmTelemetryCounters::default()),
        })
    }

    pub(crate) async fn enqueue(&self, event: PmTelemetryEvent) {
        self.try_enqueue(event);
    }

    pub(crate) fn try_enqueue(&self, event: PmTelemetryEvent) {
        self.counters.submitted.fetch_add(1, Ordering::Relaxed);
        match self.sender.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(event)) => {
                self.enqueue_overflow(event, "closed");
            }
            Err(mpsc::error::TrySendError::Full(event)) => {
                self.enqueue_overflow(event, "full");
            }
        }
    }

    fn enqueue_overflow(&self, event: PmTelemetryEvent, reason: &'static str) {
        let Some(sender) = self.overflow_sender.as_ref() else {
            self.counters.shed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                reason,
                "PM telemetry queue unavailable; optional event was shed"
            );
            return;
        };
        match sender.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.counters.shed.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    reason,
                    "PM telemetry and WAL queues are full; optional event was shed"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.counters.shed.fetch_add(1, Ordering::Relaxed);
                tracing::error!(
                    reason,
                    "PM telemetry WAL spooler is closed; optional event was shed"
                );
            }
        }
    }
}

async fn run_pm_wal_spooler(
    mut receiver: mpsc::Receiver<PmTelemetryEvent>,
    spool_dir: PathBuf,
    wal_mode: WalMode,
    batch_size: usize,
    wal_runtime: WalRuntime,
    counters: Arc<PmTelemetryCounters>,
) {
    let mut segment_seq = 0u64;
    while let Some(first) = receiver.recv().await {
        let mut events = vec![first];
        while events.len() < batch_size {
            match receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(_) => break,
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = spool_dir.join(format!("{now:032}-{segment_seq:016}-overflow.json"));
        segment_seq = segment_seq.wrapping_add(1);
        let mut retry_delay = Duration::from_millis(100);
        let mut written = false;
        for attempt in 1..=3 {
            match write_wal_segment(
                path.clone(),
                events.clone(),
                wal_mode,
                wal_runtime.clone(),
                counters.clone(),
            )
            .await
            {
                Ok(()) => {
                    counters
                        .overflow_spooled
                        .fetch_add(events.len() as u64, Ordering::Relaxed);
                    written = true;
                    break;
                }
                Err(error) => {
                    counters.retry_count.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        error = %error,
                        attempt,
                        retry_delay_ms = retry_delay.as_millis() as u64,
                        "PM telemetry overflow WAL write failed; retrying"
                    );
                    if attempt < 3 {
                        tokio::time::sleep(retry_delay).await;
                    }
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(15));
                }
            }
        }
        if !written {
            counters
                .shed
                .fetch_add(events.len() as u64, Ordering::Relaxed);
            tracing::error!(
                event_count = events.len(),
                "PM telemetry overflow WAL batch was shed after bounded disk retries"
            );
        }
    }
}

async fn run_pm_telemetry_retention(db: sqlx::SqlitePool) {
    let raw_days = pm_env_u64("PM_TELEMETRY_RAW_RETENTION_DAYS", 7).clamp(1, 3650);
    let event_days = pm_env_u64("PM_TELEMETRY_EVENT_RETENTION_DAYS", 30).clamp(1, 3650);
    let batch_size = pm_env_usize("PM_TELEMETRY_RETENTION_BATCH_SIZE", 500).clamp(50, 5_000);
    let mut ticker = tokio::time::interval(Duration::from_secs(3_600));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    tokio::time::sleep(Duration::from_secs(300)).await;
    loop {
        apply_pm_telemetry_retention_batch(&db, raw_days, event_days, batch_size).await;
        ticker.tick().await;
    }
}

async fn apply_pm_telemetry_retention_batch(
    db: &sqlx::SqlitePool,
    raw_days: u64,
    event_days: u64,
    batch_size: usize,
) {
    let raw_result = sqlx::query(
        "UPDATE pm_research_tool_call_ledger
             SET input_raw = NULL, output_raw = NULL
             WHERE id IN (
                 SELECT id FROM pm_research_tool_call_ledger
                 WHERE created_at < datetime(CURRENT_TIMESTAMP, printf('%+d days', ?))
                   AND (input_raw IS NOT NULL OR output_raw IS NOT NULL)
                 ORDER BY created_at ASC, id ASC
                 LIMIT ?
             )",
    )
    .bind(-i64::try_from(raw_days).unwrap_or(i64::MAX))
    .bind(i64::try_from(batch_size).unwrap_or(i64::MAX))
    .execute(db)
    .await;
    let stream_result = sqlx::query(
        "DELETE FROM pm_research_task_stream_events
             WHERE id IN (
                 SELECT id FROM pm_research_task_stream_events
                 WHERE created_at < datetime(CURRENT_TIMESTAMP, printf('%+d days', ?))
                 ORDER BY created_at ASC, id ASC
                 LIMIT ?
             )",
    )
    .bind(-i64::try_from(event_days).unwrap_or(i64::MAX))
    .bind(i64::try_from(batch_size).unwrap_or(i64::MAX))
    .execute(db)
    .await;
    let ledger_result = sqlx::query(
        "DELETE FROM pm_research_tool_call_ledger
             WHERE id IN (
                 SELECT id FROM pm_research_tool_call_ledger
                 WHERE created_at < datetime(CURRENT_TIMESTAMP, printf('%+d days', ?))
                 ORDER BY created_at ASC, id ASC
                 LIMIT ?
             )",
    )
    .bind(-i64::try_from(event_days).unwrap_or(i64::MAX))
    .bind(i64::try_from(batch_size).unwrap_or(i64::MAX))
    .execute(db)
    .await;
    let task_event_result = sqlx::query(
        "DELETE FROM pm_research_task_events
             WHERE id IN (
                 SELECT id FROM pm_research_task_events
                 WHERE created_at < datetime(CURRENT_TIMESTAMP, printf('%+d days', ?))
                 ORDER BY created_at ASC, id ASC
                 LIMIT ?
             )",
    )
    .bind(-i64::try_from(event_days).unwrap_or(i64::MAX))
    .bind(i64::try_from(batch_size).unwrap_or(i64::MAX))
    .execute(db)
    .await;
    for (operation, result) in [
        ("raw_compaction", raw_result),
        ("answer_delta_retention", stream_result),
        ("tool_ledger_retention", ledger_result),
        ("task_event_retention", task_event_result),
    ] {
        match result {
            Ok(result) if result.rows_affected() > 0 => tracing::info!(
                operation,
                rows_affected = result.rows_affected(),
                "PM telemetry retention batch completed"
            ),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(operation, error = %error, "PM telemetry retention batch failed")
            }
        }
    }
}

async fn run_pm_telemetry_writer(
    db: sqlx::SqlitePool,
    mut receiver: mpsc::Receiver<PmTelemetryEvent>,
    spool_dir: PathBuf,
    wal_mode: WalMode,
    flush_interval: Duration,
    batch_size: usize,
    wal_runtime: WalRuntime,
    counters: Arc<PmTelemetryCounters>,
) {
    replay_wal_segments(
        &db,
        &spool_dir,
        wal_mode,
        wal_runtime.replay_batch_segments,
        &counters,
    )
    .await;
    let mut report_tick = tokio::time::interval(Duration::from_secs(60));
    report_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    report_tick.tick().await;

    loop {
        let first = tokio::select! {
            event = receiver.recv() => event,
            _ = report_tick.tick() => {
                report_pm_telemetry_counters(&receiver, &counters);
                replay_wal_segments(
                    &db,
                    &spool_dir,
                    wal_mode,
                    wal_runtime.replay_batch_segments,
                    &counters,
                ).await;
                continue;
            }
        };
        let Some(first) = first else {
            break;
        };
        let mut events = vec![first];
        let deadline = tokio::time::Instant::now() + flush_interval;
        while events.len() < batch_size {
            match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Ok(Some(event)) => events.push(event),
                Ok(None) | Err(_) => break,
            }
        }

        let segment_path = if wal_mode == WalMode::Off {
            None
        } else {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Some(spool_dir.join(format!("{now:032}-writer.json")))
        };
        if let Some(path) = segment_path.as_ref() {
            if let Err(error) = write_wal_segment(
                path.clone(),
                events.clone(),
                wal_mode,
                wal_runtime.clone(),
                counters.clone(),
            )
            .await
            {
                // WAL is an additional durability layer. A local disk failure
                // must not silently discard a batch that the database can still accept.
                tracing::error!(
                    error = %error,
                    "failed to write PM telemetry WAL segment; attempting telemetry lane directly"
                );
            }
        }

        let mut retry_delay = Duration::from_millis(250);
        loop {
            match persist_pm_telemetry_batch(&db, &events).await {
                Ok(()) => {
                    counters
                        .committed
                        .fetch_add(events.len() as u64, Ordering::Relaxed);
                    if let Some(path) = segment_path.as_ref() {
                        if let Err(error) = tokio::fs::remove_file(path).await {
                            tracing::warn!(path = %path.display(), error = %error, "failed to acknowledge PM telemetry WAL segment");
                        }
                    }
                    break;
                }
                Err(error) => {
                    counters.retry_count.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        event_count = events.len(),
                        retry_delay_ms = retry_delay.as_millis() as u64,
                        error = %error,
                        "PM telemetry batch failed; retaining WAL and retrying"
                    );
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(15));
                }
            }
        }
        replay_wal_segments(&db, &spool_dir, wal_mode, 4, &counters).await;
    }
}

fn report_pm_telemetry_counters(
    receiver: &mpsc::Receiver<PmTelemetryEvent>,
    counters: &PmTelemetryCounters,
) {
    tracing::info!(
        queued = receiver.len(),
        queue_capacity = receiver.max_capacity(),
        submitted = counters.submitted.load(Ordering::Relaxed),
        committed = counters.committed.load(Ordering::Relaxed),
        overflow_spooled = counters.overflow_spooled.load(Ordering::Relaxed),
        retry_count = counters.retry_count.load(Ordering::Relaxed),
        invalid_segments = counters.invalid_segments.load(Ordering::Relaxed),
        wal_pruned_segments = counters.wal_pruned_segments.load(Ordering::Relaxed),
        wal_pruned_bytes = counters.wal_pruned_bytes.load(Ordering::Relaxed),
        wal_write_failures = counters.wal_write_failures.load(Ordering::Relaxed),
        shed = counters.shed.load(Ordering::Relaxed),
        "PM telemetry writer health"
    );
}

async fn write_wal_segment(
    path: PathBuf,
    events: Vec<PmTelemetryEvent>,
    wal_mode: WalMode,
    wal_runtime: WalRuntime,
    counters: Arc<PmTelemetryCounters>,
) -> anyhow::Result<()> {
    if wal_mode == WalMode::Off {
        return Ok(());
    }
    let write_counters = counters.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let raw = serde_json::to_string(&events)?;
        let protected =
            runtime::protect_sensitive_text(&raw, runtime::configured_data_protection_mode()).value;
        // Validate after redaction before making the segment visible to replay.
        let _: Vec<PmTelemetryEvent> = serde_json::from_str(&protected)?;
        let incoming_bytes = u64::try_from(protected.len()).unwrap_or(u64::MAX);
        if incoming_bytes > wal_runtime.max_bytes {
            anyhow::bail!(
                "telemetry WAL segment is {} bytes, above PM_TELEMETRY_WAL_MAX_BYTES={}",
                incoming_bytes,
                wal_runtime.max_bytes
            );
        }
        let _guard = wal_runtime
            .io_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (pruned_segments, pruned_bytes) = prune_wal_segments_for_write(
            path.parent().unwrap_or_else(|| Path::new(".")),
            incoming_bytes,
            wal_runtime.max_bytes,
            wal_runtime.max_segments,
        )?;
        write_counters
            .wal_pruned_segments
            .fetch_add(pruned_segments, Ordering::Relaxed);
        write_counters
            .wal_pruned_bytes
            .fetch_add(pruned_bytes, Ordering::Relaxed);
        if pruned_segments > 0 {
            tracing::warn!(
                pruned_segments,
                pruned_bytes,
                wal_max_bytes = wal_runtime.max_bytes,
                wal_max_segments = wal_runtime.max_segments,
                "PM telemetry WAL capacity reached; pruned oldest optional telemetry"
            );
        }
        let temp = path.with_extension("tmp");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)?;
        file.write_all(protected.as_bytes())?;
        file.flush()?;
        if wal_mode == WalMode::Durable {
            file.sync_data()?;
        }
        drop(file);
        std::fs::rename(temp, path)?;
        Ok(())
    })
    .await
    .map_err(|error| anyhow::anyhow!("telemetry WAL writer task failed: {error}"))?
    .inspect_err(|_| {
        counters.wal_write_failures.fetch_add(1, Ordering::Relaxed);
    })?;
    Ok(())
}

fn prune_wal_segments_for_write(
    spool_dir: &Path,
    incoming_bytes: u64,
    max_bytes: u64,
    max_segments: usize,
) -> std::io::Result<(u64, u64)> {
    let mut segments = Vec::<(PathBuf, u64, SystemTime)>::new();
    for entry in std::fs::read_dir(spool_dir)? {
        let entry = entry?;
        let path = entry.path();
        let extension = path.extension().and_then(|value| value.to_str());
        // Crash-left temporary files count toward the same hard WAL bound.
        // The shared WAL lock prevents pruning a file being written here.
        if !matches!(extension, Some("json" | "corrupt" | "tmp")) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        segments.push((
            path,
            metadata.len(),
            metadata.modified().unwrap_or(UNIX_EPOCH),
        ));
    }
    segments.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    let mut total_bytes = segments
        .iter()
        .fold(0u64, |total, (_, bytes, _)| total.saturating_add(*bytes));
    let mut remaining_segments = segments.len();
    let mut pruned_segments = 0u64;
    let mut pruned_bytes = 0u64;
    for (path, bytes, _) in segments {
        if remaining_segments < max_segments
            && total_bytes.saturating_add(incoming_bytes) <= max_bytes
        {
            break;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                remaining_segments = remaining_segments.saturating_sub(1);
                total_bytes = total_bytes.saturating_sub(bytes);
                pruned_segments = pruned_segments.saturating_add(1);
                pruned_bytes = pruned_bytes.saturating_add(bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                remaining_segments = remaining_segments.saturating_sub(1);
                total_bytes = total_bytes.saturating_sub(bytes);
            }
            Err(error) => return Err(error),
        }
    }
    Ok((pruned_segments, pruned_bytes))
}

async fn replay_wal_segments(
    db: &sqlx::SqlitePool,
    spool_dir: &Path,
    wal_mode: WalMode,
    max_segments: usize,
    counters: &PmTelemetryCounters,
) {
    if wal_mode == WalMode::Off {
        return;
    }
    let Ok(mut entries) = tokio::fs::read_dir(spool_dir).await else {
        return;
    };
    let mut paths = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths.into_iter().take(max_segments.max(1)) {
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %error, "failed to read PM telemetry WAL segment");
                continue;
            }
        };
        let events = match serde_json::from_str::<Vec<PmTelemetryEvent>>(&raw) {
            Ok(events) => events,
            Err(error) => {
                counters.invalid_segments.fetch_add(1, Ordering::Relaxed);
                let corrupt = path.with_extension("corrupt");
                let _ = tokio::fs::rename(&path, &corrupt).await;
                tracing::error!(path = %path.display(), error = %error, "quarantined invalid PM telemetry WAL segment");
                continue;
            }
        };
        match persist_pm_telemetry_batch(db, &events).await {
            Ok(()) => {
                counters
                    .committed
                    .fetch_add(events.len() as u64, Ordering::Relaxed);
                let _ = tokio::fs::remove_file(&path).await;
            }
            Err(error) => {
                counters.retry_count.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(path = %path.display(), error = %error, "PM telemetry WAL replay deferred");
                break;
            }
        }
    }
}

async fn persist_pm_telemetry_batch(
    db: &sqlx::SqlitePool,
    events: &[PmTelemetryEvent],
) -> Result<(), sqlx::Error> {
    let mut answer_deltas = Vec::new();
    let mut task_events = Vec::new();
    let mut source_ledgers = Vec::new();
    let mut stage_attempts = Vec::new();
    for event in events {
        match event {
            PmTelemetryEvent::AnswerDelta {
                tenant_id,
                user_id,
                event,
            } => answer_deltas.push((tenant_id, user_id, event)),
            PmTelemetryEvent::TaskEvent {
                tenant_id,
                user_id,
                sequence,
                event,
            } => task_events.push((tenant_id, user_id, *sequence, event)),
            PmTelemetryEvent::StageAttempt { .. } => stage_attempts.push(event),
            PmTelemetryEvent::SourceToolLedger { batch } => source_ledgers.push(batch.clone()),
        }
    }
    persist_answer_delta_batch(db, &answer_deltas).await?;
    persist_task_event_batch(db, &task_events).await?;
    let stage_attempt_writes = stage_attempts
        .iter()
        .filter_map(|event| {
            let PmTelemetryEvent::StageAttempt {
                run_id,
                stage,
                attempt,
                status,
                detail,
                elapsed_ms,
                strategy,
                route,
                channel,
                variant,
            } = event
            else {
                return None;
            };
            Some(pm_orchestrator::persistence::PmStageAttemptWrite {
                run_id,
                stage,
                attempt: *attempt,
                status,
                detail: detail.as_ref(),
                elapsed_ms: *elapsed_ms,
                strategy: strategy.as_deref(),
                route: route.as_deref(),
                channel: channel.as_deref(),
                variant: variant.as_deref(),
            })
        })
        .collect::<Vec<_>>();
    pm_orchestrator::persistence::persist_pm_stage_attempt_batch_result(db, &stage_attempt_writes)
        .await?;
    for mut batch in source_ledgers {
        persist_pm_source_tool_ledger_batch_direct(db, &mut batch).await?;
    }
    Ok(())
}

async fn persist_task_event_batch(
    db: &sqlx::SqlitePool,
    rows: &[(&String, &String, u64, &PmResearchTaskEvent)],
) -> Result<(), sqlx::Error> {
    for chunk in rows.chunks(100) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO pm_research_task_events
                (task_id, tenant_id, user_id, seq, status, stage, attempt, message,
                 elapsed_ms, stage_elapsed_ms, detail_json, response_json, error_message) ",
        );
        query.push_values(
            chunk,
            |mut values, (tenant_id, user_id, sequence, event)| {
                values
                    .push_bind(&event.task_id)
                    .push_bind(tenant_id)
                    .push_bind(user_id)
                    .push_bind(i64::try_from(*sequence).unwrap_or(i64::MAX))
                    .push_bind(&event.status)
                    .push_bind(event.stage.as_deref())
                    .push_bind(event.attempt.and_then(|value| i32::try_from(value).ok()))
                    .push_bind(event.message.as_deref())
                    .push_bind(i64::try_from(event.elapsed_ms).unwrap_or(i64::MAX))
                    .push_bind(
                        event
                            .stage_elapsed_ms
                            .and_then(|value| i64::try_from(value).ok()),
                    )
                    .push_bind(event.detail.as_ref().map(serde_json::Value::to_string))
                    .push_bind(event.response.as_ref().map(serde_json::Value::to_string))
                    .push_bind(event.error.as_deref());
            },
        );
        query.push(
            " ON CONFLICT DO UPDATE SET status = excluded.status, stage = excluded.stage,
                attempt = excluded.attempt, message = excluded.message,
                elapsed_ms = excluded.elapsed_ms, stage_elapsed_ms = excluded.stage_elapsed_ms,
                detail_json = excluded.detail_json, response_json = excluded.response_json,
                error_message = excluded.error_message",
        );
        query.build().execute(db).await?;
    }
    Ok(())
}

async fn persist_answer_delta_batch(
    db: &sqlx::SqlitePool,
    rows: &[(&String, &String, &PmResearchTaskStreamEvent)],
) -> Result<(), sqlx::Error> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut coalesced = Vec::<(String, String, PmResearchTaskStreamEvent)>::new();
    for (tenant_id, user_id, event) in rows {
        if let Some((last_tenant, last_user, last)) = coalesced.last_mut() {
            if last_tenant == *tenant_id
                && last_user == *user_id
                && last.task_id == event.task_id
                && last.session_id == event.session_id
                && last.stage == event.stage
            {
                last.delta.push_str(&event.delta);
                last.sequence = last.sequence.max(event.sequence);
                continue;
            }
        }
        coalesced.push(((*tenant_id).clone(), (*user_id).clone(), (*event).clone()));
    }

    for chunk in coalesced.chunks(100) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO pm_research_task_stream_events
                (task_id, tenant_id, user_id, session_id, seq, stage, delta) ",
        );
        query.push_values(chunk, |mut values, (tenant_id, user_id, event)| {
            values
                .push_bind(&event.task_id)
                .push_bind(tenant_id)
                .push_bind(user_id)
                .push_bind(&event.session_id)
                .push_bind(i64::try_from(event.sequence).unwrap_or(i64::MAX))
                .push_bind(&event.stage)
                .push_bind(&event.delta);
        });
        query.push(" ON CONFLICT DO UPDATE SET stage = excluded.stage, delta = excluded.delta");
        query.build().execute(db).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_retention_compacts_and_deletes_bounded_old_rows() {
        let db = crate::test_sqlite_pool().await;
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_research_tool_call_ledger
               (run_id, call_seq, tool_name, input_raw, output_raw, created_at)
             VALUES
               ('raw-old', 1, 'test', 'input', 'output', datetime(CURRENT_TIMESTAMP, '-10 days')),
               ('event-old', 1, 'test', 'input', 'output', datetime(CURRENT_TIMESTAMP, '-40 days')),
               ('recent', 1, 'test', 'input', 'output', CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("insert retention ledger fixtures");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_research_task_stream_events
               (task_id, tenant_id, user_id, session_id, seq, stage, delta, created_at)
             VALUES
               ('old', 'tenant', 'user', 'session', 1, 'test', 'old', datetime(CURRENT_TIMESTAMP, '-40 days')),
               ('recent', 'tenant', 'user', 'session', 1, 'test', 'recent', CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("insert retention stream fixtures");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_research_task_events
               (task_id, tenant_id, user_id, seq, status, created_at)
             VALUES
               ('old', 'tenant', 'user', 1, 'running', datetime(CURRENT_TIMESTAMP, '-40 days')),
               ('recent', 'tenant', 'user', 1, 'running', CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("insert retention task event fixtures");

        apply_pm_telemetry_retention_batch(&db, 7, 30, 100).await;

        let raw_old: (Option<String>, Option<String>) = sqlx::query_as::<sqlx::Sqlite, _>(
            "SELECT input_raw, output_raw FROM pm_research_tool_call_ledger
             WHERE run_id = 'raw-old'",
        )
        .fetch_one(&db)
        .await
        .expect("load compacted raw fixture");
        assert_eq!(raw_old, (None, None));
        let ledger_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pm_research_tool_call_ledger")
                .fetch_one(&db)
                .await
                .expect("count retained ledger rows");
        let stream_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pm_research_task_stream_events")
                .fetch_one(&db)
                .await
                .expect("count retained stream rows");
        let task_event_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pm_research_task_events")
                .fetch_one(&db)
                .await
                .expect("count retained task event rows");
        assert_eq!(ledger_count, 2);
        assert_eq!(stream_count, 1);
        assert_eq!(task_event_count, 1);
        db.close().await;
    }

    fn answer_event(sequence: u64) -> PmTelemetryEvent {
        PmTelemetryEvent::AnswerDelta {
            tenant_id: "tenant".to_string(),
            user_id: "user".to_string(),
            event: PmResearchTaskStreamEvent {
                task_id: "task".to_string(),
                session_id: "session".to_string(),
                stage: "synthesize".to_string(),
                sequence,
                delta: "answer".to_string(),
            },
        }
    }

    #[test]
    fn answer_deltas_and_tool_batches_round_trip_through_wal_json() {
        let event = answer_event(4);
        let task_event = PmTelemetryEvent::TaskEvent {
            tenant_id: "tenant".to_string(),
            user_id: "user".to_string(),
            sequence: 2,
            event: PmResearchTaskEvent {
                task_id: "task".to_string(),
                session_id: "session".to_string(),
                status: "running".to_string(),
                stage: Some("research".to_string()),
                attempt: Some(1),
                message: Some("researching".to_string()),
                elapsed_ms: 50,
                stage_elapsed_ms: Some(25),
                detail: None,
                response: None,
                error: None,
            },
        };
        let stage_event = PmTelemetryEvent::stage_attempt(
            "run",
            "retrieve",
            1,
            "running",
            Some(serde_json::json!({"route": "native_search"})),
            None,
            None,
            None,
            None,
            None,
        );
        let raw = serde_json::to_string(&vec![event, task_event, stage_event])
            .expect("serialize telemetry");
        let decoded =
            serde_json::from_str::<Vec<PmTelemetryEvent>>(&raw).expect("deserialize telemetry");
        assert_eq!(decoded.len(), 3);
    }

    #[tokio::test]
    async fn saturated_telemetry_and_wal_queues_never_block_callers() {
        let (sender, receiver) = mpsc::channel(1);
        sender.try_send(answer_event(1)).expect("fill main queue");
        let (overflow_sender, overflow_receiver) = mpsc::channel(1);
        overflow_sender
            .try_send(answer_event(2))
            .expect("fill WAL queue");
        let counters = Arc::new(PmTelemetryCounters::default());
        let sink = PmTelemetrySink {
            sender,
            overflow_sender: Some(overflow_sender),
            counters: counters.clone(),
        };

        tokio::time::timeout(Duration::from_millis(50), sink.enqueue(answer_event(3)))
            .await
            .expect("bounded telemetry enqueue must not wait for either queue");
        assert_eq!(counters.shed.load(Ordering::Relaxed), 1);
        drop(receiver);
        drop(overflow_receiver);
    }

    #[test]
    fn wal_pruning_reserves_capacity_for_the_newest_segment() {
        let dir = std::env::temp_dir().join(format!(
            "aos-pm-wal-prune-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create WAL test directory");
        for index in 0..3 {
            std::fs::write(dir.join(format!("{index:02}.json")), b"1234")
                .expect("write WAL test segment");
        }

        let (segments, bytes) =
            prune_wal_segments_for_write(&dir, 4, 8, 2).expect("prune WAL segments");
        assert_eq!(segments, 2);
        assert_eq!(bytes, 8);
        assert_eq!(
            std::fs::read_dir(&dir)
                .expect("list WAL test directory")
                .count(),
            1
        );
        std::fs::remove_dir_all(dir).expect("remove WAL test directory");
    }

    #[test]
    fn wal_pruning_counts_crash_left_temporary_segments() {
        let dir = std::env::temp_dir().join(format!(
            "aos-pm-wal-tmp-prune-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create WAL test directory");
        std::fs::write(dir.join("old.tmp"), b"12345678").expect("write temporary WAL segment");

        let (segments, bytes) =
            prune_wal_segments_for_write(&dir, 4, 8, 2).expect("prune temporary WAL segment");
        assert_eq!(segments, 1);
        assert_eq!(bytes, 8);
        assert_eq!(
            std::fs::read_dir(&dir)
                .expect("list WAL test directory")
                .count(),
            0
        );
        std::fs::remove_dir_all(dir).expect("remove WAL test directory");
    }
}
