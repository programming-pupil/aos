use super::agent_chat_adversarial_domain::{
    build_final_system_prompt, build_initial_system_prompt, build_judge_system_prompt,
    build_review_system_prompt, evaluate_participant_consensus, format_followup_context,
    format_own_previous_answer, format_peer_answers, format_peer_answers_for_reviewer,
    format_previous_judge_feedback, is_configured_winner, model_answer_to_json,
    parse_final_decision, parse_initial_answer, parse_judge_decision, parse_review_answer,
    participant_consensus_to_json, preferred_consensus_winner, truncate_chars,
    AdversarialDebateMemory, JudgeDecision, ModelAnswer, ParticipantConsensus,
    CHAT_ADV_CONSENSUS_VOTE_END, CHAT_ADV_CONSENSUS_VOTE_START, CHAT_ADV_EVIDENCE_REQUEST_END,
    CHAT_ADV_EVIDENCE_REQUEST_START,
};
use super::*;

const CHAT_ADV_MIN_MODELS: usize = 2;
const CHAT_ADV_MAX_MODELS: usize = 3;
pub(crate) const CHAT_ADVERSARIAL_NEEDS_MODELS_ERROR: &str =
    "super_adversarial_requires_distinct_models: super adversarial mode requires at least 2 distinct usable AI Chat models";
const CHAT_ADV_DEFAULT_MAX_ROUNDS: u32 = 3;
const CHAT_ADV_HARD_MAX_ROUNDS: u32 = 8;
const CHAT_ADV_MODEL_TIMEOUT_SECS: u64 = 180;
const CHAT_ADV_JUDGE_TIMEOUT_SECS: u64 = 240;
const CHAT_ADV_JOB_TIMEOUT_SECS: u64 = 12 * 60;
const CHAT_ADV_EVENT_REPLAY_LIMIT: usize = 512;
const CHAT_ADV_EVENT_CHANNEL_CAPACITY: usize = 1024;
const CHAT_ADV_EVIDENCE_MAX_RESULTS: usize = 5;
const CHAT_ADV_EVIDENCE_TOTAL_MAX_RESULTS: usize = 12;
const CHAT_ADV_MAX_EVIDENCE_SEARCHES_PER_MODEL: usize = 2;
const CHAT_ADV_CONTEXT_ARCHIVE_MAX_CHARS: usize = 1_000_000;

#[derive(Debug, Deserialize)]
pub(super) struct ChatAdversarialRunEventsQuery {
    after_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChatAdversarialStreamEvent {
    seq: u64,
    run_id: String,
    thread_id: Option<String>,
    round: Option<u32>,
    phase: String,
    model: Option<String>,
    message_id: String,
    event: String,
    delta: Option<String>,
    text: Option<String>,
    error: Option<String>,
    status: Option<String>,
    degraded: bool,
    usage: Option<serde_json::Value>,
    created_at_ms: u128,
}

#[derive(Debug, Clone)]
struct ChatAdversarialEventDraft {
    run_id: String,
    thread_id: Option<String>,
    round: Option<u32>,
    phase: String,
    model: Option<String>,
    message_id: String,
    event: String,
    delta: Option<String>,
    text: Option<String>,
    error: Option<String>,
    status: Option<String>,
    degraded: bool,
    usage: Option<serde_json::Value>,
}

#[derive(Default)]
struct ChatAdversarialEventManager {
    inner: Mutex<ChatAdversarialEventManagerInner>,
}

#[derive(Default)]
struct ChatAdversarialEventManagerInner {
    runs: HashMap<String, ChatAdversarialEventRun>,
}

struct ChatAdversarialEventRun {
    next_seq: u64,
    sender: broadcast::Sender<ChatAdversarialStreamEvent>,
    replay: VecDeque<ChatAdversarialStreamEvent>,
    terminal: bool,
}

#[derive(Debug)]
struct ChatAdversarialEventSubscription {
    replay: Vec<ChatAdversarialStreamEvent>,
    receiver: Option<broadcast::Receiver<ChatAdversarialStreamEvent>>,
    terminal: bool,
}

#[derive(Debug, Clone)]
struct ChatAdversarialCallContext {
    round: Option<u32>,
    phase: String,
    message_id: String,
    event_prefix: String,
}

#[derive(Default)]
struct AdversarialStreamUsage {
    input: u32,
    output: u32,
    cache_creation: u32,
    cache_read: u32,
}

#[derive(Debug, Clone)]
struct AdversarialMemoryContext {
    prompt: String,
    artifact: serde_json::Value,
}

static CHAT_ADVERSARIAL_EVENT_MANAGER: OnceLock<ChatAdversarialEventManager> = OnceLock::new();

fn chat_adversarial_event_manager() -> &'static ChatAdversarialEventManager {
    CHAT_ADVERSARIAL_EVENT_MANAGER.get_or_init(ChatAdversarialEventManager::default)
}

fn chat_adversarial_now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

impl ChatAdversarialEventManager {
    async fn ensure_run(&self, run_id: &str) {
        let mut guard = self.inner.lock().await;
        guard.ensure_run(run_id);
    }

    async fn emit(&self, draft: ChatAdversarialEventDraft) {
        let mut guard = self.inner.lock().await;
        guard.emit(draft);
    }

    async fn subscribe(
        &self,
        run_id: &str,
        after_seq: u64,
    ) -> Option<ChatAdversarialEventSubscription> {
        let guard = self.inner.lock().await;
        let run = guard.runs.get(run_id)?;
        let replay = run
            .replay
            .iter()
            .filter(|event| event.seq > after_seq)
            .cloned()
            .collect::<Vec<_>>();
        let receiver = if run.terminal {
            None
        } else {
            Some(run.sender.subscribe())
        };
        Some(ChatAdversarialEventSubscription {
            replay,
            receiver,
            terminal: run.terminal,
        })
    }

    async fn mark_terminal(&self, run_id: &str) {
        let mut guard = self.inner.lock().await;
        if let Some(run) = guard.runs.get_mut(run_id) {
            run.terminal = true;
        }
    }
}

impl ChatAdversarialEventManagerInner {
    fn ensure_run(&mut self, run_id: &str) -> &mut ChatAdversarialEventRun {
        self.runs.entry(run_id.to_string()).or_insert_with(|| {
            let (sender, _) = broadcast::channel(CHAT_ADV_EVENT_CHANNEL_CAPACITY);
            ChatAdversarialEventRun {
                next_seq: 1,
                sender,
                replay: VecDeque::with_capacity(CHAT_ADV_EVENT_REPLAY_LIMIT),
                terminal: false,
            }
        })
    }

    fn emit(&mut self, draft: ChatAdversarialEventDraft) {
        let run = self.ensure_run(&draft.run_id);
        let event = ChatAdversarialStreamEvent {
            seq: run.next_seq,
            run_id: draft.run_id,
            thread_id: draft.thread_id,
            round: draft.round,
            phase: draft.phase,
            model: draft.model,
            message_id: draft.message_id,
            event: draft.event,
            delta: draft.delta,
            text: draft.text,
            error: draft.error,
            status: draft.status,
            degraded: draft.degraded,
            usage: draft.usage,
            created_at_ms: chat_adversarial_now_ms(),
        };
        run.next_seq = run.next_seq.saturating_add(1);
        if run.replay.len() >= CHAT_ADV_EVENT_REPLAY_LIMIT {
            run.replay.pop_front();
        }
        run.replay.push_back(event.clone());
        if event.event == "run_completed"
            || event.event == "run_failed"
            || event.event == "run_cancelled"
        {
            run.terminal = true;
        }
        let _ = run.sender.send(event);
    }
}

fn chat_adversarial_message_id(
    run_id: &str,
    phase: &str,
    round: Option<u32>,
    model: Option<&str>,
) -> String {
    let model_part = model
        .unwrap_or("system")
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    format!(
        "{run_id}-{phase}-{}-{model_part}",
        round
            .map(|value| value.to_string())
            .unwrap_or_else(|| "final".to_string())
    )
}

fn chat_adversarial_usage_json(usage: &api::Usage) -> serde_json::Value {
    serde_json::json!({
        "inputTokens": usage.input_tokens,
        "outputTokens": usage.output_tokens,
        "cacheCreationInputTokens": usage.cache_creation_input_tokens,
        "cacheReadInputTokens": usage.cache_read_input_tokens,
        "totalTokens": usage.total_tokens(),
    })
}

fn chat_adversarial_snapshot_event(
    run: &ChatAdversarialRunDto,
    after_seq: u64,
) -> Option<ChatAdversarialStreamEvent> {
    if after_seq > 0 {
        return None;
    }
    Some(ChatAdversarialStreamEvent {
        seq: 0,
        run_id: run.id.clone(),
        thread_id: run.thread_id.clone(),
        round: u32::try_from(run.current_round)
            .ok()
            .filter(|round| *round > 0),
        phase: "system".to_string(),
        model: None,
        message_id: chat_adversarial_message_id(&run.id, "snapshot", None, None),
        event: "snapshot".to_string(),
        delta: None,
        text: None,
        error: run.error_message.clone(),
        status: Some(run.status.clone()),
        degraded: false,
        usage: Some(serde_json::json!({
            "currentRound": run.current_round,
            "maxRounds": run.max_rounds,
            "winnerModel": run.winner_model,
            "hasFinalAnswer": run.final_answer.as_deref().is_some_and(|value| !value.trim().is_empty()),
        })),
        created_at_ms: chat_adversarial_now_ms(),
    })
}

async fn emit_chat_adversarial_event(
    run_id: &str,
    thread_id: Option<&str>,
    context: &ChatAdversarialCallContext,
    event: &str,
    delta: Option<String>,
    text: Option<String>,
    error: Option<String>,
    status: Option<String>,
    model: Option<&str>,
    degraded: bool,
    usage: Option<serde_json::Value>,
) {
    chat_adversarial_event_manager()
        .emit(ChatAdversarialEventDraft {
            run_id: run_id.to_string(),
            thread_id: thread_id.map(ToString::to_string),
            round: context.round,
            phase: context.phase.clone(),
            model: model.map(ToString::to_string),
            message_id: context.message_id.clone(),
            event: event.to_string(),
            delta,
            text,
            error,
            status,
            degraded,
            usage,
        })
        .await;
}

async fn emit_chat_adversarial_system_event(
    run_id: &str,
    thread_id: Option<&str>,
    event: &str,
    status: Option<&str>,
    error: Option<String>,
) {
    let context = ChatAdversarialCallContext {
        round: None,
        phase: "system".to_string(),
        message_id: chat_adversarial_message_id(run_id, "system", None, None),
        event_prefix: "system".to_string(),
    };
    emit_chat_adversarial_event(
        run_id,
        thread_id,
        &context,
        event,
        None,
        None,
        error,
        status.map(ToString::to_string),
        None,
        false,
        None,
    )
    .await;
}

#[derive(Debug, Deserialize)]
pub(super) struct StartChatAdversarialRunRequest {
    question: String,
    models: Vec<String>,
    max_rounds: Option<u32>,
    parent_run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ChatAdversarialRunDto {
    id: String,
    agent_task_id: Option<String>,
    thread_id: Option<String>,
    thread_title: Option<String>,
    thread_pinned: bool,
    parent_run_id: Option<String>,
    iteration_no: i32,
    question: String,
    models: Vec<String>,
    judge_model: Option<String>,
    status: String,
    current_round: i32,
    max_rounds: i32,
    winner_model: Option<String>,
    winner_reason: Option<String>,
    final_answer: Option<String>,
    error_message: Option<String>,
    trace: Option<serde_json::Value>,
    session_id: Option<String>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ListChatAdversarialRunsQuery {
    limit: Option<u32>,
    page: Option<u32>,
    per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GetChatAdversarialThreadQuery {
    limit: Option<u32>,
    page: Option<u32>,
    per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateChatAdversarialThreadRequest {
    title: Option<String>,
    is_pinned: Option<bool>,
}

#[derive(Debug, Clone)]
struct ChatAdversarialThreadRef {
    thread_id: String,
    default_title: String,
}

#[derive(Debug, Clone)]
struct ParentRunContext {
    thread_id: String,
    iteration_no: i32,
    context_text: String,
    debate_state: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
struct AdversarialModelRuntime {
    model: String,
    entry: agent_gateway::ApiKeyEntry,
}

#[derive(Debug, Clone, Default)]
struct AdversarialEvidenceContext {
    attempted: bool,
    available: bool,
    degraded_reason: Option<String>,
    query: Option<String>,
    items: Vec<AdversarialEvidenceItem>,
    trace: serde_json::Value,
}

#[derive(Debug, Clone)]
struct AdversarialEvidenceItem {
    title: String,
    url: String,
    snippet: Option<String>,
    domain: Option<String>,
    source_type: String,
    source_name: String,
}

#[derive(Debug, Clone)]
struct AdversarialContextArchivePreview {
    window_id: String,
    ordinal: i64,
    role: String,
    content_kind: String,
    char_count: i64,
    preview: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ChatAdversarialBotRunInput {
    pub question: String,
    pub models: Vec<String>,
    pub max_rounds: Option<u32>,
    pub parent_run_id: Option<String>,
    pub session_id: Option<String>,
    pub evidence_search_required: bool,
    pub evidence_search_query: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChatAdversarialBotRunResult {
    pub id: String,
    pub thread_id: Option<String>,
    pub status: String,
    pub current_round: i32,
    pub max_rounds: i32,
}

pub(super) async fn start_chat_adversarial_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<StartChatAdversarialRunRequest>,
) -> Result<Json<ChatAdversarialRunDto>, AppError> {
    let dto = start_chat_adversarial_run_inner(
        state,
        claims,
        true,
        req.question,
        req.models,
        req.max_rounds,
        req.parent_run_id,
        None,
        false,
        None,
    )
    .await?;
    Ok(Json(dto))
}

pub(crate) async fn start_chat_adversarial_run_from_bot(
    state: AppState,
    claims: Claims,
    input: ChatAdversarialBotRunInput,
) -> Result<ChatAdversarialBotRunResult, AppError> {
    let dto = start_chat_adversarial_run_inner(
        state,
        claims,
        false,
        input.question,
        input.models,
        input.max_rounds,
        input.parent_run_id,
        input.session_id,
        input.evidence_search_required,
        input.evidence_search_query,
    )
    .await?;
    Ok(ChatAdversarialBotRunResult {
        id: dto.id,
        thread_id: dto.thread_id,
        status: dto.status,
        current_round: dto.current_round,
        max_rounds: dto.max_rounds,
    })
}

#[cfg(feature = "bot-agents")]
pub(crate) async fn default_chat_adversarial_models(
    state: &AppState,
    tenant_id: &str,
) -> Result<Vec<String>, AppError> {
    let entries = state
        .config_registry()
        .resolve_api_keys_by_model_type(tenant_id, Some("chat"), "chat")
        .await
        .map_err(|e| AppError::Internal(format!("failed to load chat API keys: {e}")))?;
    let mut out =
        distinct_chat_adversarial_models(entries.iter().map(|entry| entry.model.as_deref()));
    out.truncate(CHAT_ADV_MAX_MODELS);
    if out.len() < CHAT_ADV_MIN_MODELS {
        return Err(AppError::ValidationError(
            CHAT_ADVERSARIAL_NEEDS_MODELS_ERROR.to_string(),
        ));
    }
    Ok(out)
}

pub(crate) async fn request_chat_adversarial_cancel_from_agent_ops(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
) -> Result<(), AppError> {
    super::agent_chat_adversarial_support::request_chat_adversarial_cancel(
        state, tenant_id, None, run_id,
    )
    .await?;
    Ok(())
}

pub(crate) async fn mark_chat_adversarial_failed_from_parent(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
    error: &str,
) -> Result<(), AppError> {
    super::agent_chat_adversarial_support::mark_chat_adversarial_agent_failed(
        state, tenant_id, run_id, error,
    )
    .await
}

async fn start_chat_adversarial_run_inner(
    state: AppState,
    claims: Claims,
    create_agent_task: bool,
    question: String,
    models: Vec<String>,
    max_rounds: Option<u32>,
    parent_run_id: Option<String>,
    session_id: Option<String>,
    evidence_search_required: bool,
    evidence_search_query: Option<String>,
) -> Result<ChatAdversarialRunDto, AppError> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err(AppError::ValidationError(
            "question is required".to_string(),
        ));
    }

    let models = normalize_selected_models(models)?;
    let max_rounds = max_rounds
        .unwrap_or(CHAT_ADV_DEFAULT_MAX_ROUNDS)
        .clamp(1, CHAT_ADV_HARD_MAX_ROUNDS);
    let runtimes =
        resolve_chat_adversarial_model_runtimes(&state, &claims.tenant_id, &models).await?;
    let (judge_runtime, judge_is_independent) =
        resolve_chat_adversarial_judge_runtime(&state, &claims.tenant_id, &models, &runtimes)
            .await?;
    let parent_context = match parent_run_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(parent_run_id) => Some(
            load_parent_run_context(&state, &claims.tenant_id, &claims.sub, parent_run_id).await?,
        ),
        None => None,
    };
    let judge_model = Some(judge_runtime.model.clone());
    let run_id = format!("chat-adv-{}", uuid::Uuid::new_v4());
    let thread_id = parent_context
        .as_ref()
        .map_or_else(|| run_id.clone(), |parent| parent.thread_id.clone());
    let parent_run_id = parent_run_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let session_id = session_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let iteration_no = parent_context
        .as_ref()
        .map_or(1, |parent| parent.iteration_no.saturating_add(1));
    let models_json = serde_json::to_string(&models)
        .map_err(|e| AppError::Internal(format!("failed to encode models: {e}")))?;
    let trace = serde_json::json!({
        "models": models,
        "maxRounds": max_rounds,
        "judgeModel": judge_model,
        "evidencePolicy": {
            "mode": "adaptive_after_first_round",
            "routeRequiresSearch": evidence_search_required,
            "routeQuery": evidence_search_query.clone(),
            "maxSearchesPerModel": CHAT_ADV_MAX_EVIDENCE_SEARCHES_PER_MODEL,
        },
        "rounds": [],
    });
    let trace_json = serde_json::to_string(&trace)
        .map_err(|e| AppError::Internal(format!("failed to encode trace: {e}")))?;

    sqlx::query(
        r"
        INSERT INTO chat_adversarial_runs
            (id, tenant_id, user_id, session_id, thread_id, parent_run_id, iteration_no,
             question, models_json, judge_model, status,
             current_round, max_rounds, trace_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', 0, ?, ?)
        ",
    )
    .bind(&run_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&session_id)
    .bind(&thread_id)
    .bind(&parent_run_id)
    .bind(iteration_no)
    .bind(&question)
    .bind(&models_json)
    .bind(&judge_model)
    .bind(i32::try_from(max_rounds).unwrap_or(5))
    .bind(&trace_json)
    .execute(&state.db)
    .await?;

    chat_adversarial_event_manager().ensure_run(&run_id).await;
    emit_chat_adversarial_system_event(
        &run_id,
        Some(&thread_id),
        "run_queued",
        Some("queued"),
        None,
    )
    .await;

    upsert_chat_adversarial_thread_metadata(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &thread_id,
        &question,
    )
    .await?;

    if create_agent_task {
        super::agent_chat_adversarial_support::create_chat_adversarial_agent_task(
            &state, &claims, &run_id, &question, &models, max_rounds,
        )
        .await?;
    }

    let state_for_bg = state.clone();
    let tenant_id_for_bg = claims.tenant_id.clone();
    let user_id_for_bg = claims.sub.clone();
    let run_id_for_bg = run_id.clone();
    let thread_id_for_bg = thread_id.clone();
    let session_id_for_bg = session_id.clone();
    let parent_context_text = parent_context
        .as_ref()
        .map(|parent| parent.context_text.clone());
    let parent_debate_state = parent_context
        .as_ref()
        .and_then(|parent| parent.debate_state.clone());
    tokio::spawn(async move {
        let job_result = timeout(
            Duration::from_secs(CHAT_ADV_JOB_TIMEOUT_SECS),
            run_chat_adversarial_job(
                state_for_bg.clone(),
                tenant_id_for_bg.clone(),
                user_id_for_bg.clone(),
                run_id_for_bg.clone(),
                thread_id_for_bg.clone(),
                question,
                max_rounds,
                runtimes,
                judge_runtime,
                judge_is_independent,
                parent_context_text,
                parent_debate_state,
                session_id_for_bg.clone(),
                evidence_search_required,
                evidence_search_query,
            ),
        )
        .await;
        let result = match job_result {
            Ok(result) => result,
            Err(_) => match finalize_timed_out_adversarial_run(
                &state_for_bg,
                &tenant_id_for_bg,
                &user_id_for_bg,
                &run_id_for_bg,
                &thread_id_for_bg,
                session_id_for_bg.as_deref(),
            )
            .await
            {
                Ok(true) => Ok(()),
                Ok(false) => Err(AppError::Internal(format!(
                    "super adversarial run exceeded the {} minute total deadline without a complete round",
                    CHAT_ADV_JOB_TIMEOUT_SECS / 60
                ))),
                Err(error) => Err(error),
            },
        };
        if let Err(error) = result {
            if is_chat_adversarial_cancelled_error(&error) {
                let _ = super::agent_chat_adversarial_support::finish_chat_adversarial_cancelled(
                    &state_for_bg,
                    &tenant_id_for_bg,
                    &user_id_for_bg,
                    &run_id_for_bg,
                    None,
                )
                .await;
                emit_chat_adversarial_system_event(
                    &run_id_for_bg,
                    Some(&thread_id_for_bg),
                    "run_cancelled",
                    Some("cancelled"),
                    None,
                )
                .await;
                return;
            }
            tracing::error!(
                tenant_id = %tenant_id_for_bg,
                user_id = %user_id_for_bg,
                run_id = %run_id_for_bg,
                error = %error,
                "chat adversarial run failed"
            );
            let _ = sqlx::query(
                r"
                UPDATE chat_adversarial_runs
                SET status = 'failed', error_message = ?, updated_at = CURRENT_TIMESTAMP, completed_at = CURRENT_TIMESTAMP
                WHERE id = ? AND tenant_id = ? AND user_id = ?
                  AND status NOT IN ('completed','cancelled','cancelling')
                ",
            )
            .bind(error.to_string())
            .bind(&run_id_for_bg)
            .bind(&tenant_id_for_bg)
            .bind(&user_id_for_bg)
            .execute(&state_for_bg.db)
            .await;
            let _ = super::agent_chat_adversarial_support::mark_chat_adversarial_agent_failed(
                &state_for_bg,
                &tenant_id_for_bg,
                &run_id_for_bg,
                &error.to_string(),
            )
            .await;
            emit_chat_adversarial_system_event(
                &run_id_for_bg,
                Some(&thread_id_for_bg),
                "run_failed",
                Some("failed"),
                Some(error.to_string()),
            )
            .await;
        }
    });

    let Json(dto) =
        get_chat_adversarial_run_by_id(&state, &claims.tenant_id, &claims.sub, &run_id).await?;
    Ok(dto)
}

async fn finalize_timed_out_adversarial_run(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    thread_id: &str,
    session_id: Option<&str>,
) -> Result<bool, AppError> {
    let row = sqlx::query(
        "SELECT CAST(trace_json AS TEXT) AS trace_json
         FROM chat_adversarial_runs
         WHERE id = ? AND tenant_id = ? AND user_id = ?
           AND status IN ('queued','running') LIMIT 1",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let mut trace = row
        .get::<Option<String>, _>("trace_json")
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({"rounds": []}));
    let answers = trace
        .get("rounds")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .find_map(|round| {
            let usable = round
                .get("answers")
                .and_then(serde_json::Value::as_array)?
                .iter()
                .filter(|answer| {
                    answer.get("error").is_none_or(serde_json::Value::is_null)
                        && answer
                            .get("answer")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|text| !text.trim().is_empty())
                })
                .cloned()
                .collect::<Vec<_>>();
            (!usable.is_empty()).then_some(usable)
        });
    let Some(answers) = answers else {
        return Ok(false);
    };
    let winner = answers
        .iter()
        .max_by_key(|answer| {
            answer
                .get("answer")
                .and_then(serde_json::Value::as_str)
                .map(str::len)
                .unwrap_or(0)
        })
        .and_then(|answer| answer.get("model"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let sections = answers
        .iter()
        .filter_map(|answer| {
            let model = answer.get("model").and_then(serde_json::Value::as_str)?;
            let text = answer.get("answer").and_then(serde_json::Value::as_str)?;
            Some(format!("### {model}\n\n{}", truncate_chars(text, 8000)))
        })
        .collect::<Vec<_>>();
    let final_answer = format!(
        "## 超级对抗部分结果\n\n整体执行达到 {} 分钟上限。以下是最近一个完整轮次中各健康模型的可用答案，尚未完成独立裁判终局；请将分歧视为待验证项。\n\n{}",
        CHAT_ADV_JOB_TIMEOUT_SECS / 60,
        sections.join("\n\n---\n\n")
    );
    let winner_reason =
        format!("整体超时后交付最近完整轮次；{winner} 的可用答案最完整，但未完成独立终局裁判。");
    trace["termination"] = serde_json::json!({
        "reason": "total_deadline_partial_delivery",
        "partial": true,
        "winnerModel": winner,
    });
    trace["final"] = serde_json::json!({
        "winnerModel": winner,
        "winnerReason": winner_reason,
        "finalAnswer": final_answer,
        "partial": true,
    });
    let updated = sqlx::query(
        "UPDATE chat_adversarial_runs
         SET status = 'completed', winner_model = ?, winner_reason = ?, final_answer = ?,
             trace_json = ?, error_message = NULL, updated_at = CURRENT_TIMESTAMP,
             completed_at = CURRENT_TIMESTAMP
         WHERE id = ? AND tenant_id = ? AND user_id = ?
           AND status IN ('queued','running')",
    )
    .bind(&winner)
    .bind(&winner_reason)
    .bind(&final_answer)
    .bind(trace.to_string())
    .bind(run_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    persist_adversarial_final_answer_to_super_assistant_session(
        state,
        tenant_id,
        user_id,
        session_id,
        &final_answer,
    )
    .await;
    super::agent_chat_adversarial_support::mark_chat_adversarial_agent_completed(
        state,
        tenant_id,
        run_id,
        serde_json::json!({
            "runId": run_id,
            "winnerModel": winner,
            "winnerReason": winner_reason,
            "finalAnswer": final_answer,
            "partial": true,
        }),
    )
    .await?;
    emit_chat_adversarial_system_event(
        run_id,
        Some(thread_id),
        "run_completed",
        Some("completed"),
        None,
    )
    .await;
    chat_adversarial_event_manager().mark_terminal(run_id).await;
    Ok(true)
}

fn is_chat_adversarial_cancelled_error(error: &AppError) -> bool {
    matches!(error, AppError::ValidationError(message) if message.contains("chat adversarial run was cancelled"))
}

fn chat_adversarial_should_stop_after_round(
    round: u32,
    max_rounds: u32,
    participant_consensus: &ParticipantConsensus,
    judge: &JudgeDecision,
    configured_models: &[String],
) -> bool {
    round >= max_rounds
        || ((round == 1 || participant_consensus.reached)
            && judge.resolved
            && judge.claim_audit_complete
            && judge.critical_conflicts.is_empty()
            && is_configured_winner(judge.winner_model.as_deref(), configured_models))
}

pub(super) async fn cancel_chat_adversarial_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let status = super::agent_chat_adversarial_support::request_chat_adversarial_cancel(
        &state,
        &claims.tenant_id,
        Some(&claims.sub),
        &run_id,
    )
    .await?;
    emit_chat_adversarial_system_event(&run_id, None, "cancel_requested", Some(&status), None)
        .await;
    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": run_id,
        "status": status,
    })))
}

pub(super) async fn stream_chat_adversarial_run_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
    Query(query): Query<ChatAdversarialRunEventsQuery>,
) -> impl IntoResponse {
    let run = match get_chat_adversarial_run_by_id(&state, &claims.tenant_id, &claims.sub, &run_id)
        .await
    {
        Ok(Json(run)) => run,
        Err(error) => return error.into_response(),
    };
    let after_seq = query.after_seq.unwrap_or(0);
    let manager = chat_adversarial_event_manager();
    let subscription = manager.subscribe(&run_id, after_seq).await;
    let stream = async_stream::stream! {
        let snapshot = chat_adversarial_snapshot_event(&run, after_seq);
        if let Some(snapshot) = snapshot {
            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                axum::response::sse::Event::default()
                    .event("adversarial_event")
                    .data(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string())),
            );
        }

        if let Some(mut subscription) = subscription {
            for event in subscription.replay {
                yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                    axum::response::sse::Event::default()
                        .event("adversarial_event")
                        .data(serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string())),
                );
            }
            if subscription.terminal {
                return;
            }
            if let Some(mut rx) = subscription.receiver.take() {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            let is_terminal = matches!(
                                event.event.as_str(),
                                "run_completed" | "run_failed" | "run_cancelled"
                            );
                            yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                                axum::response::sse::Event::default()
                                    .event("adversarial_event")
                                    .data(serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string())),
                            );
                            if is_terminal {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                run_id = %run_id,
                                skipped,
                                "chat adversarial event stream lagged; continuing"
                            );
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
            return;
        }

        if matches!(
            run.status.as_str(),
            "completed" | "failed" | "cancelled" | "cancelling"
        ) {
            return;
        }

        let db = state.db.clone();
        let tenant_id = claims.tenant_id.clone();
        let user_id = claims.sub.clone();
        let mut last_status = run.status.clone();
        loop {
            tokio::time::sleep(Duration::from_millis(900)).await;
            let row = sqlx::query(
                r"
                SELECT r.id, r.thread_id, m.title AS thread_title,
                       COALESCE(m.is_pinned, 0) AS thread_pinned,
                       (
                         SELECT at.id
                         FROM agent_tasks at
                         WHERE at.tenant_id = r.tenant_id
                           AND at.linked_resource_type = 'chat_adversarial_run'
                           AND at.linked_resource_id = r.id
                         ORDER BY at.updated_at DESC, at.created_at DESC
                         LIMIT 1
                       ) AS agent_task_id,
                       r.parent_run_id, r.iteration_no,
                       r.question, CAST(r.models_json AS TEXT) AS models_json, r.judge_model, r.status,
                       r.current_round, r.max_rounds, r.winner_model, r.winner_reason, r.final_answer,
                       r.error_message, CAST(r.trace_json AS TEXT) AS trace_json,
                       CAST(r.created_at AS TEXT) AS created_at,
                       CAST(r.updated_at AS TEXT) AS updated_at,
                       CAST(r.completed_at AS TEXT) AS completed_at
                FROM chat_adversarial_runs r
                LEFT JOIN chat_adversarial_threads m
                  ON m.tenant_id = r.tenant_id
                 AND m.user_id = r.user_id
                 AND m.thread_id = COALESCE(r.thread_id, r.id)
                WHERE r.id = ? AND r.tenant_id = ? AND r.user_id = ? AND m.deleted_at IS NULL
                LIMIT 1
                ",
            )
            .bind(&run_id)
            .bind(&tenant_id)
            .bind(&user_id)
            .fetch_optional(&db)
            .await;
            let Ok(Some(row)) = row else {
                break;
            };
            let latest = chat_adversarial_run_from_row(row);
            if latest.status != last_status
                || matches!(latest.status.as_str(), "completed" | "failed" | "cancelled")
            {
                last_status = latest.status.clone();
                if let Some(event) = chat_adversarial_snapshot_event(&latest, 0) {
                    yield Ok::<axum::response::sse::Event, std::convert::Infallible>(
                        axum::response::sse::Event::default()
                            .event("adversarial_event")
                            .data(serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string())),
                    );
                }
            }
            if matches!(latest.status.as_str(), "completed" | "failed" | "cancelled") {
                break;
            }
        }
    };

    axum::response::Sse::new(stream).into_response()
}

pub(super) async fn list_chat_adversarial_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListChatAdversarialRunsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.or(query.limit).unwrap_or(20).clamp(1, 50);
    let offset = i64::from(page.saturating_sub(1)) * i64::from(per_page);
    let total: i64 = sqlx::query_scalar(
        r"
        SELECT COUNT(*)
        FROM (
            SELECT COALESCE(r.thread_id, r.id) AS thread_key
            FROM chat_adversarial_runs r
            LEFT JOIN chat_adversarial_threads m
              ON m.tenant_id = r.tenant_id
             AND m.user_id = r.user_id
             AND m.thread_id = COALESCE(r.thread_id, r.id)
            WHERE r.tenant_id = ? AND r.user_id = ? AND m.deleted_at IS NULL
            GROUP BY COALESCE(r.thread_id, r.id)
        ) grouped_threads
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;
    let rows = sqlx::query(
        r"
        WITH ranked_runs AS (
            SELECT r.*,
                   COALESCE(r.thread_id, r.id) AS thread_key,
                   ROW_NUMBER() OVER (
                       PARTITION BY COALESCE(r.thread_id, r.id)
                       ORDER BY r.iteration_no DESC, r.created_at DESC, r.id DESC
                   ) AS rn
            FROM chat_adversarial_runs r
            LEFT JOIN chat_adversarial_threads m
              ON m.tenant_id = r.tenant_id
             AND m.user_id = r.user_id
             AND m.thread_id = COALESCE(r.thread_id, r.id)
            WHERE r.tenant_id = ? AND r.user_id = ? AND m.deleted_at IS NULL
        )
        SELECT r.id, r.thread_id, m.title AS thread_title,
               COALESCE(m.is_pinned, 0) AS thread_pinned,
               (
                 SELECT at.id
                 FROM agent_tasks at
                 WHERE at.tenant_id = r.tenant_id
                   AND at.linked_resource_type = 'chat_adversarial_run'
                   AND at.linked_resource_id = r.id
                 ORDER BY at.updated_at DESC, at.created_at DESC
                 LIMIT 1
               ) AS agent_task_id,
               r.parent_run_id, r.iteration_no,
               r.question, CAST(r.models_json AS TEXT) AS models_json, r.judge_model, r.status,
               r.current_round, r.max_rounds, r.winner_model, r.winner_reason, r.final_answer,
               r.error_message, CAST(r.trace_json AS TEXT) AS trace_json,
               CAST(r.created_at AS TEXT) AS created_at,
               CAST(r.updated_at AS TEXT) AS updated_at,
               CAST(r.completed_at AS TEXT) AS completed_at
        FROM ranked_runs r
        LEFT JOIN chat_adversarial_threads m
          ON m.tenant_id = r.tenant_id
         AND m.user_id = r.user_id
         AND m.thread_id = r.thread_key
        WHERE r.rn = 1 AND m.deleted_at IS NULL
        ORDER BY COALESCE(m.is_pinned, 0) DESC,
                 COALESCE(m.updated_at, r.updated_at) DESC,
                 r.created_at DESC,
                 r.id DESC
        LIMIT ? OFFSET ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(i64::from(per_page))
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .into_iter()
        .map(chat_adversarial_run_from_row)
        .collect::<Vec<_>>();
    let loaded = offset.saturating_add(i64::try_from(items.len()).unwrap_or(0));
    Ok(Json(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "per_page": per_page,
        "has_more": loaded < total,
    })))
}

pub(super) async fn get_chat_adversarial_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> Result<Json<ChatAdversarialRunDto>, AppError> {
    get_chat_adversarial_run_by_id(&state, &claims.tenant_id, &claims.sub, &run_id).await
}

pub(super) async fn get_chat_adversarial_run_thread(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
    Query(query): Query<GetChatAdversarialThreadQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query(
        r"
        SELECT r.id, COALESCE(r.thread_id, r.id) AS thread_key
        FROM chat_adversarial_runs r
        LEFT JOIN chat_adversarial_threads m
          ON m.tenant_id = r.tenant_id
         AND m.user_id = r.user_id
         AND m.thread_id = COALESCE(r.thread_id, r.id)
        WHERE r.id = ? AND r.tenant_id = ? AND r.user_id = ? AND m.deleted_at IS NULL
        LIMIT 1
        ",
    )
    .bind(&run_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound(
            "chat adversarial run not found".to_string(),
        ));
    };
    let thread_id: String = row.get("thread_key");
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.or(query.limit).unwrap_or(3).clamp(1, 20);
    let offset = i64::from(page.saturating_sub(1)) * i64::from(per_page);

    let total: i64 = sqlx::query_scalar(
        r"
        SELECT COUNT(*)
        FROM chat_adversarial_runs r
        LEFT JOIN chat_adversarial_threads m
          ON m.tenant_id = r.tenant_id
         AND m.user_id = r.user_id
         AND m.thread_id = COALESCE(r.thread_id, r.id)
        WHERE r.tenant_id = ? AND r.user_id = ?
          AND (r.thread_id = ? OR (r.thread_id IS NULL AND r.id = ?))
          AND m.deleted_at IS NULL
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&thread_id)
    .bind(&thread_id)
    .fetch_one(&state.db)
    .await?;

    let rows = sqlx::query(
        r"
        SELECT r.id, r.thread_id, m.title AS thread_title,
               COALESCE(m.is_pinned, 0) AS thread_pinned,
               (
                 SELECT at.id
                 FROM agent_tasks at
                 WHERE at.tenant_id = r.tenant_id
                   AND at.linked_resource_type = 'chat_adversarial_run'
                   AND at.linked_resource_id = r.id
                 ORDER BY at.updated_at DESC, at.created_at DESC
                 LIMIT 1
               ) AS agent_task_id,
               r.parent_run_id, r.iteration_no,
               r.question, CAST(r.models_json AS TEXT) AS models_json, r.judge_model, r.status,
               r.current_round, r.max_rounds, r.winner_model, r.winner_reason, r.final_answer,
               r.error_message, CAST(r.trace_json AS TEXT) AS trace_json,
               CAST(r.created_at AS TEXT) AS created_at,
               CAST(r.updated_at AS TEXT) AS updated_at,
               CAST(r.completed_at AS TEXT) AS completed_at
        FROM chat_adversarial_runs r
        LEFT JOIN chat_adversarial_threads m
          ON m.tenant_id = r.tenant_id
         AND m.user_id = r.user_id
         AND m.thread_id = COALESCE(r.thread_id, r.id)
        WHERE r.tenant_id = ? AND r.user_id = ?
          AND (r.thread_id = ? OR (r.thread_id IS NULL AND r.id = ?))
          AND m.deleted_at IS NULL
        ORDER BY r.iteration_no DESC, r.created_at DESC, r.id DESC
        LIMIT ? OFFSET ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&thread_id)
    .bind(&thread_id)
    .bind(i64::from(per_page))
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let mut items = rows
        .into_iter()
        .map(chat_adversarial_run_from_row)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        a.iteration_no
            .cmp(&b.iteration_no)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let loaded = offset.saturating_add(i64::try_from(items.len()).unwrap_or(0));
    Ok(Json(serde_json::json!({
        "thread_id": thread_id,
        "total": total,
        "page": page,
        "per_page": per_page,
        "has_more": loaded < total,
        "items": items,
    })))
}

pub(super) async fn update_chat_adversarial_run_thread(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
    Json(req): Json<UpdateChatAdversarialThreadRequest>,
) -> Result<Json<ChatAdversarialRunDto>, AppError> {
    let thread_ref =
        resolve_chat_adversarial_thread_ref(&state, &claims.tenant_id, &claims.sub, &run_id)
            .await?;
    ensure_chat_adversarial_thread_metadata(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &thread_ref.thread_id,
        &thread_ref.default_title,
    )
    .await?;

    let mut changed = false;
    if let Some(title) = req.title {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::ValidationError(
                "thread title cannot be empty".to_string(),
            ));
        }
        if title.chars().count() > 191 {
            return Err(AppError::ValidationError(
                "thread title is too long".to_string(),
            ));
        }
        sqlx::query(
            r"
            UPDATE chat_adversarial_threads
            SET title = ?, updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND user_id = ? AND thread_id = ? AND deleted_at IS NULL
            ",
        )
        .bind(title)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&thread_ref.thread_id)
        .execute(&state.db)
        .await?;
        changed = true;
    }

    if let Some(is_pinned) = req.is_pinned {
        sqlx::query(
            r"
            UPDATE chat_adversarial_threads
            SET is_pinned = ?, updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND user_id = ? AND thread_id = ? AND deleted_at IS NULL
            ",
        )
        .bind(is_pinned)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&thread_ref.thread_id)
        .execute(&state.db)
        .await?;
        changed = true;
    }

    if !changed {
        return Err(AppError::ValidationError(
            "no thread fields to update".to_string(),
        ));
    }

    get_chat_adversarial_run_by_id(&state, &claims.tenant_id, &claims.sub, &run_id).await
}

pub(super) async fn delete_chat_adversarial_run_thread(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let thread_ref =
        resolve_chat_adversarial_thread_ref(&state, &claims.tenant_id, &claims.sub, &run_id)
            .await?;
    sqlx::query(
        r"
        INSERT INTO chat_adversarial_threads
            (tenant_id, user_id, thread_id, title, is_pinned, deleted_at)
        VALUES (?, ?, ?, ?, 0, CURRENT_TIMESTAMP)
        ON CONFLICT DO UPDATE SET
            deleted_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&thread_ref.thread_id)
    .bind(&thread_ref.default_title)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({
        "deleted": true,
        "thread_id": thread_ref.thread_id,
    })))
}

async fn get_chat_adversarial_run_by_id(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
) -> Result<Json<ChatAdversarialRunDto>, AppError> {
    let row = sqlx::query(
        r"
        SELECT r.id, r.thread_id, m.title AS thread_title,
               COALESCE(m.is_pinned, 0) AS thread_pinned,
               (
                 SELECT at.id
                 FROM agent_tasks at
                 WHERE at.tenant_id = r.tenant_id
                   AND at.linked_resource_type = 'chat_adversarial_run'
                   AND at.linked_resource_id = r.id
                 ORDER BY at.updated_at DESC, at.created_at DESC
                 LIMIT 1
               ) AS agent_task_id,
               r.parent_run_id, r.iteration_no,
               r.question, CAST(r.models_json AS TEXT) AS models_json, r.judge_model, r.status,
               r.current_round, r.max_rounds, r.winner_model, r.winner_reason, r.final_answer,
               r.error_message, CAST(r.trace_json AS TEXT) AS trace_json,
               CAST(r.created_at AS TEXT) AS created_at,
               CAST(r.updated_at AS TEXT) AS updated_at,
               CAST(r.completed_at AS TEXT) AS completed_at
        FROM chat_adversarial_runs r
        LEFT JOIN chat_adversarial_threads m
          ON m.tenant_id = r.tenant_id
         AND m.user_id = r.user_id
         AND m.thread_id = COALESCE(r.thread_id, r.id)
        WHERE r.id = ? AND r.tenant_id = ? AND r.user_id = ? AND m.deleted_at IS NULL
        LIMIT 1
        ",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    match row {
        Some(row) => Ok(Json(chat_adversarial_run_from_row(row))),
        None => Err(AppError::NotFound(
            "chat adversarial run not found".to_string(),
        )),
    }
}

async fn resolve_chat_adversarial_thread_ref(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
) -> Result<ChatAdversarialThreadRef, AppError> {
    let row = sqlx::query(
        r"
        SELECT COALESCE(r.thread_id, r.id) AS thread_key, r.question
        FROM chat_adversarial_runs r
        LEFT JOIN chat_adversarial_threads m
          ON m.tenant_id = r.tenant_id
         AND m.user_id = r.user_id
         AND m.thread_id = COALESCE(r.thread_id, r.id)
        WHERE r.id = ? AND r.tenant_id = ? AND r.user_id = ? AND m.deleted_at IS NULL
        LIMIT 1
        ",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;

    let Some(row) = row else {
        return Err(AppError::NotFound(
            "chat adversarial thread not found".to_string(),
        ));
    };
    let question: String = row.get("question");
    Ok(ChatAdversarialThreadRef {
        thread_id: row.get("thread_key"),
        default_title: truncate_chars(&question, 80),
    })
}

async fn upsert_chat_adversarial_thread_metadata(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
    question: &str,
) -> Result<(), AppError> {
    ensure_chat_adversarial_thread_metadata(
        state,
        tenant_id,
        user_id,
        thread_id,
        &truncate_chars(question, 80),
    )
    .await
}

async fn ensure_chat_adversarial_thread_metadata(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
    default_title: &str,
) -> Result<(), AppError> {
    sqlx::query(
        r"
        INSERT INTO chat_adversarial_threads
            (tenant_id, user_id, thread_id, title, is_pinned, deleted_at)
        VALUES (?, ?, ?, ?, 0, NULL)
        ON CONFLICT DO UPDATE SET
            title = COALESCE(title, excluded.title),
            updated_at = CURRENT_TIMESTAMP
        ",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .bind(default_title)
    .execute(&state.db)
    .await?;
    Ok(())
}

fn chat_adversarial_run_from_row(row: sqlx::sqlite::SqliteRow) -> ChatAdversarialRunDto {
    let models_json: Option<String> = row.get("models_json");
    let trace_json: Option<String> = row.get("trace_json");
    let thread_pinned = row
        .try_get::<i8, _>("thread_pinned")
        .map(|value| value != 0)
        .or_else(|_| {
            row.try_get::<i64, _>("thread_pinned")
                .map(|value| value != 0)
        })
        .unwrap_or(false);
    ChatAdversarialRunDto {
        id: row.get("id"),
        agent_task_id: row.try_get("agent_task_id").ok().flatten(),
        thread_id: row.get("thread_id"),
        thread_title: row.get("thread_title"),
        thread_pinned,
        parent_run_id: row.get("parent_run_id"),
        iteration_no: row.get("iteration_no"),
        question: row.get("question"),
        models: models_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default(),
        judge_model: row.get("judge_model"),
        status: row.get("status"),
        current_round: row.get("current_round"),
        max_rounds: row.get("max_rounds"),
        winner_model: row.get("winner_model"),
        winner_reason: row.get("winner_reason"),
        final_answer: row.get("final_answer"),
        error_message: row.get("error_message"),
        trace: trace_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        session_id: row.try_get("session_id").ok().flatten(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        completed_at: row.get("completed_at"),
    }
}

async fn load_parent_run_context(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    parent_run_id: &str,
) -> Result<ParentRunContext, AppError> {
    let parent = sqlx::query(
        r"
        SELECT r.id, r.thread_id, r.iteration_no, r.status
        FROM chat_adversarial_runs r
        LEFT JOIN chat_adversarial_threads m
          ON m.tenant_id = r.tenant_id
         AND m.user_id = r.user_id
         AND m.thread_id = COALESCE(r.thread_id, r.id)
        WHERE r.id = ? AND r.tenant_id = ? AND r.user_id = ? AND m.deleted_at IS NULL
        LIMIT 1
        ",
    )
    .bind(parent_run_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;

    let Some(parent) = parent else {
        return Err(AppError::NotFound(
            "parent adversarial run not found".to_string(),
        ));
    };
    let parent_id: String = parent.get("id");
    let parent_status: String = parent.get("status");
    if parent_status != "completed" {
        return Err(AppError::ValidationError(
            "parent adversarial run must be completed before follow-up".to_string(),
        ));
    }

    let thread_id = parent
        .get::<Option<String>, _>("thread_id")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| parent_id.clone());
    let parent_iteration = parent.get::<i32, _>("iteration_no");
    let summary = load_adversarial_thread_summary(state, tenant_id, user_id, &thread_id).await;
    let archive_previews =
        load_adversarial_context_archive_previews(state, tenant_id, user_id, &thread_id, 12).await;
    let archive_context = format_adversarial_context_archive_previews(&archive_previews);
    let rows = sqlx::query(
        r"
        SELECT id, iteration_no, question, winner_model, winner_reason, final_answer,
               CAST(trace_json AS TEXT) AS trace_json,
               CAST(created_at AS TEXT) AS created_at
        FROM chat_adversarial_runs
        WHERE tenant_id = ? AND user_id = ? AND thread_id = ? AND iteration_no <= ?
        ORDER BY iteration_no ASC, created_at ASC, id ASC
        LIMIT 12
        ",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&thread_id)
    .bind(parent_iteration)
    .fetch_all(&state.db)
    .await?;

    let mut sections = Vec::new();
    let mut latest_debate_state = None;
    for row in rows {
        let iteration = row.get::<i32, _>("iteration_no");
        let question = row.get::<String, _>("question");
        let final_answer = row.get::<Option<String>, _>("final_answer");
        let trace_json = row.get::<Option<String>, _>("trace_json");
        if let Some(debate_state) = trace_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|trace| trace.get("debateState").cloned())
            .filter(|value| !value.is_null())
        {
            latest_debate_state = Some(debate_state);
        }
        let created_at = row.get::<String, _>("created_at");
        sections.push(format!(
            "## 第 {iteration} 次对抗（{created_at}）\n用户问题：{}\n最终答案：{}",
            truncate_chars(&question, 1200),
            final_answer
                .as_deref()
                .map(|text| truncate_chars(text, 4000))
                .unwrap_or_else(|| "未记录".to_string()),
        ));
    }

    if sections.is_empty() {
        return Err(AppError::ValidationError(
            "parent adversarial run has no usable thread context".to_string(),
        ));
    }
    let raw_context = sections.join("\n\n---\n\n");
    let context_text = match summary {
        Some(summary) if archive_context.trim().is_empty() => {
            format!(
                "## 历史压缩摘要（用于定向理解）\n{}\n\n---\n\n## 最近对抗明细（用于核对）\n{}",
                truncate_chars(&summary, 6000),
                truncate_chars(&raw_context, 12_000)
            )
        }
        Some(summary) => {
            format!(
                "## 历史压缩摘要（用于定向理解）\n{}\n\n---\n\n{}\n\n---\n\n## 最近对抗明细（用于核对）\n{}",
                truncate_chars(&summary, 6000),
                archive_context,
                truncate_chars(&raw_context, 12_000)
            )
        }
        None if archive_context.trim().is_empty() => raw_context,
        None => format!("{archive_context}\n\n---\n\n## 最近对抗明细（用于核对）\n{raw_context}"),
    };

    Ok(ParentRunContext {
        thread_id,
        iteration_no: parent_iteration,
        context_text,
        debate_state: latest_debate_state,
    })
}

async fn load_adversarial_thread_summary(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
) -> Option<String> {
    let row = sqlx::query(
        r#"
        SELECT summary
        FROM agent_memory_summaries
        WHERE tenant_id = ? AND user_id = ?
          AND scope = 'session'
          AND app = 'shared'
          AND session_key = ?
        ORDER BY updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .fetch_optional(&state.db)
    .await
    .ok()??;
    row.try_get::<String, _>("summary").ok()
}

async fn load_adversarial_memory_context(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
    question: &str,
) -> Option<AdversarialMemoryContext> {
    crate::routes::memory_continuity::build_unified_memory_prompt(
        &state.db,
        tenant_id,
        user_id,
        Some(thread_id),
        "shared",
        question,
        "auto",
    )
    .await
    .map(|(prompt, artifact)| AdversarialMemoryContext { prompt, artifact })
}

fn format_adversarial_memory_prompt(memory: Option<&AdversarialMemoryContext>) -> String {
    memory
        .map(|memory| format!("{}\n\n---\n\n", memory.prompt))
        .unwrap_or_default()
}

fn format_adversarial_debate_state_for_model(
    debate_state: Option<&serde_json::Value>,
    model: &str,
) -> String {
    let Some(state) = debate_state else {
        return String::new();
    };
    let mut sections = Vec::new();
    sections.push("结构化对抗记忆（同一线程追问时使用）：以下内容是上一轮裁判压缩后的辩论状态，不是不可推翻的事实；如果新问题要求修正或推翻旧结论，以新问题和证据为准。".to_string());
    if let Some(question) = json_string_at(state, &["question"]) {
        sections.push(format!("上一轮问题：{}", truncate_chars(question, 800)));
    }
    if let Some(final_answer) = json_string_at(state, &["finalAnswer"]) {
        sections.push(format!(
            "上一轮最终答案摘要：{}",
            summarize_adversarial_text(final_answer, 1800)
        ));
    }
    if let Some(judge) = state.get("judgeVerdict") {
        let winner = json_string_at(judge, &["winnerModel"]).unwrap_or("未指定");
        let reason = json_string_at(judge, &["winnerReason"]).unwrap_or("未记录");
        sections.push(format!(
            "裁判结论：winner_model={}；winner_reason={}",
            winner,
            truncate_chars(reason, 1000)
        ));
    }
    if let Some(items) = json_array_at(state, &["acceptedConclusions"]) {
        let lines = json_string_list(items, 5, 900);
        if !lines.is_empty() {
            sections.push(format!("已采纳强结论：\n{}", lines.join("\n")));
        }
    }
    if let Some(items) = json_array_at(state, &["unresolvedDisputes"]) {
        let lines = json_string_list(items, 5, 900);
        if !lines.is_empty() {
            sections.push(format!("未解决分歧/低置信点：\n{}", lines.join("\n")));
        }
    }
    if let Some(items) = json_array_at(state, &["rejectedClaims"]) {
        let lines = json_string_list(items, 4, 700);
        if !lines.is_empty() {
            sections.push(format!("已被裁判弱化或驳回的观点：\n{}", lines.join("\n")));
        }
    }
    if let Some(model_stances) = state
        .get("modelStances")
        .and_then(serde_json::Value::as_object)
    {
        if let Some((_, stance)) = model_stances
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(model))
        {
            sections.push(format!(
                "你在上一轮的历史立场：\n{}",
                format_model_stance_for_prompt(stance, 1200)
            ));
        }
        let peer_lines = model_stances
            .iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case(model))
            .take(4)
            .map(|(name, stance)| {
                format!(
                    "- {}: {}",
                    name,
                    truncate_chars(&format_model_stance_for_prompt(stance, 900), 900)
                )
            })
            .collect::<Vec<_>>();
        if !peer_lines.is_empty() {
            sections.push(format!(
                "其他模型上一轮关键立场：\n{}",
                peer_lines.join("\n")
            ));
        }
    }
    if let Some(items) = json_array_at(state, &["evidenceLedger"]) {
        let lines = items
            .iter()
            .take(5)
            .filter_map(|item| {
                let title = json_string_at(item, &["title"]).unwrap_or("未命名证据");
                let source_type = json_string_at(item, &["sourceType"]).unwrap_or("unknown");
                let domain = json_string_at(item, &["domain"]).unwrap_or("");
                let snippet = json_string_at(item, &["snippet"]).unwrap_or("");
                if title.trim().is_empty() && snippet.trim().is_empty() {
                    None
                } else {
                    let source_label = if domain.is_empty() {
                        source_type.to_string()
                    } else {
                        format!("{source_type} / {domain}")
                    };
                    Some(format!(
                        "- [{}] {}：{}",
                        source_label,
                        truncate_chars(title, 180),
                        truncate_chars(snippet, 420)
                    ))
                }
            })
            .collect::<Vec<_>>();
        if !lines.is_empty() {
            sections.push(format!("上一轮信息证据账本：\n{}", lines.join("\n")));
        }
    }
    if sections.len() <= 1 {
        return String::new();
    }
    format!("{}\n\n---\n\n", sections.join("\n\n"))
}

fn format_model_stance_for_prompt(stance: &serde_json::Value, max_chars: usize) -> String {
    let mut lines = Vec::new();
    if let Some(value) = json_string_at(stance, &["corePosition"]) {
        lines.push(format!("核心立场：{}", truncate_chars(value, 700)));
    }
    for (label, key) in [
        ("强观点", "strongClaims"),
        ("弱观点/风险", "weakClaims"),
        ("让步/修正", "concessions"),
    ] {
        if let Some(items) = json_array_at(stance, &[key]) {
            let item_lines = json_string_list(items, 4, 500);
            if !item_lines.is_empty() {
                lines.push(format!("{label}：{}", item_lines.join("；")));
            }
        }
    }
    if lines.is_empty() {
        if let Some(raw) = json_string_at(stance, &["raw"]) {
            lines.push(truncate_chars(raw, max_chars));
        }
    }
    truncate_chars(&lines.join("\n"), max_chars)
}

async fn persist_adversarial_thread_summary_after_run(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
    question: &str,
    final_answer: &str,
    debate_state: &serde_json::Value,
) {
    let existing = load_adversarial_thread_summary(state, tenant_id, user_id, thread_id)
        .await
        .unwrap_or_default();
    let summary = build_adversarial_thread_summary(&existing, question, final_answer);
    let turn_count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM chat_adversarial_runs
        WHERE tenant_id = ? AND user_id = ? AND thread_id = ?
          AND status = 'completed'
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let metadata = serde_json::json!({
        "source": "chat_adversarial_thread",
        "lastQuestionPreview": truncate_chars(question, 240),
        "summaryStrategy": "deterministic_rolling",
        "debateStateVersion": 1,
        "debateState": debate_state,
    });
    let _ = sqlx::query(
        r#"
        INSERT INTO agent_memory_summaries
          (id, tenant_id, user_id, scope, app, session_id, session_key, summary,
           source_type, turn_count, metadata_json)
        VALUES (?, ?, ?, 'session', 'shared', ?, ?, ?, 'compaction', ?, json(?))
        ON CONFLICT DO UPDATE SET
          summary = excluded.summary,
          source_type = excluded.source_type,
          turn_count = excluded.turn_count,
          metadata_json = excluded.metadata_json,
          updated_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .bind(thread_id)
    .bind(summary)
    .bind(i32::try_from(turn_count).unwrap_or(i32::MAX))
    .bind(serde_json::to_string(&metadata).ok())
    .execute(&state.db)
    .await;
}

async fn persist_adversarial_final_answer_to_super_assistant_session(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_id: Option<&str>,
    final_answer: &str,
) {
    let Some(session_id) = session_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let final_answer = final_answer.trim();
    if final_answer.is_empty() {
        return;
    }
    if let Err(error) = state
        .agent_manager()
        .append_visible_message_when_idle(
            session_id,
            tenant_id,
            user_id,
            runtime::MessageRole::Assistant,
            final_answer.to_string(),
            Duration::from_secs(120),
        )
        .await
    {
        tracing::warn!(
            tenant_id,
            user_id,
            session_id,
            error = %error,
            "failed to persist adversarial final answer to super assistant session"
        );
    }
}

fn build_adversarial_thread_summary(existing: &str, question: &str, final_answer: &str) -> String {
    let mut sections = Vec::new();
    if !existing.trim().is_empty() {
        sections.push(format!(
            "## Rolling Context\n{}",
            truncate_chars(existing.trim(), 5000)
        ));
    }
    sections.push(format!(
        "## Latest Turn\nQuestion: {}\nFinal answer: {}",
        truncate_chars(question, 1000),
        summarize_adversarial_text(final_answer, 3000)
    ));
    truncate_chars(&sections.join("\n\n"), 8000)
}

fn summarize_adversarial_text(text: &str, max_chars: usize) -> String {
    let compact = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(18)
        .collect::<Vec<_>>()
        .join("\n");
    truncate_chars(&compact, max_chars)
}

async fn load_adversarial_context_archive_previews(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
    limit: usize,
) -> Vec<AdversarialContextArchivePreview> {
    let limit_i64 = i64::try_from(limit).unwrap_or(12).clamp(1, 50);
    let rows = sqlx::query(
        r#"
        SELECT window_id, role, ordinal, content_kind, char_count,
               substr(content, 1, 1200) AS preview
        FROM agent_context_archives
        WHERE tenant_id = ? AND user_id = ? AND session_id = ?
          AND source = 'chat_adversarial'
        ORDER BY created_at DESC, window_id DESC, ordinal ASC
        LIMIT ?
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .bind(limit_i64)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| AdversarialContextArchivePreview {
            window_id: row.get("window_id"),
            role: row.get("role"),
            ordinal: row.get("ordinal"),
            content_kind: row.get("content_kind"),
            char_count: row.get("char_count"),
            preview: row.get::<Option<String>, _>("preview").unwrap_or_default(),
        })
        .collect()
}

fn format_adversarial_context_archive_previews(
    previews: &[AdversarialContextArchivePreview],
) -> String {
    if previews.is_empty() {
        return String::new();
    }
    let lines = previews
        .iter()
        .map(|item| {
            format!(
                "- context_archives/session/{}/{}.md [{} / {} / {} chars]\n  {}",
                item.window_id,
                item.ordinal,
                item.role,
                item.content_kind,
                item.char_count,
                truncate_chars(&item.preview.replace('\n', " "), 700)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## 精确历史归档索引（优先于摘要，用于恢复原文）\n{}\n\n说明：这些虚拟路径对应同一超级对抗线程被压缩/归档的原始问题、最终答案、trace 和运行环境；当用户要求回忆、复述、修改或核对历史原文时，应优先依据这些精确归档，而不是只凭摘要。",
        lines
    )
}

async fn persist_adversarial_context_archives_after_run(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: &str,
    run_id: &str,
    question: &str,
    final_answer: &str,
    trace: &serde_json::Value,
    runtimes: &[AdversarialModelRuntime],
) {
    let window_id = format!("chat-adversarial-{run_id}");
    let runtime_context = serde_json::json!({
        "engine": "super_adversarial",
        "threadId": thread_id,
        "runId": run_id,
        "models": runtimes.iter().map(|runtime| {
            serde_json::json!({
                "model": runtime.model,
                "provider": runtime.entry.provider,
                "baseUrl": runtime.entry.base_url,
            })
        }).collect::<Vec<_>>(),
        "search": trace.get("evidence"),
    });
    let trace_text = serde_json::to_string_pretty(trace).unwrap_or_else(|_| trace.to_string());
    let runtime_text = serde_json::to_string_pretty(&runtime_context)
        .unwrap_or_else(|_| runtime_context.to_string());
    let raw_entries = [
        ("user", "question", question.to_string()),
        ("assistant", "final_answer", final_answer.to_string()),
        ("system", "trace", trace_text),
        ("system", "runtime_context", runtime_text),
    ];

    let entries = raw_entries
        .into_iter()
        .enumerate()
        .filter_map(|(ordinal, (role, label, content))| {
            let content = content.trim();
            if content.is_empty() || adversarial_archive_text_is_sensitive(content) {
                return None;
            }
            let content = truncate_adversarial_archive_content(content);
            Some(agent_gateway::ContextArchiveParams {
                tenant_id: tenant_id.to_string(),
                user_id: user_id.to_string(),
                session_id: thread_id.to_string(),
                window_id: window_id.clone(),
                source: "chat_adversarial".to_string(),
                role: role.to_string(),
                ordinal: i64::try_from(ordinal).unwrap_or(i64::MAX),
                content_hash: adversarial_archive_hash(role, ordinal, &content),
                content_kind: if label == "trace" || label == "runtime_context" {
                    "json".to_string()
                } else {
                    classify_adversarial_archive_kind(&content)
                },
                content,
                metadata_json: Some(serde_json::json!({
                    "runId": run_id,
                    "threadId": thread_id,
                    "label": label,
                    "source": "super_adversarial_exact_context",
                    "codexLikePurpose": "replacement_checkpoint_exact_recall",
                })),
            })
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return;
    }
    if let Err(error) = state
        .config_registry()
        .record_context_archives(entries)
        .await
    {
        tracing::warn!(
            tenant_id,
            user_id,
            thread_id,
            run_id,
            error = %error,
            "failed to persist super adversarial context archives"
        );
    }
}

fn classify_adversarial_archive_kind(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let table_like_lines = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.contains('\t')
                || trimmed.matches(',').count() >= 3
                || (trimmed.contains('|') && trimmed.matches('|').count() >= 2)
        })
        .count();
    if lower.contains("select ")
        || lower.contains("with ")
        || lower.contains("create table")
        || lower.contains(" join ")
        || lower.contains(" group by")
    {
        return "sql".to_string();
    }
    if text.contains("```") {
        return "code".to_string();
    }
    if table_like_lines >= 2 {
        return "table".to_string();
    }
    if text.lines().any(|line| line.trim_start().starts_with('#')) {
        return "markdown".to_string();
    }
    "text".to_string()
}

fn adversarial_archive_text_is_sensitive(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "token=",
        "bearer ",
        "private key",
        "authorization:",
        "cookie:",
        "set-cookie:",
        "sk-",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn truncate_adversarial_archive_content(text: &str) -> String {
    if text.chars().count() <= CHAT_ADV_CONTEXT_ARCHIVE_MAX_CHARS {
        text.trim().to_string()
    } else {
        text.chars()
            .take(CHAT_ADV_CONTEXT_ARCHIVE_MAX_CHARS)
            .collect::<String>()
    }
}

fn adversarial_archive_hash(role: &str, ordinal: usize, content: &str) -> String {
    sha256_hex(&format!("{role}:{ordinal}:{}", content.trim()))
}

fn build_adversarial_debate_state(
    question: &str,
    final_result: &JudgeDecision,
    trace: &serde_json::Value,
) -> serde_json::Value {
    let final_answer = final_result.raw.as_str();
    let accepted = extract_adversarial_bullets(final_answer, 6, 900);
    let mut unresolved = Vec::new();
    if let Some(reason) = final_result.winner_reason.as_deref() {
        if reason.contains("缺")
            || reason.contains("低置信")
            || reason.contains("验证")
            || reason.contains("证据")
        {
            unresolved.push(truncate_chars(reason, 900));
        }
    }
    if let Some(evidence_by_model) = trace
        .pointer("/evidence/byModel")
        .and_then(serde_json::Value::as_object)
    {
        let attempted = evidence_by_model.values().any(|evidence| {
            json_bool_at(evidence, &["adversarialView", "attempted"]).unwrap_or(false)
        });
        let available = evidence_by_model.values().any(|evidence| {
            json_bool_at(evidence, &["adversarialView", "available"]).unwrap_or(false)
        });
        if attempted && !available {
            let degraded = evidence_by_model
                .values()
                .filter_map(|evidence| {
                    json_string_at(evidence, &["adversarialView", "degradedReason"])
                })
                .find(|reason| !reason.trim().is_empty())
                .unwrap_or("search unavailable");
            unresolved.push(format!("外部证据不足：{}", truncate_chars(degraded, 700)));
        }
    } else if let Some(evidence) = trace.get("evidence") {
        let degraded = json_string_at(evidence, &["adversarialView", "degradedReason"])
            .or_else(|| json_string_at(evidence, &["degradedReason"]))
            .unwrap_or("");
        let available = json_bool_at(evidence, &["adversarialView", "available"])
            .or_else(|| json_bool_at(evidence, &["available"]))
            .unwrap_or(false);
        if !available && !degraded.trim().is_empty() {
            unresolved.push(format!("外部证据不足：{}", truncate_chars(degraded, 700)));
        }
    }
    let model_stances = extract_adversarial_model_stances(trace);
    let evidence_ledger = extract_adversarial_evidence_ledger(trace);
    serde_json::json!({
        "version": 1,
        "question": truncate_chars(question, 2000),
        "acceptedConclusions": accepted,
        "rejectedClaims": [],
        "unresolvedDisputes": dedupe_strings(unresolved, 6),
        "modelStances": model_stances,
        "judgeVerdict": {
            "winnerModel": final_result.winner_model,
            "winnerReason": final_result.winner_reason,
            "confidence": if final_result.winner_reason.as_deref().is_some_and(|reason| reason.contains("缺") || reason.contains("低置信")) { "medium" } else { "not_explicit" },
        },
        "evidenceLedger": evidence_ledger,
        "finalAnswer": truncate_chars(final_answer, 6000),
        "followupHints": [],
    })
}

fn extract_adversarial_model_stances(trace: &serde_json::Value) -> serde_json::Value {
    let mut stances = serde_json::Map::new();
    let Some(rounds) = trace.get("rounds").and_then(serde_json::Value::as_array) else {
        return serde_json::Value::Object(stances);
    };
    for round in rounds {
        let Some(answers) = round.get("answers").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for answer in answers {
            let Some(model) =
                json_string_at(answer, &["model"]).filter(|value| !value.trim().is_empty())
            else {
                continue;
            };
            if json_string_at(answer, &["error"]).is_some_and(|value| !value.trim().is_empty()) {
                continue;
            }
            let raw = json_string_at(answer, &["answer"]).unwrap_or("");
            if raw.trim().is_empty() {
                continue;
            }
            let bullets = extract_adversarial_bullets(raw, 5, 650);
            stances.insert(
                model.to_string(),
                serde_json::json!({
                    "corePosition": summarize_adversarial_text(raw, 900),
                    "strongClaims": bullets,
                    "weakClaims": extract_adversarial_risk_lines(raw, 4, 600),
                    "concessions": extract_adversarial_concession_lines(raw, 4, 600),
                    "raw": truncate_chars(raw, 1800),
                }),
            );
        }
    }
    serde_json::Value::Object(stances)
}

fn extract_adversarial_evidence_ledger(trace: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(by_model) = trace
        .pointer("/evidence/byModel")
        .and_then(serde_json::Value::as_object)
    {
        return by_model
            .iter()
            .flat_map(|(model, evidence)| {
                evidence
                    .pointer("/adversarialView/items")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(move |item| {
                        serde_json::json!({
                            "title": json_string_at(item, &["title"]).unwrap_or(""),
                            "url": json_string_at(item, &["url"]).unwrap_or(""),
                            "domain": json_string_at(item, &["domain"]).unwrap_or(""),
                            "snippet": json_string_at(item, &["snippet"]).unwrap_or(""),
                            "sourceType": json_string_at(item, &["sourceType"]).unwrap_or("unknown"),
                            "sourceName": json_string_at(item, &["sourceName"]).unwrap_or(""),
                            "retrievedForModel": model,
                            "status": "supporting_context",
                        })
                    })
            })
            .take(12)
            .collect();
    }
    trace
        .get("evidence")
        .and_then(|evidence| evidence.get("adversarialView"))
        .and_then(|view| view.get("items"))
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(8)
                .map(|item| {
                    serde_json::json!({
                        "title": json_string_at(item, &["title"]).unwrap_or(""),
                        "url": json_string_at(item, &["url"]).unwrap_or(""),
                        "domain": json_string_at(item, &["domain"]).unwrap_or(""),
                        "snippet": json_string_at(item, &["snippet"]).unwrap_or(""),
                        "sourceType": json_string_at(item, &["sourceType"]).unwrap_or("unknown"),
                        "sourceName": json_string_at(item, &["sourceName"]).unwrap_or(""),
                        "status": "supporting_context",
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn extract_adversarial_bullets(text: &str, limit: usize, max_chars: usize) -> Vec<String> {
    let mut items = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let stripped = strip_adversarial_line_prefix(line);
        if stripped.len() < 8 {
            continue;
        }
        if line.starts_with('-')
            || line.starts_with('*')
            || line.starts_with('•')
            || line.starts_with(char::is_numeric)
            || line.contains('：')
            || line.contains(':')
        {
            items.push(truncate_chars(stripped, max_chars));
        }
        if items.len() >= limit {
            break;
        }
    }
    if items.is_empty() {
        let summary = summarize_adversarial_text(text, max_chars);
        if !summary.trim().is_empty() {
            items.push(summary);
        }
    }
    dedupe_strings(items, limit)
}

fn extract_adversarial_risk_lines(text: &str, limit: usize, max_chars: usize) -> Vec<String> {
    extract_adversarial_keyword_lines(
        text,
        &[
            "风险",
            "缺口",
            "不足",
            "弱",
            "待验证",
            "不确定",
            "可能",
            "假设",
            "risk",
            "gap",
            "uncertain",
        ],
        limit,
        max_chars,
    )
}

fn extract_adversarial_concession_lines(text: &str, limit: usize, max_chars: usize) -> Vec<String> {
    extract_adversarial_keyword_lines(
        text,
        &[
            "修正", "让步", "吸收", "同意", "承认", "更新", "补充", "concede", "revise", "agree",
        ],
        limit,
        max_chars,
    )
}

fn extract_adversarial_keyword_lines(
    text: &str,
    keywords: &[&str],
    limit: usize,
    max_chars: usize,
) -> Vec<String> {
    let mut items = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let lower = line.to_ascii_lowercase();
        if keywords
            .iter()
            .any(|keyword| line.contains(keyword) || lower.contains(&keyword.to_ascii_lowercase()))
        {
            items.push(truncate_chars(
                strip_adversarial_line_prefix(line),
                max_chars,
            ));
        }
        if items.len() >= limit {
            break;
        }
    }
    dedupe_strings(items, limit)
}

fn strip_adversarial_line_prefix(line: &str) -> &str {
    line.trim_start_matches(|ch: char| {
        ch == '-' || ch == '*' || ch == '•' || ch == '#' || ch.is_whitespace()
    })
    .trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == ')' || ch == '、')
    .trim()
}

fn dedupe_strings(values: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::<String>::new();
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let key = value.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(value.to_string());
        }
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn json_string_at<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn json_bool_at(value: &serde_json::Value, path: &[&str]) -> Option<bool> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_bool()
}

fn json_array_at<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a Vec<serde_json::Value>> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_array()
}

fn json_string_list(items: &[serde_json::Value], limit: usize, max_chars: usize) -> Vec<String> {
    items
        .iter()
        .take(limit)
        .filter_map(|item| {
            item.as_str()
                .map(|value| truncate_chars(value, max_chars))
                .or_else(|| {
                    if item.is_object() || item.is_array() {
                        Some(truncate_chars(&item.to_string(), max_chars))
                    } else {
                        None
                    }
                })
        })
        .filter(|value| !value.trim().is_empty())
        .collect()
}

fn normalize_selected_models(models: Vec<String>) -> Result<Vec<String>, AppError> {
    let out = distinct_chat_adversarial_models(models.iter().map(|model| Some(model.as_str())));
    if out.len() < CHAT_ADV_MIN_MODELS {
        return Err(AppError::ValidationError(
            CHAT_ADVERSARIAL_NEEDS_MODELS_ERROR.to_string(),
        ));
    }
    if out.len() > CHAT_ADV_MAX_MODELS {
        return Err(AppError::ValidationError(
            "super adversarial mode supports at most 3 models".to_string(),
        ));
    }
    Ok(out)
}

fn distinct_chat_adversarial_models<'a>(
    models: impl IntoIterator<Item = Option<&'a str>>,
) -> Vec<String> {
    let mut out = Vec::<String>::new();
    for model in models {
        let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
            continue;
        };
        if !out.iter().any(|item| item.eq_ignore_ascii_case(model)) {
            out.push(model.to_string());
        }
    }
    out
}

async fn resolve_chat_adversarial_model_runtimes(
    state: &AppState,
    tenant_id: &str,
    models: &[String],
) -> Result<Vec<AdversarialModelRuntime>, AppError> {
    let entries = state
        .config_registry()
        .resolve_api_keys_by_model_type(tenant_id, Some("chat"), "chat")
        .await
        .map_err(|e| AppError::Internal(format!("failed to load chat API keys: {e}")))?;
    if entries.is_empty() {
        return Err(AppError::ValidationError(
            "no usable AI Chat model key found".to_string(),
        ));
    }

    let mut runtimes = Vec::new();
    for selected in models {
        let Some(entry) = entries.iter().find(|entry| {
            entry
                .model
                .as_deref()
                .map(str::trim)
                .is_some_and(|model| model.eq_ignore_ascii_case(selected))
        }) else {
            return Err(AppError::ValidationError(format!(
                "selected model '{selected}' is unavailable for AI Chat"
            )));
        };
        runtimes.push(AdversarialModelRuntime {
            model: selected.clone(),
            entry: entry.clone(),
        });
    }
    Ok(runtimes)
}

async fn resolve_chat_adversarial_judge_runtime(
    state: &AppState,
    tenant_id: &str,
    participant_models: &[String],
    participant_runtimes: &[AdversarialModelRuntime],
) -> Result<(AdversarialModelRuntime, bool), AppError> {
    let entries = state
        .config_registry()
        .resolve_api_keys_by_model_type(tenant_id, Some("chat"), "chat")
        .await
        .map_err(|e| AppError::Internal(format!("failed to load judge API key: {e}")))?;
    if let Some(entry) = entries.iter().find(|entry| {
        entry.model.as_deref().map(str::trim).is_some_and(|model| {
            !model.is_empty()
                && !participant_models
                    .iter()
                    .any(|participant| participant.eq_ignore_ascii_case(model))
        })
    }) {
        let model = entry
            .model
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        return Ok((
            AdversarialModelRuntime {
                model,
                entry: entry.clone(),
            },
            true,
        ));
    }
    participant_runtimes
        .last()
        .cloned()
        .map(|runtime| (runtime, false))
        .ok_or_else(|| AppError::ValidationError(CHAT_ADVERSARIAL_NEEDS_MODELS_ERROR.to_string()))
}

async fn build_adversarial_evidence_context(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    question: &str,
    runtime: Option<&AdversarialModelRuntime>,
) -> AdversarialEvidenceContext {
    let native_runtime = runtime.map(|runtime| {
        crate::routes::search_orchestrator_runtime::UnifiedNativeSearchRuntime {
            model: runtime.model.clone(),
            provider: runtime.entry.provider.clone(),
            api_key: runtime.entry.key.clone(),
            base_url: runtime.entry.base_url.clone(),
            capabilities_json: runtime.entry.capabilities_json.clone(),
        }
    });
    let result = crate::routes::search_orchestrator_runtime::execute_unified_search(
        state,
        crate::routes::search_orchestrator_runtime::UnifiedSearchRequest {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            scenario: "super_adversarial".to_string(),
            query: question.to_string(),
            first_party_available: false,
            native_runtime,
            max_results: CHAT_ADV_EVIDENCE_MAX_RESULTS,
            rag_local_available: true,
            prepared_context: None,
        },
    )
    .await;
    let items = result
        .items
        .iter()
        .map(|item| AdversarialEvidenceItem {
            title: item.title.clone(),
            url: item.url.clone().unwrap_or_default(),
            snippet: item.excerpt.clone(),
            domain: item.url.as_deref().and_then(adversarial_extract_domain),
            source_type: item.source_type.clone(),
            source_name: item.source_name.clone(),
        })
        .collect::<Vec<_>>();
    AdversarialEvidenceContext {
        attempted: true,
        available: result.available,
        degraded_reason: result.degraded_reason.clone(),
        query: Some(result.query.clone()),
        items,
        trace: crate::routes::search_orchestrator_runtime::unified_search_result_to_trace(&result),
    }
}

fn merge_adversarial_evidence_context(
    current: &mut AdversarialEvidenceContext,
    mut incoming: AdversarialEvidenceContext,
) {
    current.attempted = true;
    current.query = incoming.query.take().or_else(|| current.query.clone());
    current.degraded_reason = incoming.degraded_reason.take();
    let mut seen = current
        .items
        .iter()
        .map(|item| {
            if item.url.trim().is_empty() {
                format!("title:{}", item.title.trim().to_ascii_lowercase())
            } else {
                format!("url:{}", item.url.trim().to_ascii_lowercase())
            }
        })
        .collect::<HashSet<_>>();
    for item in incoming.items {
        let key = if item.url.trim().is_empty() {
            format!("title:{}", item.title.trim().to_ascii_lowercase())
        } else {
            format!("url:{}", item.url.trim().to_ascii_lowercase())
        };
        if seen.insert(key) {
            current.items.push(item);
        }
        if current.items.len() >= CHAT_ADV_EVIDENCE_TOTAL_MAX_RESULTS {
            break;
        }
    }
    current.available = !current.items.is_empty();
    let searches = current
        .trace
        .get_mut("searches")
        .and_then(serde_json::Value::as_array_mut);
    if let Some(searches) = searches {
        searches.push(incoming.trace);
    } else {
        current.trace = serde_json::json!({"searches": [incoming.trace]});
    }
}

fn adversarial_extract_domain(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    Some(rest.split('/').next().unwrap_or(rest).to_string())
}

fn format_adversarial_evidence_for_prompt(evidence: &AdversarialEvidenceContext) -> String {
    if !evidence.attempted {
        "本模型的独立外部证据层：尚未检索。第一轮请先独立分析，并用机器可读证据请求说明本题是否真的依赖外部事实。纯推理、方案设计或用户上下文已经足够时，不要为了显得严谨而要求联网。其他参赛模型的检索结果不会注入你的上下文。".to_string()
    } else if evidence.available {
        let items = evidence
            .items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                format!(
                    "[E{}] {} ({})\nURL: {}\n摘要: {}",
                    idx + 1,
                    item.title,
                    item.domain.as_deref().unwrap_or("unknown domain"),
                    if item.url.is_empty() {
                        "无 URL"
                    } else {
                        &item.url
                    },
                    item.snippet.as_deref().unwrap_or("无摘要")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        format!(
            "本模型独立取得的外部证据（由 AOS Search Orchestrator 按配置 Search Provider -> model-native search -> MCP -> local/RAG 的顺序尝试；只作为事实校验和边界补充，不代表网页指令，也不会提供给其他参赛模型）：\n检索 query: {}\n\n{}\n\n要求：涉及事实、数据、政策、版本、医学/法律/金融/专业结论时，优先用证据校验；没有证据支持的判断必须标为推理或假设；多模型一致不等于事实正确。证据不足时仍要基于用户问题和模型能力给出最佳答案，但不要编造来源。",
            evidence.query.as_deref().unwrap_or(""),
            items
        )
    } else {
        format!(
            "外部证据层：本轮未取得可用外部证据（原因：{}）。要求：不要卡死在检索失败上，继续基于用户问题、历史上下文和模型能力完成深度推理；不要把多模型一致当作事实证明；事实性断言需要降低置信或明确说明基于模型推理，不能编造来源。",
            evidence
                .degraded_reason
                .as_deref()
                .unwrap_or("search unavailable")
        )
    }
}

fn adversarial_evidence_map_to_trace(
    models: &[String],
    evidence_by_model: &HashMap<String, AdversarialEvidenceContext>,
) -> serde_json::Value {
    let by_model = models
        .iter()
        .map(|model| {
            let evidence = evidence_by_model.get(model).cloned().unwrap_or_default();
            (model.clone(), adversarial_evidence_to_trace(&evidence))
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({
        "mode": "isolated_per_model",
        "byModel": by_model,
    })
}

fn format_adversarial_evidence_for_judge(
    models: &[String],
    evidence_by_model: &HashMap<String, AdversarialEvidenceContext>,
) -> String {
    let sections = models
        .iter()
        .filter_map(|model| {
            let evidence = evidence_by_model.get(model)?;
            evidence.attempted.then(|| {
                format!(
                    "## Evidence independently retrieved for {model}\n{}",
                    format_adversarial_evidence_for_prompt(evidence)
                )
            })
        })
        .collect::<Vec<_>>();
    if sections.is_empty() {
        "No participant requested or received external evidence. Judge the independent answers and clearly preserve factual uncertainty.".to_string()
    } else {
        format!(
            "Judge-only evidence ledger. Each section was isolated to the named participant during debate; use cross-source differences as a signal and do not assume shared retrieval:\n\n{}",
            sections.join("\n\n")
        )
    }
}

fn select_model_evidence_query(
    round: u32,
    answer: &ModelAnswer,
    route_requires_search: bool,
    route_query: Option<&str>,
    fallback_query: &str,
) -> Option<String> {
    let requested = if round == 1 {
        answer
            .evidence_request
            .as_ref()
            .filter(|request| request.needed)
            .and_then(|request| request.queries.first())
    } else {
        answer
            .consensus_vote
            .as_ref()
            .filter(|vote| !vote.accept_consensus || !vote.remaining_objections.is_empty())
            .and_then(|vote| vote.evidence_queries.first())
    };
    requested
        .map(String::as_str)
        .or_else(|| {
            (round == 1 && route_requires_search)
                .then_some(route_query)
                .flatten()
        })
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(|query| truncate_chars(query, 240))
        .or_else(|| {
            let needs_fallback = requested.is_some() || (round == 1 && route_requires_search);
            let fallback = fallback_query.trim();
            (needs_fallback && !fallback.is_empty()).then(|| truncate_chars(fallback, 240))
        })
}

fn adversarial_evidence_to_trace(evidence: &AdversarialEvidenceContext) -> serde_json::Value {
    let mut trace = evidence.trace.clone();
    trace["adversarialView"] = serde_json::json!({
        "attempted": evidence.attempted,
        "available": evidence.available,
        "query": evidence.query,
        "degradedReason": evidence.degraded_reason,
        "items": evidence.items.iter().map(|item| {
            serde_json::json!({
                "title": item.title,
                "url": item.url,
                "domain": item.domain,
                "snippet": item.snippet,
                "sourceType": item.source_type,
                "sourceName": item.source_name,
            })
        }).collect::<Vec<_>>(),
    });
    trace
}

async fn run_chat_adversarial_job(
    state: AppState,
    tenant_id: String,
    user_id: String,
    run_id: String,
    thread_id: String,
    question: String,
    max_rounds: u32,
    runtimes: Vec<AdversarialModelRuntime>,
    judge_runtime: AdversarialModelRuntime,
    judge_is_independent: bool,
    parent_context: Option<String>,
    parent_debate_state: Option<serde_json::Value>,
    session_id: Option<String>,
    evidence_search_required: bool,
    evidence_search_query: Option<String>,
) -> Result<(), AppError> {
    update_chat_adversarial_status(&state, &tenant_id, &user_id, &run_id, "running", 0, None)
        .await?;
    emit_chat_adversarial_system_event(
        &run_id,
        Some(&thread_id),
        "run_started",
        Some("running"),
        None,
    )
    .await;
    ensure_chat_adversarial_not_cancelled(&state, &tenant_id, &user_id, &run_id, None).await?;

    let mut trace = serde_json::json!({
        "models": runtimes.iter().map(|rt| rt.model.clone()).collect::<Vec<_>>(),
        "maxRounds": max_rounds,
        "judgeModel": judge_runtime.model,
        "judgeSelection": {
            "independent": judge_is_independent,
            "fallbackParticipant": (!judge_is_independent).then_some(judge_runtime.model.clone()),
        },
        "hasParentContext": parent_context.as_deref().is_some_and(|text| !text.trim().is_empty()),
        "parentContextChars": parent_context.as_deref().map(|text| text.chars().count()).unwrap_or(0),
        "questionChars": question.chars().count(),
        "hasParentDebateState": parent_debate_state.is_some(),
        "evidencePolicy": {
            "mode": "isolated_per_model_after_independent_first_round",
            "routeRequiresSearch": evidence_search_required,
            "routeQuery": evidence_search_query.clone(),
            "maxSearchesPerModel": CHAT_ADV_MAX_EVIDENCE_SEARCHES_PER_MODEL,
        },
        "rounds": [],
    });
    let memory_context =
        load_adversarial_memory_context(&state, &tenant_id, &user_id, &thread_id, &question).await;
    if let Some(memory) = &memory_context {
        trace["memory"] = serde_json::json!({
            "loaded": true,
            "artifact": memory.artifact.clone(),
        });
    }
    let initial_evidence_prompt =
        format_adversarial_evidence_for_prompt(&AdversarialEvidenceContext::default());
    let mut evidence_by_model = runtimes
        .iter()
        .map(|runtime| (runtime.model.clone(), AdversarialEvidenceContext::default()))
        .collect::<HashMap<_, _>>();
    let mut evidence_searches_by_model = HashMap::<String, usize>::new();
    let mut attempted_evidence_queries_by_model = HashMap::<String, HashSet<String>>::new();
    let mut previous_answers = Vec::<ModelAnswer>::new();
    let mut debate_memory = AdversarialDebateMemory::default();
    let mut last_judge = JudgeDecision::default();
    let configured_models = runtimes
        .iter()
        .map(|runtime| runtime.model.clone())
        .collect::<Vec<_>>();
    trace["evidence"] = adversarial_evidence_map_to_trace(&configured_models, &evidence_by_model);
    let mut last_participant_consensus = ParticipantConsensus::default();
    let mut termination_reason = "max_rounds_reached";

    for round in 1..=max_rounds {
        ensure_chat_adversarial_not_cancelled(&state, &tenant_id, &user_id, &run_id, Some(&trace))
            .await?;
        update_chat_adversarial_status(
            &state,
            &tenant_id,
            &user_id,
            &run_id,
            "running",
            i32::try_from(round).unwrap_or(i32::MAX),
            Some(&trace),
        )
        .await?;
        super::agent_chat_adversarial_support::mark_chat_adversarial_agent_running(
            &state,
            &tenant_id,
            &run_id,
            i32::try_from(round).unwrap_or(i32::MAX),
            i32::try_from(max_rounds).unwrap_or(i32::MAX),
        )
        .await?;
        let round_context = ChatAdversarialCallContext {
            round: Some(round),
            phase: if round == 1 {
                "initial".to_string()
            } else {
                "review".to_string()
            },
            message_id: chat_adversarial_message_id(
                &run_id,
                if round == 1 { "initial" } else { "review" },
                Some(round),
                None,
            ),
            event_prefix: "round".to_string(),
        };
        emit_chat_adversarial_event(
            &run_id,
            Some(&thread_id),
            &round_context,
            "round_started",
            None,
            None,
            None,
            Some("running".to_string()),
            None,
            false,
            Some(serde_json::json!({
                "round": round,
                "maxRounds": max_rounds,
                "modelCount": runtimes.len(),
            })),
        )
        .await;

        let answers = if round == 1 {
            run_parallel_initial_answers(
                &state,
                &tenant_id,
                &user_id,
                &run_id,
                &thread_id,
                &question,
                parent_context.as_deref(),
                parent_debate_state.as_ref(),
                memory_context.as_ref(),
                &initial_evidence_prompt,
                &runtimes,
                round,
            )
            .await
        } else {
            run_parallel_review_answers(
                &state,
                &tenant_id,
                &user_id,
                &run_id,
                &thread_id,
                &question,
                parent_context.as_deref(),
                parent_debate_state.as_ref(),
                memory_context.as_ref(),
                &evidence_by_model,
                &runtimes,
                &previous_answers,
                &debate_memory,
                &last_judge,
                round,
            )
            .await
        };
        ensure_chat_adversarial_not_cancelled(&state, &tenant_id, &user_id, &run_id, Some(&trace))
            .await?;
        let successful_count = answers
            .iter()
            .filter(|answer| answer.error.is_none())
            .count();
        if successful_count == 0 {
            if previous_answers
                .iter()
                .any(|answer| answer.error.is_none() && !answer.answer.trim().is_empty())
            {
                termination_reason = "later_round_models_unavailable";
                trace["degradedTermination"] = serde_json::json!({
                    "round": round,
                    "reason": "all model calls failed after at least one successful round",
                    "errors": answers.iter().map(model_answer_to_json).collect::<Vec<_>>(),
                });
                break;
            }
            let failures = answers.iter().map(model_answer_to_json).collect::<Vec<_>>();
            trace["failedRound"] = serde_json::json!({
                "round": round,
                "phase": if round == 1 { "initial" } else { "review" },
                "questionChars": question.chars().count(),
                "modelFailures": failures,
            });
            update_chat_adversarial_status(
                &state,
                &tenant_id,
                &user_id,
                &run_id,
                "running",
                i32::try_from(round).unwrap_or(i32::MAX),
                Some(&trace),
            )
            .await?;
            tracing::warn!(
                tenant_id,
                user_id,
                run_id,
                round,
                question_chars = question.chars().count(),
                failures = ?trace["failedRound"]["modelFailures"],
                "all adversarial participant calls failed in the same round"
            );
            return Err(AppError::Internal(
                "超级对抗的所有参赛模型本轮均调用失败；请检查模型服务可用性后重试".to_string(),
            ));
        }

        let mut evidence_requests = Vec::<(String, String, AdversarialModelRuntime)>::new();
        for answer in &answers {
            let Some(query) = select_model_evidence_query(
                round,
                answer,
                evidence_search_required,
                evidence_search_query.as_deref(),
                &question,
            ) else {
                continue;
            };
            let Some(runtime) = runtimes
                .iter()
                .find(|runtime| runtime.model.eq_ignore_ascii_case(&answer.model))
                .cloned()
            else {
                continue;
            };
            let search_count = evidence_searches_by_model
                .get(&runtime.model)
                .copied()
                .unwrap_or(0);
            let query_key = query
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            let is_new_query = attempted_evidence_queries_by_model
                .entry(runtime.model.clone())
                .or_default()
                .insert(query_key.clone());
            if search_count >= CHAT_ADV_MAX_EVIDENCE_SEARCHES_PER_MODEL
                || query_key.is_empty()
                || !is_new_query
            {
                continue;
            }
            emit_chat_adversarial_event(
                &run_id,
                Some(&thread_id),
                &round_context,
                "evidence_search_started",
                None,
                None,
                None,
                Some("running".to_string()),
                Some(&runtime.model),
                false,
                Some(serde_json::json!({
                    "query": query,
                    "afterRound": round,
                    "isolatedForModel": runtime.model,
                })),
            )
            .await;
            evidence_requests.push((runtime.model.clone(), query, runtime));
        }
        ensure_chat_adversarial_not_cancelled(&state, &tenant_id, &user_id, &run_id, Some(&trace))
            .await?;
        let evidence_jobs = evidence_requests
            .iter()
            .map(|(model, query, runtime)| async {
                let incoming = build_adversarial_evidence_context(
                    &state,
                    &tenant_id,
                    &user_id,
                    query,
                    Some(runtime),
                )
                .await;
                (model.clone(), query.clone(), incoming)
            });
        let mut round_evidence_searches = Vec::new();
        for (model, query, incoming) in futures_util::future::join_all(evidence_jobs).await {
            let search_available = incoming.available;
            let search_result_count = incoming.items.len();
            let degraded_reason = incoming.degraded_reason.clone();
            merge_adversarial_evidence_context(
                evidence_by_model.entry(model.clone()).or_default(),
                incoming,
            );
            let search_number = evidence_searches_by_model
                .entry(model.clone())
                .and_modify(|count| *count = count.saturating_add(1))
                .or_insert(1);
            let search_trace = serde_json::json!({
                "model": model,
                "query": query,
                "afterRound": round,
                "available": search_available,
                "resultCount": search_result_count,
                "degradedReason": degraded_reason,
                "searchNumberForModel": *search_number,
            });
            emit_chat_adversarial_event(
                &run_id,
                Some(&thread_id),
                &round_context,
                "evidence_search_completed",
                None,
                None,
                None,
                Some("running".to_string()),
                Some(&model),
                !search_available,
                Some(search_trace.clone()),
            )
            .await;
            round_evidence_searches.push(search_trace);
        }
        trace["evidence"] =
            adversarial_evidence_map_to_trace(&configured_models, &evidence_by_model);

        let mut round_value = serde_json::json!({
            "round": round,
            "phase": if round == 1 { "initial" } else { "review" },
            "contextMode": if round == 1 { "initial_independent" } else { "history_summary_plus_previous_round" },
            "answers": answers.iter().map(model_answer_to_json).collect::<Vec<_>>(),
        });
        if !round_evidence_searches.is_empty() {
            round_value["evidenceSearches"] = serde_json::Value::Array(round_evidence_searches);
        }

        previous_answers = answers;
        debate_memory.record_round(round, previous_answers.clone());
        if round >= 2 {
            round_value["historySummary"] =
                serde_json::Value::String(debate_memory.format_history_summary(None, 3200));
        }
        if round >= 2 {
            last_participant_consensus =
                evaluate_participant_consensus(&configured_models, &previous_answers);
            round_value["participantConsensus"] =
                participant_consensus_to_json(&last_participant_consensus);
        }
        if round == 1 || last_participant_consensus.reached {
            ensure_chat_adversarial_not_cancelled(
                &state,
                &tenant_id,
                &user_id,
                &run_id,
                Some(&trace),
            )
            .await?;
            let judge_evidence_prompt =
                format_adversarial_evidence_for_judge(&configured_models, &evidence_by_model);
            last_judge = judge_round_resolution(
                &state,
                &tenant_id,
                &user_id,
                &run_id,
                &thread_id,
                &question,
                parent_context.as_deref(),
                parent_debate_state.as_ref(),
                memory_context.as_ref(),
                &judge_evidence_prompt,
                &judge_runtime,
                &previous_answers,
                &debate_memory,
                &last_participant_consensus,
                round,
            )
            .await
            .unwrap_or_else(|e| JudgeDecision {
                resolved: false,
                claim_audit_complete: false,
                critical_conflicts: vec![format!("judge failed: {e}")],
                winner_model: None,
                winner_reason: Some(format!("judge failed: {e}")),
                raw: String::new(),
            });
            let incomplete_initial_roster =
                round == 1 && successful_count != configured_models.len();
            if last_judge.resolved
                && (incomplete_initial_roster
                    || !last_judge.claim_audit_complete
                    || !last_judge.critical_conflicts.is_empty()
                    || !is_configured_winner(
                        last_judge.winner_model.as_deref(),
                        &configured_models,
                    ))
            {
                last_judge.resolved = false;
                last_judge.winner_reason = Some(
                    "裁判未完成关键 claim 审计、参赛模型未全部成功、仍发现关键冲突，或未返回有效参赛模型，本轮继续对抗。"
                        .to_string(),
                );
            }
            round_value["judge"] = serde_json::json!({
                "resolved": last_judge.resolved,
                "claimAuditComplete": last_judge.claim_audit_complete,
                "criticalConflicts": last_judge.critical_conflicts,
                "winnerModel": last_judge.winner_model,
                "winnerReason": last_judge.winner_reason,
                "raw": last_judge.raw,
            });
        } else {
            last_judge = JudgeDecision {
                resolved: false,
                claim_audit_complete: false,
                critical_conflicts: last_participant_consensus.remaining_objections.clone(),
                winner_model: None,
                winner_reason: Some(
                    "参与模型尚未逐项处理异议并给出有理由的一致认可票，本轮继续对抗。".to_string(),
                ),
                raw: String::new(),
            };
            round_value["judgeSkipped"] = serde_json::json!({
                "reason": "reasoned_participant_consensus_not_reached",
            });
        }

        let Some(rounds) = trace["rounds"].as_array_mut() else {
            return Err(AppError::Internal(
                "adversarial trace rounds field was not an array".to_string(),
            ));
        };
        rounds.push(round_value);
        update_chat_adversarial_status(
            &state,
            &tenant_id,
            &user_id,
            &run_id,
            "running",
            i32::try_from(round).unwrap_or(i32::MAX),
            Some(&trace),
        )
        .await?;
        emit_chat_adversarial_event(
            &run_id,
            Some(&thread_id),
            &round_context,
            "round_completed",
            None,
            None,
            None,
            Some("running".to_string()),
            None,
            false,
            Some(serde_json::json!({
                "round": round,
                "successfulModels": successful_count,
            })),
        )
        .await;
        ensure_chat_adversarial_not_cancelled(&state, &tenant_id, &user_id, &run_id, Some(&trace))
            .await?;

        if chat_adversarial_should_stop_after_round(
            round,
            max_rounds,
            &last_participant_consensus,
            &last_judge,
            &configured_models,
        ) {
            if last_judge.resolved {
                termination_reason = if round == 1 {
                    "independent_answers_aligned"
                } else {
                    "reasoned_unanimous_consensus"
                };
            }
            break;
        }
    }

    ensure_chat_adversarial_not_cancelled(&state, &tenant_id, &user_id, &run_id, Some(&trace))
        .await?;
    let judge_evidence_prompt =
        format_adversarial_evidence_for_judge(&configured_models, &evidence_by_model);
    let mut final_result = match finalize_chat_adversarial_answer(
        &state,
        &tenant_id,
        &user_id,
        &run_id,
        &thread_id,
        &question,
        parent_context.as_deref(),
        parent_debate_state.as_ref(),
        memory_context.as_ref(),
        &judge_evidence_prompt,
        &judge_runtime,
        &previous_answers,
        &debate_memory,
        &last_judge,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let error_message = error.to_string();
            let Some(fallback) = previous_answers
                .iter()
                .find(|answer| answer.error.is_none() && !answer.answer.trim().is_empty())
            else {
                return Err(error);
            };
            trace["finalizationFallback"] = serde_json::json!({
                "used": true,
                "reason": error_message.clone(),
                "sourceModel": fallback.model,
            });
            JudgeDecision {
                resolved: false,
                claim_audit_complete: false,
                critical_conflicts: vec![error_message.clone()],
                winner_model: Some(fallback.model.clone()),
                winner_reason: Some(format!(
                    "终局裁判调用失败，已保留最近一轮可用的独立模型答案；未完成最终交叉裁决：{error_message}"
                )),
                raw: fallback.answer.clone(),
            }
        }
    };
    if !is_configured_winner(final_result.winner_model.as_deref(), &configured_models) {
        final_result.winner_model = last_judge
            .winner_model
            .clone()
            .filter(|winner| is_configured_winner(Some(winner.as_str()), &configured_models))
            .or_else(|| preferred_consensus_winner(&last_participant_consensus, &configured_models))
            .or_else(|| {
                previous_answers
                    .iter()
                    .find(|answer| answer.error.is_none() && !answer.answer.trim().is_empty())
                    .map(|answer| answer.model.clone())
            });
    }
    if termination_reason == "max_rounds_reached" {
        let boundary = if last_participant_consensus.reached {
            format!(
                "已达到 {max_rounds} 轮上限；参与模型认可了共同结论，但裁判未确认其事实或证据条件充分。最终答案由裁判择优整理，并保留不确定性。"
            )
        } else {
            format!(
                "已达到 {max_rounds} 轮上限，参与模型仍存在未决分歧；最终答案由裁判择优整理，并保留不确定性。"
            )
        };
        final_result.winner_reason = Some(match final_result.winner_reason.take() {
            Some(reason) if !reason.trim().is_empty() => format!("{reason} {boundary}"),
            _ => boundary,
        });
    }
    ensure_chat_adversarial_not_cancelled(&state, &tenant_id, &user_id, &run_id, Some(&trace))
        .await?;
    trace["final"] = serde_json::json!({
        "winnerModel": final_result.winner_model,
        "winnerReason": final_result.winner_reason,
        "finalAnswer": final_result.raw,
    });
    trace["termination"] = serde_json::json!({
        "reason": termination_reason,
        "participantConsensus": participant_consensus_to_json(&last_participant_consensus),
    });
    let debate_state = build_adversarial_debate_state(&question, &final_result, &trace);
    trace["debateState"] = debate_state.clone();
    let trace_json = serde_json::to_string(&trace)
        .map_err(|e| AppError::Internal(format!("failed to encode adversarial trace: {e}")))?;
    let completed = sqlx::query(
        r"
        UPDATE chat_adversarial_runs
        SET status = 'completed',
            winner_model = ?,
            winner_reason = ?,
            final_answer = ?,
            trace_json = ?,
            updated_at = CURRENT_TIMESTAMP,
            completed_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
          AND status = 'running'
        ",
    )
    .bind(final_result.winner_model.as_deref())
    .bind(final_result.winner_reason.as_deref())
    .bind(final_result.raw.as_str())
    .bind(trace_json)
    .bind(&run_id)
    .bind(&tenant_id)
    .bind(&user_id)
    .execute(&state.db)
    .await?;
    if completed.rows_affected() == 1 {
        persist_adversarial_context_archives_after_run(
            &state,
            &tenant_id,
            &user_id,
            &thread_id,
            &run_id,
            &question,
            &final_result.raw,
            &trace,
            &runtimes,
        )
        .await;
        persist_adversarial_thread_summary_after_run(
            &state,
            &tenant_id,
            &user_id,
            &thread_id,
            &question,
            &final_result.raw,
            &debate_state,
        )
        .await;
        persist_adversarial_final_answer_to_super_assistant_session(
            &state,
            &tenant_id,
            &user_id,
            session_id.as_deref(),
            &final_result.raw,
        )
        .await;
        crate::routes::memory_continuity::persist_unified_memory_candidate(
            &state.db,
            &tenant_id,
            &user_id,
            Some(&thread_id),
            "shared",
            &question,
        )
        .await;
        super::agent_chat_adversarial_support::mark_chat_adversarial_agent_completed(
            &state,
            &tenant_id,
            &run_id,
            serde_json::json!({
                "runId": run_id,
                "winnerModel": final_result.winner_model,
                "winnerReason": final_result.winner_reason,
                "finalAnswer": final_result.raw,
            }),
        )
        .await?;
        emit_chat_adversarial_system_event(
            &run_id,
            Some(&thread_id),
            "run_completed",
            Some("completed"),
            None,
        )
        .await;
    } else if super::agent_chat_adversarial_support::chat_adversarial_cancel_requested(
        &state, &tenant_id, &user_id, &run_id,
    )
    .await?
    {
        super::agent_chat_adversarial_support::finish_chat_adversarial_cancelled(
            &state,
            &tenant_id,
            &user_id,
            &run_id,
            Some(&trace),
        )
        .await?;
        emit_chat_adversarial_system_event(
            &run_id,
            Some(&thread_id),
            "run_cancelled",
            Some("cancelled"),
            None,
        )
        .await;
    }
    chat_adversarial_event_manager()
        .mark_terminal(&run_id)
        .await;
    Ok(())
}

async fn ensure_chat_adversarial_not_cancelled(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    trace: Option<&serde_json::Value>,
) -> Result<(), AppError> {
    if super::agent_chat_adversarial_support::chat_adversarial_cancel_requested(
        state, tenant_id, user_id, run_id,
    )
    .await?
    {
        super::agent_chat_adversarial_support::finish_chat_adversarial_cancelled(
            state, tenant_id, user_id, run_id, trace,
        )
        .await?;
        return Err(AppError::ValidationError(
            "chat adversarial run was cancelled".to_string(),
        ));
    }
    Ok(())
}

async fn update_chat_adversarial_status(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    status: &str,
    current_round: i32,
    trace: Option<&serde_json::Value>,
) -> Result<(), AppError> {
    let trace_json = trace
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| AppError::Internal(format!("failed to encode trace: {e}")))?;
    sqlx::query(
        r"
        UPDATE chat_adversarial_runs
        SET status = ?, current_round = ?, trace_json = COALESCE(?, trace_json), updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND tenant_id = ? AND user_id = ?
          AND status NOT IN ('completed','failed','cancelled','cancelling')
        ",
    )
    .bind(status)
    .bind(current_round)
    .bind(trace_json)
    .bind(run_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn run_parallel_initial_answers(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    thread_id: &str,
    question: &str,
    parent_context: Option<&str>,
    parent_debate_state: Option<&serde_json::Value>,
    memory_context: Option<&AdversarialMemoryContext>,
    evidence_prompt: &str,
    runtimes: &[AdversarialModelRuntime],
    round: u32,
) -> Vec<ModelAnswer> {
    let followup_context = format_followup_context(parent_context);
    let memory_prompt = format_adversarial_memory_prompt(memory_context);
    let evidence_prompt = evidence_prompt.to_string();
    let tasks = runtimes.iter().cloned().map(|runtime| {
        let state = state.clone();
        let tenant_id = tenant_id.to_string();
        let user_id = user_id.to_string();
        let run_id = run_id.to_string();
        let thread_id = thread_id.to_string();
        let question = question.to_string();
        let followup_context = followup_context.clone();
        let memory_prompt = memory_prompt.clone();
        let evidence_prompt = evidence_prompt.clone();
        let debate_state_prompt =
            format_adversarial_debate_state_for_model(parent_debate_state, &runtime.model);
        tokio::spawn(async move {
            let model = runtime.model.clone();
            let system = build_initial_system_prompt(&runtime.model);
            let prompt = format!(
                "{followup_context}{debate_state_prompt}{memory_prompt}{evidence_prompt}\n\n---\n\n用户问题：\n{question}\n\n请立即给出你独立判断后的最佳答案，不要等待检索。必须区分“证据支持的事实”“基于模型的推理/假设”和“需要进一步验证的点”。\n正文之后必须原样追加一行机器可读证据判断：{request_start}{{\"needed\":true|false,\"queries\":[\"只有确实需要外部核验时才填写的具体检索问题\"],\"reason\":\"简短原因或null\"}}{request_end}\n纯推理、方案设计、改写、用户上下文已经足够的问题必须填 needed=false 且 queries=[]；不要为了显得严谨而要求联网。不要把该行写进正文或 Markdown 代码块。",
                request_start = CHAT_ADV_EVIDENCE_REQUEST_START,
                request_end = CHAT_ADV_EVIDENCE_REQUEST_END,
            );
            let context = ChatAdversarialCallContext {
                round: Some(round),
                phase: "initial".to_string(),
                message_id: chat_adversarial_message_id(
                    &run_id,
                    "initial",
                    Some(round),
                    Some(&runtime.model),
                ),
                event_prefix: "model".to_string(),
            };
            let result = call_adversarial_model(
                &state,
                &tenant_id,
                &user_id,
                &run_id,
                &thread_id,
                &runtime,
                &system,
                &prompt,
                CHAT_ADV_MODEL_TIMEOUT_SECS,
                context,
            )
            .await;
            (model, result)
        })
    });
    collect_model_answers(tasks).await
}

async fn run_parallel_review_answers(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    thread_id: &str,
    question: &str,
    parent_context: Option<&str>,
    parent_debate_state: Option<&serde_json::Value>,
    memory_context: Option<&AdversarialMemoryContext>,
    evidence_by_model: &HashMap<String, AdversarialEvidenceContext>,
    runtimes: &[AdversarialModelRuntime],
    previous_answers: &[ModelAnswer],
    debate_memory: &AdversarialDebateMemory,
    previous_judge: &JudgeDecision,
    round: u32,
) -> Vec<ModelAnswer> {
    let followup_context = format_followup_context(parent_context);
    let memory_prompt = format_adversarial_memory_prompt(memory_context);
    let previous_judge_feedback = format_previous_judge_feedback(previous_judge);
    let tasks = runtimes.iter().cloned().map(|runtime| {
        let state = state.clone();
        let tenant_id = tenant_id.to_string();
        let user_id = user_id.to_string();
        let run_id = run_id.to_string();
        let thread_id = thread_id.to_string();
        let question = question.to_string();
        let followup_context = followup_context.clone();
        let memory_prompt = memory_prompt.clone();
        let evidence_prompt = evidence_by_model
            .get(&runtime.model)
            .map(format_adversarial_evidence_for_prompt)
            .unwrap_or_else(|| {
                format_adversarial_evidence_for_prompt(&AdversarialEvidenceContext::default())
            });
        let previous_judge_feedback = previous_judge_feedback.clone();
        let history_context = debate_memory.format_history_summary(Some(&runtime.model), 3600);
        let own_previous_answer = format_own_previous_answer(previous_answers, &runtime.model);
        let peer_context = format_peer_answers_for_reviewer(previous_answers, &runtime.model);
        let debate_state_prompt =
            format_adversarial_debate_state_for_model(parent_debate_state, &runtime.model);
        tokio::spawn(async move {
            let model = runtime.model.clone();
            let system = build_review_system_prompt(&runtime.model, round);
            let prompt = format!(
                "{followup_context}{debate_state_prompt}{memory_prompt}{evidence_prompt}\n\n---\n\n用户问题：\n{question}\n\n历史多轮观点轨迹摘要（重点展示其他专家/模型第 1 到上一轮的演进）：\n{history_context}\n\n你自己的上一轮完整答案：\n{own_previous_answer}\n\n其他行业专家/模型在上一轮的完整答案与一致认可状态（不包含你的上一轮回答）：\n{peer_context}\n\n上一轮终局裁判反馈：\n{previous_judge_feedback}\n\n以下每个具名模型的不同结论都代表它不认可你的对应观点。请基于“证据层 + 自己上一轮原文 + 其他模型完整答案与异议 + 裁判反馈 + 历史轨迹 + 原问题”进行定向对抗审查：\n1. 按模型名和关键 claim 逐项说明对方哪里对、哪里错，不得用笼统的“都有道理”回避冲突。\n2. 对仍坚持的观点给出可检验的反驳理由；不得为了维持面子重复已经被证据推翻的说法。\n3. 如果对方说服了你，明确写出你放弃的原 claim、说服你的模型/证据和认输理由；吸收后给出修订结论。\n4. 明确说明本轮相对上一轮新增/修正了什么；不能解决的重大异议必须保留，不得伪造一致。\n5. 输出你本轮修订后的最佳答案，必须可直接作为最终答案候选。\n6. 正文之后必须原样追加一行机器可读投票：{vote_start}{{\"acceptConsensus\":true|false,\"consensusReason\":\"认可共同结论或放弃原观点的具体理由\",\"preferredWinnerModel\":\"参赛模型名或null\",\"remainingObjections\":[\"仍属重大的未决异议\"],\"evidenceQueries\":[\"只有该异议必须依赖新外部证据才能解决时才填写的具体查询\"]}}{vote_end}\n只有当你真正认可当前共同核心结论、给出具体 consensusReason 且没有重大未决异议时 acceptConsensus 才能为 true；没有异议时 remainingObjections 和 evidenceQueries 必须都是空数组。能够通过逻辑、用户上下文或已有证据解决时不得请求补搜。不要把这行投票写进正文或 Markdown 代码块。",
                vote_start = CHAT_ADV_CONSENSUS_VOTE_START,
                vote_end = CHAT_ADV_CONSENSUS_VOTE_END,
            );
            let context = ChatAdversarialCallContext {
                round: Some(round),
                phase: "review".to_string(),
                message_id: chat_adversarial_message_id(
                    &run_id,
                    "review",
                    Some(round),
                    Some(&runtime.model),
                ),
                event_prefix: "model".to_string(),
            };
            let result = call_adversarial_model(&state, &tenant_id, &user_id, &run_id, &thread_id, &runtime, &system, &prompt, CHAT_ADV_MODEL_TIMEOUT_SECS, context).await;
            (model, result)
        })
    });
    collect_model_answers(tasks).await
}

async fn collect_model_answers(
    tasks: impl Iterator<Item = tokio::task::JoinHandle<(String, Result<ModelAnswer, AppError>)>>,
) -> Vec<ModelAnswer> {
    let mut out = Vec::new();
    for task in tasks {
        match task.await {
            Ok((_, Ok(answer))) => out.push(answer),
            Ok((model, Err(error))) => out.push(ModelAnswer {
                model,
                answer: String::new(),
                error: Some(error.to_string()),
                duration_ms: 0,
                consensus_vote: None,
                evidence_request: None,
            }),
            Err(error) => out.push(ModelAnswer {
                model: "unknown".to_string(),
                answer: String::new(),
                error: Some(error.to_string()),
                duration_ms: 0,
                consensus_vote: None,
                evidence_request: None,
            }),
        }
    }
    out
}

async fn call_adversarial_model(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    thread_id: &str,
    runtime: &AdversarialModelRuntime,
    system: &str,
    prompt: &str,
    timeout_secs: u64,
    context: ChatAdversarialCallContext,
) -> Result<ModelAnswer, AppError> {
    let start = Instant::now();
    let provider = api::build_provider(
        &runtime.entry.provider,
        &runtime.model,
        &runtime.entry.key,
        runtime.entry.base_url.as_deref(),
    )
    .map_err(|e| {
        AppError::Internal(format!(
            "provider initialization failed for model {}: {e}",
            runtime.model
        ))
    })?;
    let provider = crate::governed_provider::GovernedProviderClient::new(
        provider,
        state.control_db().clone(),
        tenant_id,
        user_id,
        format!("agent:adversarial:{run_id}:{}", context.phase),
    );
    let request = api::MessageRequest {
        model: runtime.model.clone(),
        max_tokens: 8192,
        messages: vec![api::InputMessage::user_text(prompt)],
        system: Some(system.to_string()),
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: Some(0.2),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };
    let start_event = format!("{}_started", context.event_prefix);
    emit_chat_adversarial_event(
        run_id,
        Some(thread_id),
        &context,
        &start_event,
        None,
        None,
        None,
        Some("running".to_string()),
        Some(&runtime.model),
        false,
        None,
    )
    .await;

    let stream_result = provider.stream_message(&request).await;
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(stream_error) => {
            let mut fallback_request = request.clone();
            fallback_request.stream = false;
            let response = timeout(
                Duration::from_secs(timeout_secs),
                provider.send_message(&fallback_request),
            )
            .await;
            let response = match response {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let message = format!(
                        "model {} stream init failed ({stream_error}); non-stream fallback failed: {error}",
                        runtime.model
                    );
                    emit_chat_adversarial_event(
                        run_id,
                        Some(thread_id),
                        &context,
                        &format!("{}_failed", context.event_prefix),
                        None,
                        None,
                        Some(message.clone()),
                        Some("failed".to_string()),
                        Some(&runtime.model),
                        true,
                        None,
                    )
                    .await;
                    return Err(AppError::Internal(message));
                }
                Err(_) => {
                    let message =
                        format!("model {} timed out after {}s", runtime.model, timeout_secs);
                    emit_chat_adversarial_event(
                        run_id,
                        Some(thread_id),
                        &context,
                        &format!("{}_failed", context.event_prefix),
                        None,
                        None,
                        Some(message.clone()),
                        Some("failed".to_string()),
                        Some(&runtime.model),
                        true,
                        None,
                    )
                    .await;
                    return Err(AppError::Internal(message));
                }
            };
            let raw_answer = extract_response_text(&response);
            let (answer, consensus_vote, evidence_request) = if context.phase == "review" {
                let (answer, vote) = parse_review_answer(&raw_answer);
                (answer, vote, None)
            } else {
                let (answer, request) = parse_initial_answer(&raw_answer);
                (answer, None, request)
            };
            record_adversarial_usage(state, tenant_id, user_id, run_id, runtime, &response.usage)
                .await;
            let completed_event = format!("{}_completed", context.event_prefix);
            emit_chat_adversarial_event(
                run_id,
                Some(thread_id),
                &context,
                &completed_event,
                None,
                Some(answer.clone()),
                None,
                Some("completed".to_string()),
                Some(&runtime.model),
                true,
                Some(chat_adversarial_usage_json(&response.usage)),
            )
            .await;
            return Ok(ModelAnswer {
                model: runtime.model.clone(),
                answer,
                error: None,
                duration_ms: start.elapsed().as_millis(),
                consensus_vote,
                evidence_request,
            });
        }
    };

    let mut answer = String::new();
    let mut usage = AdversarialStreamUsage::default();
    let stream_deadline = tokio::time::sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(stream_deadline);

    loop {
        tokio::select! {
            _ = &mut stream_deadline => {
                let error = format!("model {} timed out after {}s", runtime.model, timeout_secs);
                let failed_event = format!("{}_failed", context.event_prefix);
                emit_chat_adversarial_event(
                    run_id,
                    Some(thread_id),
                    &context,
                    &failed_event,
                    None,
                    Some(answer.clone()),
                    Some(error.clone()),
                    Some("failed".to_string()),
                    Some(&runtime.model),
                    false,
                    None,
                )
                .await;
                return Err(AppError::Internal(error));
            }
            next = stream.next_event() => {
                let event = match next {
                    Ok(event) => event,
                    Err(error) => {
                        let message = format!("model {} stream failed: {error}", runtime.model);
                        let failed_event = format!("{}_failed", context.event_prefix);
                        emit_chat_adversarial_event(
                            run_id,
                            Some(thread_id),
                            &context,
                            &failed_event,
                            None,
                            Some(answer.clone()),
                            Some(message.clone()),
                            Some("failed".to_string()),
                            Some(&runtime.model),
                            true,
                            None,
                        )
                        .await;
                        return Err(AppError::Internal(message));
                    }
                };
                let Some(event) = event else {
                    break;
                };
                if super::agent_chat_adversarial_support::chat_adversarial_cancel_requested(
                    state, tenant_id, user_id, run_id,
                )
                .await?
                {
                    let cancelled_event = format!("{}_cancelled", context.event_prefix);
                    emit_chat_adversarial_event(
                        run_id,
                        Some(thread_id),
                        &context,
                        &cancelled_event,
                        None,
                        Some(answer.clone()),
                        None,
                        Some("cancelled".to_string()),
                        Some(&runtime.model),
                        false,
                        None,
                    )
                    .await;
                    return Err(AppError::ValidationError(
                        "chat adversarial run was cancelled".to_string(),
                    ));
                }

                match event {
                    api::StreamEvent::ContentBlockDelta(delta) => {
                        if let api::ContentBlockDelta::TextDelta { text } = delta.delta {
                            if text.is_empty() {
                                continue;
                            }
                            answer.push_str(&text);
                            let delta_event = format!("{}_delta", context.event_prefix);
                            emit_chat_adversarial_event(
                                run_id,
                                Some(thread_id),
                                &context,
                                &delta_event,
                                Some(text),
                                None,
                                None,
                                Some("running".to_string()),
                                Some(&runtime.model),
                                false,
                                None,
                            )
                            .await;
                        }
                    }
                    api::StreamEvent::MessageDelta(delta) => {
                        usage.input = delta.usage.input_tokens;
                        usage.output = delta.usage.output_tokens;
                        usage.cache_creation = delta.usage.cache_creation_input_tokens;
                        usage.cache_read = delta.usage.cache_read_input_tokens;
                    }
                    api::StreamEvent::MessageStop(_) => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    let (answer, consensus_vote, evidence_request) = if context.phase == "review" {
        let (answer, vote) = parse_review_answer(&answer);
        (answer, vote, None)
    } else {
        let (answer, request) = parse_initial_answer(&answer);
        (answer, None, request)
    };
    let usage = api::Usage {
        input_tokens: usage.input,
        output_tokens: usage.output,
        cache_creation_input_tokens: usage.cache_creation,
        cache_read_input_tokens: usage.cache_read,
    };
    record_adversarial_usage(state, tenant_id, user_id, run_id, runtime, &usage).await;
    let completed_event = format!("{}_completed", context.event_prefix);
    emit_chat_adversarial_event(
        run_id,
        Some(thread_id),
        &context,
        &completed_event,
        None,
        Some(answer.clone()),
        None,
        Some("completed".to_string()),
        Some(&runtime.model),
        false,
        Some(chat_adversarial_usage_json(&usage)),
    )
    .await;
    Ok(ModelAnswer {
        model: runtime.model.clone(),
        answer,
        error: None,
        duration_ms: start.elapsed().as_millis(),
        consensus_vote,
        evidence_request,
    })
}

async fn record_adversarial_usage(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    runtime: &AdversarialModelRuntime,
    usage: &api::Usage,
) {
    let _ = state
        .config_registry()
        .record_token_usage(agent_gateway::TokenUsageParams {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            session_id: Some(run_id.to_string()),
            model: runtime.model.clone(),
            input_tokens: i64::from(usage.input_tokens),
            output_tokens: i64::from(usage.output_tokens),
            cache_creation_tokens: i64::from(usage.cache_creation_input_tokens),
            cache_read_tokens: i64::from(usage.cache_read_input_tokens),
            api_key_id: Some(runtime.entry.id.clone()),
            provider: runtime.entry.provider.clone(),
            custom_input_price: runtime.entry.input_price_per_million,
            custom_output_price: runtime.entry.output_price_per_million,
        })
        .await;
}

fn extract_response_text(response: &api::MessageResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| {
            if let api::OutputContentBlock::Text { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

async fn judge_round_resolution(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    thread_id: &str,
    question: &str,
    parent_context: Option<&str>,
    parent_debate_state: Option<&serde_json::Value>,
    memory_context: Option<&AdversarialMemoryContext>,
    evidence_prompt: &str,
    judge: &AdversarialModelRuntime,
    answers: &[ModelAnswer],
    debate_memory: &AdversarialDebateMemory,
    participant_consensus: &ParticipantConsensus,
    round: u32,
) -> Result<JudgeDecision, AppError> {
    let system = build_judge_system_prompt();
    let followup_context = format_followup_context(parent_context);
    let debate_state_prompt =
        format_adversarial_debate_state_for_model(parent_debate_state, "judge");
    let memory_prompt = format_adversarial_memory_prompt(memory_context);
    let history_context = debate_memory.format_history_summary(None, 4800);
    let participant_consensus = participant_consensus_to_json(participant_consensus);
    let entry_contract = if round == 1 {
        "这是彼此不可见的独立首轮。先判断所有健康参赛模型的核心结论是否实质一致，而不是只比较措辞。只要关键建议、事实、因果、边界或风险存在实质分歧，resolved 必须为 false，并在 critical_conflicts 中写出带模型名的具体冲突，供下一轮互相反驳。只有全部配置模型均成功、核心结论实质一致且逐项 claim 审计通过时，才可 resolved=true。"
    } else {
        "这是参与模型互相看到并逐项反驳后的收敛审查。服务端已验证所有健康参与者提供了有具体理由的一致认可票且无重大未决异议；仍须独立审计事实，不能把投票当作正确性证据。"
    };
    let prompt = format!(
        "{followup_context}{debate_state_prompt}{memory_prompt}{evidence_prompt}\n\n---\n\n用户问题：\n{question}\n\n历史多轮观点轨迹摘要：\n{history_context}\n\n第 {round} 轮各模型答案：\n{}\n\n服务端校验的参与模型一致认可状态：\n{participant_consensus}\n\n{entry_contract}\n\n请逐项抽取并审计关键 claim，尤其检查数字、日期、因果、否定关系、适用边界和证据是否冲突。只有审计完成且 critical_conflicts 为空，同时不存在明显事实冲突、逻辑缺口或未披露的关键不确定性时 resolved 才能为 true。resolved=true 时 winner_model 必须是本轮真实参赛模型之一，选择答案最准确、完整、清晰的一方；不能返回角色名、占位名或 null。多模型一致不等于事实正确，证据不足必须在 winner_reason 中明确降低置信。返回 JSON：{{\"resolved\": boolean, \"claim_audit_complete\": true, \"critical_conflicts\": [\"仍未解决的关键 claim 冲突\"], \"winner_model\": string|null, \"winner_reason\": string}}",
        format_peer_answers(answers)
    );
    let context = ChatAdversarialCallContext {
        round: Some(round),
        phase: "judge".to_string(),
        message_id: chat_adversarial_message_id(run_id, "judge", Some(round), Some(&judge.model)),
        event_prefix: "judge".to_string(),
    };
    let answer = call_adversarial_model(
        state,
        tenant_id,
        user_id,
        run_id,
        thread_id,
        judge,
        &system,
        &prompt,
        CHAT_ADV_JUDGE_TIMEOUT_SECS,
        context,
    )
    .await?;
    Ok(parse_judge_decision(&answer.answer))
}

async fn finalize_chat_adversarial_answer(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    thread_id: &str,
    question: &str,
    parent_context: Option<&str>,
    parent_debate_state: Option<&serde_json::Value>,
    memory_context: Option<&AdversarialMemoryContext>,
    evidence_prompt: &str,
    judge: &AdversarialModelRuntime,
    answers: &[ModelAnswer],
    debate_memory: &AdversarialDebateMemory,
    decision: &JudgeDecision,
) -> Result<JudgeDecision, AppError> {
    let system = build_final_system_prompt();
    let followup_context = format_followup_context(parent_context);
    let debate_state_prompt =
        format_adversarial_debate_state_for_model(parent_debate_state, "judge");
    let memory_prompt = format_adversarial_memory_prompt(memory_context);
    let history_context = debate_memory.format_history_summary(None, 6400);
    let prompt = format!(
        "{followup_context}{debate_state_prompt}{memory_prompt}{evidence_prompt}\n\n---\n\n用户问题：\n{question}\n\n历史多轮观点轨迹摘要：\n{history_context}\n\n最终一轮各模型答案：\n{}\n\n上一轮裁判判断：\nresolved={}\nwinner_model={}\nwinner_reason={}\n\n请整理最终答案。必须吸收历史多轮中已被反复验证的强观点，丢弃被后续反驳的弱观点；事实性结论必须说明证据支持程度，避免“多模型一致所以正确”的表达。返回 JSON：{{\"winner_model\": string|null, \"winner_reason\": string, \"final_answer\": string}}",
        format_peer_answers(answers),
        decision.resolved,
        decision.winner_model.as_deref().unwrap_or("null"),
        decision.winner_reason.as_deref().unwrap_or("")
    );
    let context = ChatAdversarialCallContext {
        round: None,
        phase: "final".to_string(),
        message_id: chat_adversarial_message_id(run_id, "final", None, Some(&judge.model)),
        event_prefix: "final".to_string(),
    };
    let answer = call_adversarial_model(
        state,
        tenant_id,
        user_id,
        run_id,
        thread_id,
        judge,
        &system,
        &prompt,
        CHAT_ADV_JUDGE_TIMEOUT_SECS,
        context,
    )
    .await?;
    let mut final_decision = parse_final_decision(&answer.answer);
    if final_decision.raw.trim().is_empty() {
        final_decision.raw = answer.answer;
    }
    Ok(final_decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adversarial_models_are_distinct_case_insensitively() {
        let models = distinct_chat_adversarial_models([
            Some(" deepseek-v4-pro "),
            Some("DEEPSEEK-V4-PRO"),
            Some("gpt-5.2"),
            None,
            Some(""),
        ]);

        assert_eq!(models, vec!["deepseek-v4-pro", "gpt-5.2"]);
    }

    #[test]
    fn duplicate_keys_for_one_model_do_not_satisfy_adversarial_minimum() {
        let error = normalize_selected_models(vec![
            "deepseek-v4-pro".to_string(),
            "DEEPSEEK-V4-PRO".to_string(),
        ])
        .expect_err("one distinct model must be rejected");

        assert!(matches!(
            error,
            AppError::ValidationError(message)
                if message == CHAT_ADVERSARIAL_NEEDS_MODELS_ERROR
        ));
    }

    #[test]
    fn two_distinct_models_satisfy_adversarial_minimum() {
        let models =
            normalize_selected_models(vec!["deepseek-v4-pro".to_string(), "gpt-5.2".to_string()])
                .expect("two distinct models should be accepted");

        assert_eq!(models, vec!["deepseek-v4-pro", "gpt-5.2"]);
    }

    #[test]
    fn adversarial_defaults_are_bounded_for_task_center_runs() {
        assert_eq!(CHAT_ADV_DEFAULT_MAX_ROUNDS, 3);
        assert!(CHAT_ADV_HARD_MAX_ROUNDS <= 8);
        assert!(CHAT_ADV_JOB_TIMEOUT_SECS <= 15 * 60);
    }

    #[test]
    fn each_model_selects_its_own_evidence_query_without_majority_gating() {
        let answer_a = ModelAnswer {
            model: "model-a".to_string(),
            answer: "answer a".to_string(),
            error: None,
            duration_ms: 1,
            consensus_vote: None,
            evidence_request: Some(
                super::super::agent_chat_adversarial_domain::EvidenceRequest {
                    needed: true,
                    queries: vec!["official source for A".to_string()],
                    reason: None,
                },
            ),
        };
        let answer_b = ModelAnswer {
            model: "model-b".to_string(),
            answer: "answer b".to_string(),
            error: None,
            duration_ms: 1,
            consensus_vote: None,
            evidence_request: None,
        };

        assert_eq!(
            select_model_evidence_query(1, &answer_a, false, None, "fallback").as_deref(),
            Some("official source for A")
        );
        assert!(select_model_evidence_query(1, &answer_b, false, None, "fallback").is_none());
    }

    #[test]
    fn evidence_trace_keeps_participant_ledgers_separate() {
        let models = vec!["model-a".to_string(), "model-b".to_string()];
        let mut evidence_by_model = HashMap::new();
        evidence_by_model.insert(
            "model-a".to_string(),
            AdversarialEvidenceContext {
                attempted: true,
                available: true,
                query: Some("query-a".to_string()),
                ..AdversarialEvidenceContext::default()
            },
        );
        evidence_by_model.insert(
            "model-b".to_string(),
            AdversarialEvidenceContext {
                attempted: true,
                available: false,
                query: Some("query-b".to_string()),
                degraded_reason: Some("unavailable".to_string()),
                ..AdversarialEvidenceContext::default()
            },
        );

        let trace = adversarial_evidence_map_to_trace(&models, &evidence_by_model);
        assert_eq!(trace["mode"], "isolated_per_model");
        assert_eq!(
            trace["byModel"]["model-a"]["adversarialView"]["query"],
            "query-a"
        );
        assert_eq!(
            trace["byModel"]["model-b"]["adversarialView"]["query"],
            "query-b"
        );
    }

    #[test]
    fn unanimous_participants_and_judge_allow_early_exit_before_max_rounds() {
        let models = vec!["model-a".to_string(), "model-b".to_string()];
        let consensus = ParticipantConsensus {
            reached: true,
            accepted_models: models.clone(),
            ..ParticipantConsensus::default()
        };
        let decision = JudgeDecision {
            resolved: true,
            claim_audit_complete: true,
            critical_conflicts: Vec::new(),
            winner_model: Some("model-a".to_string()),
            winner_reason: Some("all models agreed".to_string()),
            raw: String::new(),
        };
        assert!(chat_adversarial_should_stop_after_round(
            2, 5, &consensus, &decision, &models,
        ));
    }

    #[test]
    fn independent_first_round_can_end_only_after_a_complete_judge_audit() {
        let models = vec!["model-a".to_string(), "model-b".to_string()];
        let no_participant_vote_in_independent_round = ParticipantConsensus::default();
        let aligned = JudgeDecision {
            resolved: true,
            claim_audit_complete: true,
            critical_conflicts: Vec::new(),
            winner_model: Some("model-b".to_string()),
            winner_reason: Some("independent answers are materially aligned".to_string()),
            raw: String::new(),
        };
        assert!(chat_adversarial_should_stop_after_round(
            1,
            5,
            &no_participant_vote_in_independent_round,
            &aligned,
            &models,
        ));

        let incomplete_audit = JudgeDecision {
            claim_audit_complete: false,
            ..aligned
        };
        assert!(!chat_adversarial_should_stop_after_round(
            1,
            5,
            &no_participant_vote_in_independent_round,
            &incomplete_audit,
            &models,
        ));
    }

    #[test]
    fn debate_continues_without_unanimity_or_a_valid_winner_until_max_rounds() {
        let models = vec!["model-a".to_string(), "model-b".to_string()];
        let no_consensus = ParticipantConsensus::default();
        let decision = JudgeDecision {
            resolved: false,
            claim_audit_complete: false,
            critical_conflicts: vec!["unresolved".to_string()],
            winner_model: None,
            winner_reason: None,
            raw: String::new(),
        };
        assert!(!chat_adversarial_should_stop_after_round(
            2,
            5,
            &no_consensus,
            &decision,
            &models,
        ));
        assert!(chat_adversarial_should_stop_after_round(
            5,
            5,
            &no_consensus,
            &decision,
            &models,
        ));

        let consensus = ParticipantConsensus {
            reached: true,
            accepted_models: models.clone(),
            ..ParticipantConsensus::default()
        };
        let invalid_winner = JudgeDecision {
            resolved: true,
            claim_audit_complete: true,
            critical_conflicts: Vec::new(),
            winner_model: Some("judge".to_string()),
            winner_reason: None,
            raw: String::new(),
        };
        assert!(!chat_adversarial_should_stop_after_round(
            2,
            5,
            &consensus,
            &invalid_winner,
            &models,
        ));
    }

    #[test]
    fn debate_state_preserves_model_stances_for_followup() {
        let trace = serde_json::json!({
            "rounds": [
                {
                    "round": 1,
                    "answers": [
                        {
                            "model": "A",
                            "answer": "结论：应该做。\n- 强观点：A 认为需求确定。\n- 风险：需要验证成本。"
                        },
                        {
                            "model": "B",
                            "answer": "结论：暂缓。\n- 强观点：B 认为证据不足。\n- 修正：如果有预算可做小实验。"
                        }
                    ]
                }
            ],
            "evidence": {
                "adversarialView": {
                    "available": true,
                    "items": [
                        {
                            "title": "Benchmark",
                            "url": "https://example.com/a",
                            "domain": "example.com",
                            "snippet": "case evidence",
                            "sourceType": "native_model_search",
                            "sourceName": "model"
                        }
                    ]
                }
            }
        });
        let final_result = JudgeDecision {
            resolved: true,
            claim_audit_complete: true,
            critical_conflicts: Vec::new(),
            winner_model: Some("B".to_string()),
            winner_reason: Some("B 的证据约束更强".to_string()),
            raw: "最终结论：先小实验，不直接全量。".to_string(),
        };

        let state = build_adversarial_debate_state("是否上线？", &final_result, &trace);
        let prompt_for_a = format_adversarial_debate_state_for_model(Some(&state), "A");

        assert!(prompt_for_a.contains("你在上一轮的历史立场"));
        assert!(prompt_for_a.contains("A 认为需求确定"));
        assert!(prompt_for_a.contains("其他模型上一轮关键立场"));
        assert!(prompt_for_a.contains("B 认为证据不足"));
        assert!(prompt_for_a.contains("example.com"));
    }

    #[test]
    fn debate_state_formatter_is_empty_without_state() {
        assert!(format_adversarial_debate_state_for_model(None, "A").is_empty());
    }
}
