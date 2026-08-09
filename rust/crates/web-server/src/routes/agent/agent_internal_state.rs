use super::*;

pub(super) type PmAnswerDeltaCallback = Arc<dyn Fn(&str, String) + Send + Sync>;

const DEFAULT_PM_WORKER_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

fn pm_worker_stack_size_bytes() -> usize {
    env::var("PM_WORKER_STACK_SIZE_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 2 * 1024 * 1024)
        .unwrap_or(DEFAULT_PM_WORKER_STACK_SIZE_BYTES)
}

fn pm_worker_threads() -> usize {
    env::var("PM_WORKER_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
        .min(8)
}

fn pm_worker_mode() -> String {
    env::var("PM_RESEARCH_TASK_WORKER_MODE")
        .unwrap_or_else(|_| "embedded".to_string())
        .trim()
        .to_ascii_lowercase()
}

pub(super) fn pm_background_runtime_claims_enabled_in_this_process() -> bool {
    let mode = pm_worker_mode();
    let external_mode = matches!(
        mode.as_str(),
        "external" | "process" | "standalone" | "worker"
    );
    let is_worker_process = env::var("AOS_PM_WORKER_PROCESS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false);
    !external_mode || is_worker_process
}

pub(super) fn pm_background_worker_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let worker_threads = pm_worker_threads();
        let stack_size = pm_worker_stack_size_bytes();
        tracing::info!(
            worker_threads,
            stack_size_bytes = stack_size,
            "starting dedicated PM background worker runtime"
        );
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(worker_threads)
            .thread_name("aos-pm-worker")
            .thread_stack_size(stack_size)
            .build()
            .expect("failed to build PM background worker runtime")
    })
}

#[derive(Debug, Clone)]
pub(super) struct PmProbeOutcome {
    pub(super) variant: String,
    pub(super) route_id: Option<String>,
    pub(super) route_channel: Option<String>,
    pub(super) subtask_key: Option<String>,
    pub(super) subtask_id: Option<String>,
    pub(super) subtask_title: Option<String>,
    pub(super) subtask_goal: Option<String>,
    pub(super) subtask_deliverable: Option<String>,
    pub(super) subtask_required_evidence_type: Option<String>,
    pub(super) subtask_priority: Option<String>,
    pub(super) elapsed_ms: Option<u64>,
    pub(super) turn: Option<TurnResult>,
    pub(super) diagnostic_turn: Option<TurnResult>,
    pub(super) quality: Option<PmAnswerQualityDto>,
    pub(super) error: Option<String>,
}

impl pm_domain::subtask_runtime::PmSubtaskOutcomeLike for PmProbeOutcome {
    fn subtask_key(&self) -> Option<&str> {
        self.subtask_key.as_deref()
    }

    fn subtask_id(&self) -> Option<&str> {
        self.subtask_id.as_deref()
    }

    fn subtask_title(&self) -> Option<&str> {
        self.subtask_title.as_deref()
    }
}

#[derive(Debug, Clone)]
pub(super) struct PmResearchTaskRecord {
    pub(super) tenant_id: String,
    pub(super) user_id: String,
    pub(super) session_id: String,
    pub(super) message: String,
    pub(super) input_context: Option<PmTaskInputContext>,
    pub(super) created_at: Instant,
    pub(super) last_update_at: Instant,
    pub(super) stage_started_at: Instant,
    pub(super) completed_at: Option<Instant>,
    pub(super) execution_active: bool,
    pub(super) done: bool,
    pub(super) cancel_requested: bool,
    pub(super) last_event: PmResearchTaskEvent,
    pub(super) event_seq: u64,
    pub(super) answer_stream_seq: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PmResearchTaskConfig {
    pub(super) max_concurrent_running: usize,
    pub(super) max_concurrent_per_tenant: usize,
    pub(super) max_tasks_in_memory: usize,
    pub(super) event_channel_capacity: usize,
    pub(super) task_ttl: Duration,
    pub(super) cleanup_interval: Duration,
    pub(super) lease_secs: u64,
    pub(super) heartbeat_interval: Duration,
    pub(super) claim_batch_size: usize,
}

impl PmResearchTaskConfig {
    pub(super) fn from_env() -> Self {
        fn read_usize(name: &str, default: usize) -> usize {
            env::var(name)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        }

        fn read_u64(name: &str, default: u64) -> u64 {
            env::var(name)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        }

        Self {
            max_concurrent_running: read_usize("PM_RESEARCH_TASK_MAX_CONCURRENT", 4),
            max_concurrent_per_tenant: read_usize("PM_RESEARCH_TASK_MAX_CONCURRENT_PER_TENANT", 2),
            max_tasks_in_memory: read_usize("PM_RESEARCH_TASK_MAX_IN_MEMORY", 1000),
            event_channel_capacity: read_usize("PM_RESEARCH_TASK_EVENT_CHANNEL_CAPACITY", 1024)
                .max(64),
            task_ttl: Duration::from_secs(read_u64("PM_RESEARCH_TASK_TTL_SECS", 3600)),
            cleanup_interval: Duration::from_secs(read_u64(
                "PM_RESEARCH_TASK_CLEANUP_INTERVAL_SECS",
                60,
            )),
            lease_secs: read_u64("PM_RESEARCH_TASK_LEASE_SECS", 180).max(30),
            heartbeat_interval: Duration::from_secs(
                read_u64("PM_RESEARCH_TASK_HEARTBEAT_SECS", 10).max(3),
            ),
            claim_batch_size: read_usize("PM_RESEARCH_TASK_CLAIM_BATCH_SIZE", 8),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PmResearchTaskManager {
    pub(super) inner: Arc<Mutex<HashMap<String, PmResearchTaskRecord>>>,
    pub(super) senders: Arc<Mutex<HashMap<String, broadcast::Sender<PmResearchTaskEvent>>>>,
    pub(super) stream_senders:
        Arc<Mutex<HashMap<String, broadcast::Sender<PmResearchTaskStreamEvent>>>>,
    pub(super) run_slots: Arc<Semaphore>,
    pub(super) tenant_run_slots: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    pub(super) config: PmResearchTaskConfig,
}

pub(super) struct PmResearchRunPermit {
    pub(super) _global: OwnedSemaphorePermit,
    pub(super) _tenant: OwnedSemaphorePermit,
}

pub async fn run_pm_background_runtime_cycle(state: &AppState) -> Result<(), String> {
    if !pm_background_runtime_claims_enabled_in_this_process() {
        tracing::debug!(
            "pm background runtime cycle skipped because PM_RESEARCH_TASK_WORKER_MODE is external"
        );
        return Ok(());
    }
    static PM_RUNTIME_CYCLE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = PM_RUNTIME_CYCLE_LOCK.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = lock.try_lock() else {
        tracing::debug!("pm background runtime cycle skipped because another cycle is active");
        return Ok(());
    };
    run_pm_background_runtime_cycle_impl(state).await
}
