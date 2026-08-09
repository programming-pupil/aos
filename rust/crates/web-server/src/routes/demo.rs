//! Open-source demo scenario launcher.
//!
//! The demo layer intentionally stays thin: it makes the four public "wow"
//! scenarios discoverable and observable, then routes users into the real AOS
//! capability surfaces. Capability execution remains owned by RD, PM,
//! WatchDog, Runtime, Queue, and Trace modules.

use axum::{
    extract::{Extension, Path, State},
    routing::{get as routing_get, post as routing_post},
    Json, Router,
};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::agent_ops::{
    self, CreateAgentTaskInput, PHASE_EXECUTING, PHASE_FINALIZING, PHASE_PLANNING,
    STATUS_COMPLETED, STATUS_RUNNING,
};
use crate::state::AppState;

const DEMO_SCENARIOS: &[DemoScenarioSeed] = &[
    DemoScenarioSeed {
        id: "fix-frontend-bug",
        title: "Fix a frontend bug",
        title_zh: "修复前端 Bug",
        summary: "Open the sample Code Studio flow, inspect files, generate a candidate diff, run tests, and review before applying.",
        summary_zh: "打开代码开发样例，读取文件、生成 candidate Diff、运行测试，并在人工审查后应用。",
        capability_key: "rd_agent",
        entry_path: "/agent?mode=code&demo=fix-frontend-bug",
        cta: "Open Code Studio",
        cta_zh: "打开代码开发",
        prompt: "Fix the demo Vite/React console error. Inspect real files first, make the change only in the candidate workspace, run tests if available, and leave the main repository untouched until I apply the diff.",
        prompt_zh: "修复 demo Vite/React 的 console error。请先读取真实文件，只修改 candidate workspace，有测试就运行测试，主仓库必须等我审查 Diff 后再应用。",
        assets: &["examples/code-studio/frontend-bug-demo"],
        setup_steps: &[
            "Open Code Repos and register examples/code-studio/frontend-bug-demo as a local repository",
            "Sync the repository in Code Studio",
            "Start Preview with npm run dev",
            "Ask the prefilled prompt and review the candidate Diff before applying",
        ],
        expected: &[
            "Code Studio opens with the demo prompt prefilled",
            "Preview can be started with npm run dev",
            "Console error points to zero-cost ROI handling",
            "Agent creates candidate Diff and test output before apply",
        ],
    },
    DemoScenarioSeed {
        id: "ask-watchdog",
        title: "Ask WatchDog",
        title_zh: "询问看门狗",
        summary: "Inspect running, stale, failed, and cancelling AgentOps tasks with details, logs, cancel, and retry actions.",
        summary_zh: "查看 running/stale/failed/cancelling 任务，支持详情、日志、取消和重试动作。",
        capability_key: "watchdog",
        entry_path: "/watchdog?demo=ask-watchdog",
        cta: "Open WatchDog",
        cta_zh: "打开看门狗",
        prompt: "当前有哪些 Agent 在运行？哪个任务卡住了？为什么 Bot 没回复？",
        prompt_zh: "当前有哪些 Agent 在运行？哪个任务卡住了？为什么 Bot 没回复？",
        assets: &[
            "AgentOps demo tasks seeded by this scenario",
            "examples/bot-router/aos_router_agent.json",
            "examples/bot-router/generic_webhook_channel.json",
            "examples/bot-router/smoke_messages.jsonl",
        ],
        setup_steps: &[
            "Click this card to seed running/stale/failed demo AgentOps tasks",
            "Open WatchDog",
            "Ask: 当前有哪些 Agent 在运行？",
            "Inspect task details, events, and trace",
            "Optional: use examples/bot-router to verify the unified Bot Router entrance",
        ],
        expected: &[
            "WatchDog sees running/stale/failed demo tasks",
            "Task inspector shows structured events and trace",
            "Ask WatchDog answers from structured evidence",
            "WatchDog action commands such as cancel/detail/retry stay WatchDog-routed",
        ],
    },
];

#[derive(Clone, Copy)]
struct DemoScenarioSeed {
    id: &'static str,
    title: &'static str,
    title_zh: &'static str,
    summary: &'static str,
    summary_zh: &'static str,
    capability_key: &'static str,
    entry_path: &'static str,
    cta: &'static str,
    cta_zh: &'static str,
    prompt: &'static str,
    prompt_zh: &'static str,
    assets: &'static [&'static str],
    setup_steps: &'static [&'static str],
    expected: &'static [&'static str],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoScenario {
    pub id: String,
    pub title: String,
    pub title_zh: String,
    pub summary: String,
    pub summary_zh: String,
    pub capability_key: String,
    pub entry_path: String,
    pub cta: String,
    pub cta_zh: String,
    pub prompt: String,
    pub prompt_zh: String,
    pub assets: Vec<String>,
    pub setup_steps: Vec<String>,
    pub expected: Vec<String>,
    pub status: String,
    pub feature_ready: bool,
    pub missing_feature: Option<String>,
    pub last_run: Option<DemoRunSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoRunSummary {
    pub run_id: String,
    pub agent_task_id: String,
    pub scenario_id: String,
    pub status: String,
    pub entry_path: String,
    pub capability_key: String,
    pub message: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoScenarioListResponse {
    pub items: Vec<DemoScenario>,
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/scenarios", routing_get(list_scenarios))
        .route("/scenarios/{id}/run", routing_post(run_scenario))
        .route("/scenarios/{id}/status", routing_get(scenario_status))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

async fn list_scenarios(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<DemoScenarioListResponse>> {
    let mut items = Vec::with_capacity(DEMO_SCENARIOS.len());
    for seed in DEMO_SCENARIOS {
        items.push(seed.to_response(&state, &claims).await?);
    }
    Ok(Json(DemoScenarioListResponse { items }))
}

async fn run_scenario(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<DemoRunSummary>> {
    let seed = find_seed(&id)?;
    let readiness = seed.readiness();
    let run_id = format!("demo-{}", uuid::Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    let task_id = agent_ops::create_task(
        &state,
        CreateAgentTaskInput {
            tenant_id: claims.tenant_id.clone(),
            source: "demo".to_string(),
            source_ref: Some(run_id.clone()),
            source_label: Some("Open Source Wow Demo".to_string()),
            capability_key: seed.capability_key.to_string(),
            agent_id: None,
            agent_name: Some("AOS Demo Launcher".to_string()),
            title: format!("Demo: {}", seed.title),
            summary: Some(seed.summary.to_string()),
            owner_user_id: Some(claims.sub.clone()),
            correlation_id: Some(run_id.clone()),
            parent_task_id: None,
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
            external_message_id: None,
            idempotency_key: None,
            input_json: Some(seed.input_json(&run_id, readiness.feature_ready)),
        },
    )
    .await?;

    agent_ops::link_task_resource(
        &state,
        &claims.tenant_id,
        &task_id,
        "demo_scenario",
        seed.id,
    )
    .await?;

    agent_ops::mark_task_running(
        &state,
        &claims.tenant_id,
        &task_id,
        PHASE_PLANNING,
        "Demo scenario prepared. Open the linked workspace to run the real capability.",
        35,
    )
    .await?;

    agent_ops::add_event(
        &state,
        &claims.tenant_id,
        &task_id,
        "demo.scenario.started",
        Some(PHASE_EXECUTING),
        Some(STATUS_RUNNING),
        "info",
        "Demo scenario route and prompt are ready",
        Some(seed.input_json(&run_id, readiness.feature_ready)),
    )
    .await?;

    if seed.id == "ask-watchdog" {
        seed_watchdog_demo_tasks(&state, &claims, &run_id).await?;
    }

    let message = if readiness.feature_ready {
        "Demo launcher is ready. Continue in the linked workspace."
    } else {
        "Demo launcher is ready, but this build is missing an optional feature."
    };
    agent_ops::complete_task(
        &state,
        &claims.tenant_id,
        &task_id,
        message,
        Some(json!({
            "runId": run_id,
            "scenarioId": seed.id,
            "entryPath": seed.entry_path,
            "prompt": seed.prompt,
            "promptZh": seed.prompt_zh,
            "assets": seed.assets,
            "setupSteps": seed.setup_steps,
            "expected": seed.expected,
            "featureReady": readiness.feature_ready,
            "missingFeature": readiness.missing_feature,
            "nextStep": seed.cta,
        })),
    )
    .await?;

    Ok(Json(DemoRunSummary {
        run_id,
        agent_task_id: task_id,
        scenario_id: seed.id.to_string(),
        status: STATUS_COMPLETED.to_string(),
        entry_path: seed.entry_path.to_string(),
        capability_key: seed.capability_key.to_string(),
        message: message.to_string(),
        created_at: now,
        completed_at: Some(Utc::now().to_rfc3339()),
    }))
}

async fn scenario_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<DemoScenario>> {
    let seed = find_seed(&id)?;
    Ok(Json(seed.to_response(&state, &claims).await?))
}

impl DemoScenarioSeed {
    async fn to_response(&self, state: &AppState, claims: &Claims) -> Result<DemoScenario> {
        let readiness = self.readiness();
        let last_run = latest_demo_run(state, &claims.tenant_id, self.id).await?;
        Ok(DemoScenario {
            id: self.id.to_string(),
            title: self.title.to_string(),
            title_zh: self.title_zh.to_string(),
            summary: self.summary.to_string(),
            summary_zh: self.summary_zh.to_string(),
            capability_key: self.capability_key.to_string(),
            entry_path: self.entry_path.to_string(),
            cta: self.cta.to_string(),
            cta_zh: self.cta_zh.to_string(),
            prompt: self.prompt.to_string(),
            prompt_zh: self.prompt_zh.to_string(),
            assets: self.assets.iter().map(ToString::to_string).collect(),
            setup_steps: self.setup_steps.iter().map(ToString::to_string).collect(),
            expected: self.expected.iter().map(ToString::to_string).collect(),
            status: if readiness.feature_ready {
                "ready".to_string()
            } else {
                "degraded".to_string()
            },
            feature_ready: readiness.feature_ready,
            missing_feature: readiness.missing_feature.map(str::to_string),
            last_run,
        })
    }

    fn readiness(&self) -> DemoReadiness {
        match self.id {
            "fix-frontend-bug" if !cfg!(feature = "rd") => DemoReadiness {
                feature_ready: false,
                missing_feature: Some("rd"),
            },
            "diagnose-roi-drop" | "daily-revenue-report" if !cfg!(feature = "nl2sql") => {
                DemoReadiness {
                    feature_ready: true,
                    missing_feature: Some("nl2sql optional; demo evidence fallback is available"),
                }
            }
            _ => DemoReadiness {
                feature_ready: true,
                missing_feature: None,
            },
        }
    }

    fn input_json(&self, run_id: &str, feature_ready: bool) -> Value {
        json!({
            "runId": run_id,
            "scenarioId": self.id,
            "title": self.title,
            "titleZh": self.title_zh,
            "capabilityKey": self.capability_key,
            "entryPath": self.entry_path,
            "prompt": self.prompt,
            "promptZh": self.prompt_zh,
            "assets": self.assets,
            "setupSteps": self.setup_steps,
            "expected": self.expected,
            "featureReady": feature_ready,
            "openSourceDemo": true,
            "traceExpectation": [
                "AgentOps task",
                "structured events",
                "real capability workspace",
                "WatchDog visible status"
            ],
        })
    }
}

#[derive(Clone, Copy)]
struct DemoReadiness {
    feature_ready: bool,
    missing_feature: Option<&'static str>,
}

fn find_seed(id: &str) -> Result<&'static DemoScenarioSeed> {
    DEMO_SCENARIOS
        .iter()
        .find(|seed| seed.id == id)
        .ok_or_else(|| AppError::NotFound(format!("demo scenario '{id}' not found")))
}

async fn latest_demo_run(
    state: &AppState,
    tenant_id: &str,
    scenario_id: &str,
) -> Result<Option<DemoRunSummary>> {
    let row = sqlx::query(
        r"
        SELECT id, source_ref, capability_key, status, last_event,
               CAST(input_json AS TEXT) AS input_json,
               CAST(created_at AS TEXT) AS created_at,
               CAST(completed_at AS TEXT) AS completed_at
        FROM agent_tasks
        WHERE tenant_id = ?
          AND source = 'demo'
          AND linked_resource_type = 'demo_scenario'
          AND linked_resource_id = ?
        ORDER BY created_at DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(scenario_id)
    .fetch_optional(&state.db)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    let task_id: String = row.get("id");
    let run_id: Option<String> = row.get("source_ref");
    let capability_key: String = row.get("capability_key");
    let status: String = row.get("status");
    let message: Option<String> = row.get("last_event");
    let input_json: Option<String> = row.get("input_json");
    let entry_path = input_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| {
            value
                .get("entryPath")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| {
            find_seed(scenario_id)
                .map(|seed| seed.entry_path.to_string())
                .unwrap_or_else(|_| "/dashboard".to_string())
        });
    let created_at: Option<String> = row.get("created_at");
    let completed_at: Option<String> = row.get("completed_at");

    Ok(Some(DemoRunSummary {
        run_id: run_id.unwrap_or_else(|| task_id.clone()),
        agent_task_id: task_id,
        scenario_id: scenario_id.to_string(),
        status,
        entry_path,
        capability_key,
        message: message.unwrap_or_else(|| "Demo scenario prepared".to_string()),
        created_at: created_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
        completed_at,
    }))
}

async fn seed_watchdog_demo_tasks(state: &AppState, claims: &Claims, run_id: &str) -> Result<()> {
    let seeds = [(
        "rd_agent",
        "Demo stale RD Agent",
        "stale",
        "executing",
        "runtime heartbeat 超过阈值，疑似卡在测试命令",
        62,
    )];

    for (capability_key, title, status, phase, event, progress) in seeds {
        let task_id = agent_ops::create_task(
            state,
            CreateAgentTaskInput {
                tenant_id: claims.tenant_id.clone(),
                source: "demo".to_string(),
                source_ref: Some(run_id.to_string()),
                source_label: Some("WatchDog demo evidence".to_string()),
                capability_key: capability_key.to_string(),
                agent_id: None,
                agent_name: Some("AOS Demo Agent".to_string()),
                title: title.to_string(),
                summary: Some(
                    "WatchDog demo task for open-source first-run experience".to_string(),
                ),
                owner_user_id: Some(claims.sub.clone()),
                correlation_id: Some(run_id.to_string()),
                parent_task_id: None,
                external_platform: Some("demo".to_string()),
                external_channel_id: Some("open-source".to_string()),
                external_conversation_id: Some("watchdog-demo".to_string()),
                external_message_id: None,
                idempotency_key: None,
                input_json: Some(json!({
                    "scenarioId": "ask-watchdog",
                    "demoEvidence": true,
                    "expectedQuestions": [
                        "当前有哪些 Agent 在运行？",
                        "哪个任务卡住了？",
                        "为什么没有回复？"
                    ],
                })),
            },
        )
        .await?;

        sqlx::query(
            r"
            UPDATE agent_tasks
            SET status = ?, phase = ?, progress_percent = ?, last_event = ?,
                queue_status = CASE
                    WHEN ? = 'failed' THEN 'dead'
                    WHEN ? = 'stale' THEN 'stale'
                    ELSE 'running'
                END,
                last_heartbeat_at = CASE
                    WHEN ? = 'stale' THEN datetime(CURRENT_TIMESTAMP, '-20 minutes')
                    ELSE CURRENT_TIMESTAMP
                END,
                started_at = COALESCE(started_at, CURRENT_TIMESTAMP),
                completed_at = CASE WHEN ? = 'failed' THEN CURRENT_TIMESTAMP ELSE completed_at END,
                finished_at = CASE WHEN ? = 'failed' THEN CURRENT_TIMESTAMP ELSE finished_at END,
                error_code = CASE WHEN ? = 'failed' THEN 'DEMO_DELIVERY_TARGET_MISSING' ELSE error_code END,
                error_message = CASE WHEN ? = 'failed' THEN ? ELSE error_message END,
                updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND id = ?
            ",
        )
        .bind(status)
        .bind(phase)
        .bind(progress)
        .bind(event)
        .bind(status)
        .bind(status)
        .bind(status)
        .bind(status)
        .bind(status)
        .bind(status)
        .bind(status)
        .bind(event)
        .bind(&claims.tenant_id)
        .bind(&task_id)
        .execute(&state.db)
        .await?;

        let severity = if status == "failed" { "error" } else { "warn" };
        agent_ops::add_event(
            state,
            &claims.tenant_id,
            &task_id,
            "demo.watchdog.evidence",
            Some(phase),
            Some(status),
            severity,
            event,
            Some(json!({
                "scenarioId": "ask-watchdog",
                "demoEvidence": true,
                "runId": run_id,
                "capabilityKey": capability_key,
            })),
        )
        .await?;
        if status == "failed" {
            agent_ops::add_event(
                state,
                &claims.tenant_id,
                &task_id,
                "failed",
                Some(PHASE_FINALIZING),
                Some("failed"),
                "error",
                event,
                Some(json!({ "errorCode": "DEMO_DELIVERY_TARGET_MISSING" })),
            )
            .await?;
        }
    }

    Ok(())
}
