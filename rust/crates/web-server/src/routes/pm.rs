//! Product/Marketing (PM) routes.
//!
//! Provides:
//! - PM copilot chat (scenario-scoped key resolution: `pm`)
//! - PM missions/runs/material jobs for V2 research pipeline

use axum::{
    extract::{Extension, Path, Query, State},
    response::IntoResponse,
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
    },
    Json, Router,
};
use base64::Engine;
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::FutureExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::multipart::{Form as MultipartForm, Part as MultipartPart};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{
    collections::HashSet,
    path::{Path as FsPath, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};
use tokio::process::Command;

use pm_domain::material::*;
use pm_domain::report_strategy::{
    detect_pm_report_strategy_signal, extract_pm_first_party_evidence,
};
use pm_domain::search_orchestrator::{pm_search_fallback_keys, PmSearchOrchestratorSnapshot};

use crate::auth::Claims;
use crate::error::AppError;
use crate::routes::hooks::{run_lifecycle_hooks, HookEventType};
use crate::routes::system_events;
use crate::state::AppState;

const PM_SEARCH_PROVIDER_TYPES: &[&str] = &[
    "brave",
    "tavily",
    "serper",
    "exa",
    "searxng",
    "generic_json",
    "internal_http",
];

const PM_SEARCH_PROVIDER_TEMPLATES: &[(&str, &str, &str, &str)] = &[
    (
        "brave",
        "Brave Search",
        "https://api.search.brave.com/res/v1/web/search",
        "GET",
    ),
    ("tavily", "Tavily", "https://api.tavily.com/search", "POST"),
    (
        "serper",
        "Serper",
        "https://google.serper.dev/search",
        "POST",
    ),
    ("exa", "Exa", "https://api.exa.ai/search", "POST"),
    ("searxng", "SearXNG", "", "GET"),
    ("generic_json", "Generic JSON", "", "POST"),
    ("internal_http", "Internal HTTP", "", "POST"),
];

const PUBLIC_MATERIAL_ASSET_TYPES_SQL: &str = "'text', 'image', 'music', 'ppt'";

fn pm_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn suno_poll_timeout_secs() -> u64 {
    pm_env_u64("PM_SUNO_POLL_TIMEOUT_SECS", 180).clamp(30, 1800)
}

fn suno_poll_interval_ms() -> u64 {
    pm_env_u64("PM_SUNO_POLL_INTERVAL_MS", 2_000).clamp(500, 30_000)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmChatRequest {
    pub model: Option<String>,
    #[serde(default)]
    pub messages: Vec<crate::routes::chat::ChatMessage>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmUsageDto {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
    pub model: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmRuleHitDto {
    pub rule_key: String,
    pub rule_name: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmChatResponse {
    pub answer: String,
    pub usage: Option<PmUsageDto>,
    pub applied_rules: Vec<PmRuleHitDto>,
}

#[derive(Debug, Serialize)]
pub struct PmListResponse<T> {
    pub items: Vec<T>,
    pub total: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionRecord {
    pub id: i64,
    pub mission_name: String,
    pub intent: String,
    pub country_code: String,
    pub schedule_cron: Option<String>,
    pub lookback_days: i32,
    pub max_sources: i32,
    pub max_signals_per_source: i32,
    pub auto_discovery: bool,
    pub enabled: bool,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionCreateRequest {
    pub mission_name: String,
    pub intent: String,
    pub country_code: Option<String>,
    pub schedule_cron: Option<String>,
    pub lookback_days: Option<i32>,
    pub max_sources: Option<i32>,
    pub max_signals_per_source: Option<i32>,
    pub auto_discovery: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionUpdateRequest {
    pub mission_name: Option<String>,
    pub intent: Option<String>,
    pub country_code: Option<String>,
    pub schedule_cron: Option<String>,
    pub lookback_days: Option<i32>,
    pub max_sources: Option<i32>,
    pub max_signals_per_source: Option<i32>,
    pub auto_discovery: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialJobRecord {
    pub id: i64,
    pub mission_run_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub parent_job_id: Option<i64>,
    pub iteration_no: i32,
    pub prompt_text: String,
    pub model: Option<String>,
    pub asset_type: String,
    pub status: String,
    pub result_count: i32,
    pub error_message: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialJobCreateRequest {
    pub mission_run_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub parent_job_id: Option<i64>,
    pub continue_from_asset_id: Option<i64>,
    pub prompt_text: String,
    pub model: Option<String>,
    pub asset_type: Option<String>,
    pub workflow_stage: Option<String>,
    pub workflow_payload: Option<Value>,
    #[serde(default)]
    pub reference_images: Vec<PmMaterialReferenceImageInput>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialReferenceImageInput {
    pub url: String,
    #[serde(default, alias = "media_type")]
    pub media_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "size")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialAssetRecord {
    pub id: i64,
    pub job_id: i64,
    pub asset_type: String,
    pub url: Option<String>,
    pub content_text: Option<String>,
    pub meta: Value,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionQuery {
    pub page: Option<u32>,
    #[serde(alias = "per_page")]
    pub per_page: Option<u32>,
    #[serde(alias = "country_code")]
    pub country_code: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialJobQuery {
    pub page: Option<u32>,
    #[serde(alias = "per_page")]
    pub per_page: Option<u32>,
    #[serde(alias = "mission_run_id")]
    pub mission_run_id: Option<i64>,
    #[serde(alias = "thread_id")]
    pub thread_id: Option<i64>,
    #[serde(alias = "asset_type")]
    pub asset_type: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialThreadRecord {
    pub thread_id: i64,
    pub latest_job_id: i64,
    pub mission_run_id: Option<i64>,
    pub version_count: i64,
    pub latest_iteration_no: i32,
    pub prompt_text: String,
    pub model: Option<String>,
    pub asset_type: String,
    pub status: String,
    pub result_count: i32,
    pub error_message: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialModelQuery {
    #[serde(alias = "asset_type")]
    pub asset_type: Option<String>,
    #[serde(alias = "workflow_stage")]
    pub workflow_stage: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialModelOption {
    pub model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialModelListResponse {
    pub items: Vec<PmMaterialModelOption>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderListResponse {
    pub items: Vec<PmSearchProviderRecord>,
    pub total: i64,
    pub templates: Vec<PmSearchProviderTemplate>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderTemplate {
    pub provider_type: &'static str,
    pub label: &'static str,
    pub default_base_url: &'static str,
    pub default_method: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderRecord {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub enabled: bool,
    pub priority: i32,
    pub base_url: Option<String>,
    pub method: String,
    pub auth_type: String,
    pub auth_secret_ref: Option<String>,
    pub has_secret: bool,
    pub key_hint: Option<String>,
    pub headers_json: Option<Value>,
    pub query_template_json: Option<Value>,
    pub response_mapping_json: Option<Value>,
    pub timeout_secs: i32,
    pub max_results: i32,
    pub fetch_content_enabled: bool,
    pub content_extract_mode: String,
    pub domain_allowlist_json: Option<Value>,
    pub domain_blocklist_json: Option<Value>,
    pub rate_limit_json: Option<Value>,
    pub health_status: String,
    pub last_error: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderUpsertRequest {
    pub name: Option<String>,
    pub provider_type: Option<String>,
    pub enabled: Option<bool>,
    pub priority: Option<i32>,
    pub base_url: Option<String>,
    pub method: Option<String>,
    pub auth_type: Option<String>,
    pub auth_secret: Option<String>,
    pub auth_secret_ref: Option<String>,
    pub headers_json: Option<Value>,
    pub query_template_json: Option<Value>,
    pub response_mapping_json: Option<Value>,
    pub timeout_secs: Option<i32>,
    pub max_results: Option<i32>,
    pub fetch_content_enabled: Option<bool>,
    pub content_extract_mode: Option<String>,
    pub domain_allowlist_json: Option<Value>,
    pub domain_blocklist_json: Option<Value>,
    pub rate_limit_json: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderReorderRequest {
    pub provider_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderTestResponse {
    pub ok: bool,
    pub latency_ms: u64,
    pub result_count: usize,
    pub error: Option<String>,
    pub provider_trace: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchDoctorResponse {
    pub orchestrator: PmSearchOrchestratorSnapshot,
    pub builtin_web_search: PmSearchLayerStatus,
    pub native_search: PmSearchLayerStatus,
    pub mcp_search: PmSearchLayerStatus,
    pub configured_providers: Vec<PmSearchProviderHealth>,
    pub rag_local: PmSearchLayerStatus,
    pub effective_order: Vec<String>,
    pub degraded_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchLayerStatus {
    pub available: bool,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderHealth {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub enabled: bool,
    pub priority: i32,
    pub health_status: String,
    pub has_secret: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchQueryRequest {
    pub query: String,
    pub provider_id: Option<String>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchQueryResponse {
    pub ok: bool,
    pub output: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmReportTextRequest {
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmReportExtractResponse {
    pub mode: String,
    pub matched: bool,
    pub score: usize,
    pub reasons: Vec<String>,
    pub primary_terms: Vec<String>,
    pub first_party_evidence: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmReportSearchPlanResponse {
    pub mode: String,
    pub matched: bool,
    pub targeted_queries: Vec<String>,
    pub fallback_order: Vec<&'static str>,
    pub first_party_is_primary: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmQualityCheckRequest {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmQualityCheckResponse {
    pub passed: bool,
    pub matched: bool,
    pub missing_checks: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmResearchTraceResponse {
    pub run_id: String,
    pub stages: Vec<Value>,
    pub tool_calls: Vec<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmResearchEvidenceResponse {
    pub run_id: String,
    pub evidence: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionTaskRunQuery {
    pub page: Option<u32>,
    #[serde(alias = "per_page")]
    pub per_page: Option<u32>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionTaskEventQuery {
    pub page: Option<u32>,
    #[serde(alias = "per_page")]
    pub per_page: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionTaskRunRecord {
    pub task_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub attempt: Option<i32>,
    pub elapsed_ms: i64,
    pub stage_elapsed_ms: Option<i64>,
    pub error_message: Option<String>,
    pub detail: Option<Value>,
    pub response: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionTaskEventRecord {
    pub seq: i64,
    pub status: String,
    pub stage: Option<String>,
    pub attempt: Option<i32>,
    pub message: Option<String>,
    pub elapsed_ms: i64,
    pub stage_elapsed_ms: Option<i64>,
    pub detail: Option<Value>,
    pub response: Option<Value>,
    pub error_message: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionRunNowResponse {
    pub mission_id: i64,
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMissionSummaryResponse {
    pub total_missions: i64,
    pub enabled_missions: i64,
    pub disabled_missions: i64,
    pub queued_runs: i64,
    pub running_runs: i64,
    pub cancelling_runs: i64,
    pub completed_runs_30d: i64,
    pub failed_runs_30d: i64,
    pub cancelled_runs_30d: i64,
    pub success_rate_30d: f64,
    pub avg_elapsed_ms_30d: Option<i64>,
    pub latest_run_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialSummaryResponse {
    pub total_jobs: i64,
    pub total_threads: i64,
    pub running_jobs: i64,
    pub completed_jobs_30d: i64,
    pub failed_jobs_30d: i64,
    pub success_rate_30d: f64,
    pub text_jobs_30d: i64,
    pub image_jobs_30d: i64,
    pub music_jobs_30d: i64,
    pub ppt_jobs_30d: i64,
    pub asset_count_30d: i64,
    pub versioned_jobs: i64,
    pub latest_job_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialAssetExportQuery {
    pub format: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmMaterialAssetExportResponse {
    pub asset_id: i64,
    pub format: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmCronPreviewQuery {
    #[serde(alias = "schedule_cron")]
    pub schedule_cron: String,
    pub count: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmCronPreviewResponse {
    pub schedule_cron: String,
    pub normalized_cron: String,
    pub next_runs: Vec<String>,
}

fn parse_json_str(raw: Option<String>) -> Option<Value> {
    raw.as_ref().and_then(|s| serde_json::from_str(s).ok())
}

fn parse_dt_opt(raw: Option<String>) -> Option<String> {
    raw.and_then(|s| {
        NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|dt| dt.and_utc().to_rfc3339())
    })
}

fn is_public_material_asset_type(asset_type: &str) -> bool {
    matches!(asset_type, "text" | "image" | "music" | "ppt")
}

fn public_material_asset_type_error() -> String {
    "asset_type must be text, image, music or ppt".to_string()
}

fn normalize_cron_for_parser(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::ValidationError(
            "invalid schedule_cron".to_string(),
        ));
    }
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    let normalized = match fields.len() {
        // Support standard 5-field cron from UI, prepend seconds for parser.
        5 => format!("0 {trimmed}"),
        6 | 7 => trimmed.to_string(),
        _ => {
            return Err(AppError::ValidationError(
                "invalid schedule_cron".to_string(),
            ));
        }
    };
    if cron::Schedule::from_str(&normalized).is_err() {
        return Err(AppError::ValidationError(
            "invalid schedule_cron".to_string(),
        ));
    }
    Ok(normalized)
}

fn parse_schedule_cron(raw: &str) -> Result<cron::Schedule, AppError> {
    let normalized = normalize_cron_for_parser(raw)?;
    cron::Schedule::from_str(&normalized)
        .map_err(|_| AppError::ValidationError("invalid schedule_cron".to_string()))
}

fn mission_misfire_grace_seconds() -> i64 {
    std::env::var("PM_MISSION_MISFIRE_GRACE_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(300)
        .clamp(30, 3_600)
}

fn latest_due_mission_occurrence(
    schedule: &cron::Schedule,
    anchor: DateTime<Utc>,
    now: DateTime<Utc>,
    grace_seconds: i64,
) -> Option<DateTime<Utc>> {
    let window_start = now - chrono::Duration::seconds(grace_seconds.clamp(30, 3_600));
    schedule
        .after(&window_start)
        .take_while(|candidate| *candidate <= now)
        .filter(|candidate| *candidate > anchor)
        .last()
}

async fn is_pm_v2_enabled(_state: &AppState, _tenant_id: &str) -> bool {
    let global_default = std::env::var("PM_V2_DEFAULT_ENABLED")
        .ok()
        .and_then(|v| {
            let norm = v.trim().to_ascii_lowercase();
            Some(matches!(norm.as_str(), "1" | "true" | "yes" | "on"))
        })
        .unwrap_or(true);
    global_default
}

async fn require_pm_v2(state: &AppState, tenant_id: &str) -> Result<(), AppError> {
    if is_pm_v2_enabled(state, tenant_id).await {
        Ok(())
    } else {
        Err(AppError::ValidationError(
            "pm v2 capability disabled for this tenant".to_string(),
        ))
    }
}

// Legacy PM collection/crawler pipeline code was removed together with legacy PM tables.

fn to_api_messages(messages: &[crate::routes::chat::ChatMessage]) -> Vec<api::InputMessage> {
    messages
        .iter()
        .map(|m| {
            let text = if let Some(s) = m.content.as_str() {
                s.to_string()
            } else {
                m.content.to_string()
            };
            api::InputMessage {
                role: m.role.clone(),
                content: vec![api::InputContentBlock::Text { text }],
            }
        })
        .collect()
}

fn estimate_cost(
    input: u32,
    output: u32,
    cache_creation: u32,
    cache_read: u32,
    model: &str,
) -> f64 {
    use runtime::{ModelPricing, TokenUsage};
    let usage = TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
    };
    let pricing =
        runtime::pricing_for_model(model).unwrap_or_else(ModelPricing::default_sonnet_tier);
    usage
        .estimate_cost_usd_with_pricing(pricing)
        .total_cost_usd()
}

#[derive(Debug)]
pub(crate) struct PmChatRunResult {
    pub answer: String,
    pub usage: PmUsageDto,
    pub applied_rules: Vec<PmRuleHitDto>,
    pub api_key_id: String,
    pub provider_name: String,
}

#[derive(Debug)]
struct PmImageRunResult {
    asset_url: String,
    revised_prompt: Option<String>,
    image_size: String,
    image_mode: String,
    reference_image_count: usize,
    usage: PmUsageDto,
    api_key_id: String,
    provider_name: String,
}

#[derive(Debug)]
struct PmAudioRunResult {
    asset_url: String,
    audio_format: String,
    usage: PmUsageDto,
    api_key_id: String,
    provider_name: String,
    provider_meta: Value,
}

#[derive(Debug)]
struct PmPptRunResult {
    asset_url: String,
    html: String,
    usage: PmUsageDto,
    api_key_id: String,
    provider_name: String,
}

#[derive(Debug)]
enum PmMaterialRunResult {
    Text(PmChatRunResult),
    Image(PmImageRunResult),
    Audio(PmAudioRunResult),
    Ppt(PmPptRunResult),
}

#[derive(Debug, Deserialize)]
struct OpenAiImageGenerationResponse {
    #[serde(default)]
    data: Vec<OpenAiImageGenerationItem>,
    #[serde(default)]
    usage: Option<OpenAiImageUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiImageGenerationItem {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiImageUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAiAudioUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

fn default_pm_rules() -> Vec<PmRuleHitDto> {
    vec![PmRuleHitDto {
        rule_key: "pm_api_key_scope".to_string(),
        rule_name: "PM API Key Scope".to_string(),
        detail: "strict gateway key resolution with explicit pm scenario".to_string(),
    }]
}

fn hook_blocking_error(stage: &str, result: &runtime::HookRunResult) -> Option<AppError> {
    if result.is_denied() {
        Some(AppError::ValidationError(format!(
            "{stage} blocked by hook: {}",
            result.messages().join("\n")
        )))
    } else if result.is_cancelled() {
        Some(AppError::Internal(format!(
            "{stage} hook cancelled: {}",
            result.messages().join("\n")
        )))
    } else if result.is_failed() {
        Some(AppError::Internal(format!(
            "{stage} hook failed: {}",
            result.messages().join("\n")
        )))
    } else {
        None
    }
}

fn hook_updated_answer(result: &runtime::HookRunResult) -> Option<String> {
    let updated = result.updated_input()?;
    let value = serde_json::from_str::<Value>(updated).ok()?;
    value
        .get("answer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|answer| !answer.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) async fn run_pm_chat_completion(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    model: String,
    messages: Vec<crate::routes::chat::ChatMessage>,
) -> Result<PmChatRunResult, AppError> {
    let api_messages = to_api_messages(&messages);
    let system_prompt = "You are a product operations copilot. Always ground your answer in available evidence and clearly separate facts from hypotheses.";
    let before_hook = run_lifecycle_hooks(
        state,
        tenant_id,
        "pm",
        HookEventType::BeforeModelCall,
        "pm.model_call",
        serde_json::json!({
            "model": &model,
            "userId": user_id,
            "messageCount": messages.len(),
            "messages": messages,
            "systemPrompt": system_prompt,
        }),
        None,
        false,
    )
    .await?;
    if let Some(error) = hook_blocking_error("before_model_call", &before_hook) {
        return Err(error);
    }
    let candidates = resolve_pm_scoped_api_keys_by_model_type(state, tenant_id, "chat").await?;
    let mut last_error: Option<String> = None;
    for entry in &candidates {
        match run_pm_completion_with_key(
            entry,
            &model,
            api_messages.clone(),
            system_prompt,
            4096,
            default_pm_rules(),
            None,
        )
        .await
        {
            Ok(result) => {
                match run_lifecycle_hooks(
                    state,
                    tenant_id,
                    "pm",
                    HookEventType::AfterModelCall,
                    "pm.model_call",
                    serde_json::json!({
                        "model": &model,
                        "userId": user_id,
                        "apiKeyId": &result.api_key_id,
                        "provider": &result.provider_name,
                    }),
                    Some(serde_json::json!({
                        "answer": &result.answer,
                        "usage": &result.usage,
                        "appliedRules": &result.applied_rules,
                    })),
                    false,
                )
                .await
                {
                    Ok(hook_result) if hook_result.is_failed() || hook_result.is_cancelled() => {
                        tracing::warn!(
                            tenant_id = %tenant_id,
                            "after_model_call hook completed with warning: {}",
                            hook_result.messages().join("\n")
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            tenant_id = %tenant_id,
                            error = %error,
                            "after_model_call hook failed to execute"
                        );
                    }
                }
                return Ok(result);
            }
            Err(err) => {
                last_error = Some(err.to_string());
                tracing::warn!(
                    key_id = %entry.id,
                    provider = %entry.provider,
                    model = %entry.model.as_deref().unwrap_or(model.as_str()),
                    error = %err,
                    "PM chat completion failed on candidate key, trying failover"
                );
            }
        }
    }
    Err(AppError::Internal(format!(
        "all PM chat model candidates failed: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )))
}

pub(crate) async fn run_chat_completion_with_any_chat_key(
    state: &AppState,
    tenant_id: &str,
    model: String,
    messages: Vec<crate::routes::chat::ChatMessage>,
    system_prompt: &str,
    max_tokens: u32,
) -> Result<PmChatRunResult, AppError> {
    run_chat_completion_with_registry(
        state.config_registry(),
        tenant_id,
        model,
        messages,
        system_prompt,
        max_tokens,
    )
    .await
}

/// Run a bounded, non-streaming chat completion without requiring an `AppState`.
/// Compaction is owned by the gateway session manager, so this registry-backed
/// helper lets its semantic extraction channel use the same tenant-scoped key
/// resolution and failover policy as PM routes while avoiding a state cycle.
pub(crate) async fn run_chat_completion_with_registry(
    registry: &agent_gateway::TenantConfigRegistry,
    tenant_id: &str,
    model: String,
    messages: Vec<crate::routes::chat::ChatMessage>,
    system_prompt: &str,
    max_tokens: u32,
) -> Result<PmChatRunResult, AppError> {
    let api_messages = to_api_messages(&messages);
    let candidates = registry
        .resolve_api_keys_by_model_type(tenant_id, None, "chat")
        .await
        .map_err(|e| AppError::Internal(format!("failed to load chat API keys: {e}")))?;
    if candidates.is_empty() {
        return Err(AppError::ValidationError(
            "no enabled chat API key found; please add an api key with model_type 'chat'"
                .to_string(),
        ));
    }

    let mut last_error: Option<String> = None;
    for entry in &candidates {
        match run_pm_completion_with_key(
            entry,
            &model,
            api_messages.clone(),
            system_prompt,
            max_tokens,
            Vec::new(),
            Some("low"),
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(err) => {
                last_error = Some(err.to_string());
                tracing::warn!(
                    key_id = %entry.id,
                    provider = %entry.provider,
                    model = %entry.model.as_deref().unwrap_or(model.as_str()),
                    error = %err,
                    "chat completion failed on candidate key, trying failover"
                );
            }
        }
    }
    Err(AppError::Internal(format!(
        "all chat model candidates failed: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )))
}

async fn resolve_pm_scoped_api_keys_by_model_type(
    state: &AppState,
    tenant_id: &str,
    model_type: &str,
) -> Result<Vec<agent_gateway::ApiKeyEntry>, AppError> {
    let entries = state
        .config_registry()
        .resolve_api_keys_by_model_type(tenant_id, Some("pm"), model_type)
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "failed to load PM runtime config (model_type={model_type}): {e}"
            ))
        })?;
    if entries.is_empty() {
        let hint = match model_type {
            "image" => "no PM-scoped image API key found; please add a key with scenario 'pm' and model_type 'image'",
            "video" => "no PM-scoped video API key found; please add a key with scenario 'pm' and model_type 'video'",
            "audio" => "no PM-scoped audio API key found; please add a key with scenario 'pm' and model_type 'audio'",
            "embedding" => "no PM-scoped embedding API key found; please add a key with scenario 'pm' and model_type 'embedding'",
            _ => "no PM-scoped chat API key found; please add a key with scenario 'pm' and model_type 'chat'",
        };
        return Err(AppError::ValidationError(hint.to_string()));
    }
    Ok(entries)
}

fn effective_model_for_entry(entry: &agent_gateway::ApiKeyEntry, model_fallback: &str) -> String {
    entry
        .model
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(model_fallback)
        .to_string()
}

pub(crate) async fn run_pm_completion_with_key(
    entry: &agent_gateway::ApiKeyEntry,
    model_fallback: &str,
    api_messages: Vec<api::InputMessage>,
    system_prompt: &str,
    max_tokens: u32,
    applied_rules: Vec<PmRuleHitDto>,
    reasoning_effort: Option<&str>,
) -> Result<PmChatRunResult, AppError> {
    let effective_model = effective_model_for_entry(entry, model_fallback);
    let provider = api::build_provider(
        &entry.provider,
        &effective_model,
        &entry.key,
        entry.base_url.as_deref(),
    )
    .map_err(|e| {
        AppError::Internal(format!(
            "PM provider initialization failed for key {}: {e}",
            entry.id
        ))
    })?;
    let api_req = api::MessageRequest {
        model: effective_model.clone(),
        max_tokens,
        messages: api_messages,
        system: Some(system_prompt.to_string()),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: reasoning_effort.map(str::to_string),
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };
    let response = provider
        .send_message(&api_req)
        .await
        .map_err(|e| AppError::Internal(format!("PM LLM call failed: {e}")))?;
    let answer: String = response
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
        .join("");
    let usage = response.usage;
    let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
    let cost = estimate_cost(
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        &effective_model,
    );
    Ok(PmChatRunResult {
        answer,
        usage: PmUsageDto {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens,
            estimated_cost_usd: cost,
            model: effective_model,
        },
        applied_rules,
        api_key_id: entry.id.clone(),
        provider_name: entry.provider.clone(),
    })
}

async fn run_pm_stream_completion_with_key(
    entry: &agent_gateway::ApiKeyEntry,
    model_fallback: &str,
    api_messages: Vec<api::InputMessage>,
    system_prompt: &str,
    max_tokens: u32,
    applied_rules: Vec<PmRuleHitDto>,
) -> Result<PmChatRunResult, AppError> {
    let effective_model = effective_model_for_entry(entry, model_fallback);
    let provider = api::build_provider(
        &entry.provider,
        &effective_model,
        &entry.key,
        entry.base_url.as_deref(),
    )
    .map_err(|e| {
        AppError::Internal(format!(
            "PM provider initialization failed for key {}: {e}",
            entry.id
        ))
    })?;
    let api_req = api::MessageRequest {
        model: effective_model.clone(),
        max_tokens,
        messages: api_messages,
        system: Some(system_prompt.to_string()),
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let mut stream = provider
        .stream_message(&api_req)
        .await
        .map_err(|e| AppError::Internal(format!("PM LLM stream failed: {e}")))?;
    let mut answer = String::new();
    let mut usage = api::Usage::default();
    let mut event_count = 0_u32;
    while let Some(event) = stream
        .next_event()
        .await
        .map_err(|e| AppError::Internal(format!("PM LLM stream failed: {e}")))?
    {
        event_count = event_count.saturating_add(1);
        match event {
            api::StreamEvent::ContentBlockDelta(api::ContentBlockDeltaEvent {
                delta: api::ContentBlockDelta::TextDelta { text },
                ..
            }) => answer.push_str(&text),
            api::StreamEvent::MessageDelta(delta) => {
                usage = delta.usage;
            }
            _ => {}
        }
    }
    if usage.total_tokens() == 0 {
        if let Some(summary) = stream.usage_summary() {
            usage = summary;
        }
    }
    if answer.trim().is_empty() {
        return Err(AppError::Internal(format!(
            "PM LLM stream produced no text after {event_count} events"
        )));
    }

    let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
    let cost = estimate_cost(
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        &effective_model,
    );
    tracing::info!(
        key_id = %entry.id,
        provider = %entry.provider,
        model = %effective_model,
        event_count,
        output_chars = answer.chars().count(),
        "PM LLM stream completed"
    );
    Ok(PmChatRunResult {
        answer,
        usage: PmUsageDto {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens,
            estimated_cost_usd: cost,
            model: effective_model,
        },
        applied_rules,
        api_key_id: entry.id.clone(),
        provider_name: entry.provider.clone(),
    })
}

fn extract_user_upload_filename(url: &str, user_id: &str) -> Result<String, AppError> {
    let expected_prefix = format!("/api/v1/uploads/{user_id}/");
    let normalized = url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if !normalized.starts_with(&expected_prefix) {
        return Err(AppError::ValidationError(
            "reference_images must use your uploaded assets".to_string(),
        ));
    }
    let filename = normalized[expected_prefix.len()..].trim();
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
    {
        return Err(AppError::ValidationError(
            "reference_images contains invalid upload path".to_string(),
        ));
    }
    Ok(filename.to_string())
}

#[derive(Debug, Clone)]
struct PmReferenceImagePayload {
    filename: String,
    media_type: String,
    bytes: Vec<u8>,
}

fn reference_image_payloads(
    state: &AppState,
    user_id: &str,
    reference_images: &[PmMaterialReferenceImageInput],
) -> Result<Vec<PmReferenceImagePayload>, AppError> {
    let mut payloads = Vec::with_capacity(reference_images.len());
    for image in reference_images {
        let filename = extract_user_upload_filename(&image.url, user_id)?;
        let media_type = image
            .media_type
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_ascii_lowercase)
            .or_else(|| infer_media_type_from_filename(&filename).map(str::to_string))
            .ok_or_else(|| {
                AppError::ValidationError(
                    "reference_images must be png/jpg/webp for image editing".to_string(),
                )
            })?;
        if !matches!(
            media_type.as_str(),
            "image/png" | "image/jpeg" | "image/jpg" | "image/webp"
        ) {
            return Err(AppError::ValidationError(
                "reference_images must be png/jpg/webp for image editing".to_string(),
            ));
        }
        let path = uploads_dir_for_user(&state.data_dir, user_id).join(&filename);
        let bytes = std::fs::read(&path).map_err(|e| {
            AppError::Internal(format!(
                "failed to read reference image {}: {}",
                path.display(),
                e
            ))
        })?;
        if bytes.is_empty() {
            return Err(AppError::ValidationError(
                "reference_images contains empty file".to_string(),
            ));
        }
        payloads.push(PmReferenceImagePayload {
            filename,
            media_type,
            bytes,
        });
    }
    Ok(payloads)
}

fn reference_image_data_urls(payloads: &[PmReferenceImagePayload]) -> Vec<String> {
    payloads
        .iter()
        .map(|payload| {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&payload.bytes);
            format!("data:{};base64,{}", payload.media_type, encoded)
        })
        .collect()
}

fn build_images_edit_multipart_form(
    model: &str,
    prompt: &str,
    size: &str,
    payloads: &[PmReferenceImagePayload],
    image_field_name: &str,
) -> MultipartForm {
    let mut form = MultipartForm::new()
        .text("model", model.to_string())
        .text("prompt", prompt.to_string())
        .text("size", size.to_string());
    for payload in payloads {
        let base_part =
            MultipartPart::bytes(payload.bytes.clone()).file_name(payload.filename.clone());
        let part = match base_part.mime_str(&payload.media_type) {
            Ok(p) => p,
            Err(_) => {
                MultipartPart::bytes(payload.bytes.clone()).file_name(payload.filename.clone())
            }
        };
        form = form.part(image_field_name.to_string(), part);
    }
    form
}

fn uploads_dir_for_user(data_dir: &std::path::Path, user_id: &str) -> PathBuf {
    data_dir.join(".aos").join("uploads").join(user_id)
}

fn persist_generated_binary(
    state: &AppState,
    user_id: &str,
    bytes: &[u8],
    extension: &str,
) -> Result<String, AppError> {
    if bytes.is_empty() {
        return Err(AppError::Internal(
            "generated media bytes are empty".to_string(),
        ));
    }
    let dir = uploads_dir_for_user(&state.data_dir, user_id);
    std::fs::create_dir_all(&dir).map_err(|e| {
        AppError::Internal(format!(
            "failed to create generated upload directory {}: {e}",
            dir.display()
        ))
    })?;
    let filename = format!("{}.{}", uuid::Uuid::new_v4(), extension);
    let path = dir.join(&filename);
    std::fs::write(&path, bytes).map_err(|e| {
        AppError::Internal(format!(
            "failed to persist generated file {}: {e}",
            path.display()
        ))
    })?;
    Ok(format!("/api/v1/uploads/{user_id}/{filename}"))
}

fn generated_upload_path_for_url(
    state: &AppState,
    user_id: &str,
    url: &str,
) -> Result<PathBuf, AppError> {
    let filename = extract_user_upload_filename(url, user_id)?;
    Ok(uploads_dir_for_user(&state.data_dir, user_id).join(filename))
}

fn strip_markdown_code_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    let after_lang = rest
        .strip_prefix("html")
        .or_else(|| rest.strip_prefix("HTML"))
        .unwrap_or(rest);
    let after_newline = after_lang
        .strip_prefix('\n')
        .or_else(|| after_lang.strip_prefix("\r\n"))
        .unwrap_or(after_lang);
    after_newline
        .strip_suffix("```")
        .unwrap_or(after_newline)
        .trim()
        .to_string()
}

fn escape_html_text(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn normalize_ppt_html(raw: &str) -> String {
    let cleaned = strip_markdown_code_fence(raw);
    if is_html_document_body(&cleaned) {
        return cleaned;
    }
    let escaped = escape_html_text(&cleaned);
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>AOS PPT Deck</title>
  <style>
    :root {{ color-scheme: light; font-family: Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    body {{ margin: 0; background: #101418; color: #111827; }}
    .deck {{ width: 100vw; height: 100vh; overflow: hidden; }}
    .slide {{ display: none; box-sizing: border-box; width: 100vw; height: 56.25vw; min-height: 100vh; padding: 56px 72px; background: #f8fafc; }}
    .slide.is-active {{ display: flex; flex-direction: column; gap: 24px; }}
    h1 {{ margin: 0; font-size: 44px; line-height: 1.08; }}
    pre {{ white-space: pre-wrap; word-break: break-word; margin: 0; font-size: 22px; line-height: 1.5; }}
    .footer {{ margin-top: auto; display: flex; justify-content: space-between; color: #64748b; font-size: 14px; }}
  </style>
</head>
<body>
  <main class="deck">
    <section class="slide is-active">
      <h1>AOS PPT Deck</h1>
      <pre>{escaped}</pre>
      <div class="footer"><span>Generated by AOS</span><span>1 / 1</span></div>
    </section>
  </main>
  <script>
    const slides = Array.from(document.querySelectorAll('.slide'));
    let idx = 0;
    function show(next) {{
      idx = Math.max(0, Math.min(slides.length - 1, next));
      slides.forEach((s, i) => s.classList.toggle('is-active', i === idx));
    }}
    window.addEventListener('keydown', (event) => {{
      if (['ArrowRight', ' ', 'PageDown'].includes(event.key)) show(idx + 1);
      if (['ArrowLeft', 'PageUp'].includes(event.key)) show(idx - 1);
      if (event.key === 'Home') show(0);
      if (event.key === 'End') show(slides.length - 1);
    }});
  </script>
</body>
</html>"#
    )
}

fn is_final_ppt_html_asset(meta: &Value, url: Option<&str>, content_text: Option<&str>) -> bool {
    let workflow_stage = meta
        .get("workflowStage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !workflow_stage.eq_ignore_ascii_case("generate") {
        return false;
    }

    let generated_kind = meta
        .get("extra")
        .and_then(|extra| extra.get("generatedKind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if generated_kind.eq_ignore_ascii_case("html_ppt") {
        return true;
    }

    let url_is_html = url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let path = value
                .split(['?', '#'])
                .next()
                .unwrap_or(value)
                .to_ascii_lowercase();
            path.ends_with(".html") || path.ends_with(".htm")
        })
        .unwrap_or(false);
    if url_is_html {
        return true;
    }

    content_text
        .map(strip_markdown_code_fence)
        .map(|content| is_html_document_body(&content))
        .unwrap_or(false)
}

fn persist_generated_html(state: &AppState, user_id: &str, html: &str) -> Result<String, AppError> {
    persist_generated_binary(state, user_id, html.as_bytes(), "html")
}

fn material_export_timeout_secs() -> u64 {
    pm_env_u64("PM_MATERIAL_EXPORT_TIMEOUT_SECS", 120).clamp(10, 600)
}

async fn command_exists(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .await
        .map(|output| {
            output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty()
        })
        .unwrap_or(false)
}

async fn find_chrome_binary() -> Option<String> {
    if let Ok(path) = std::env::var("AOS_CHROME_BIN") {
        let trimmed = path.trim();
        if !trimmed.is_empty() && FsPath::new(trimmed).exists() {
            return Some(trimmed.to_string());
        }
    }
    for candidate in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        if candidate.starts_with('/') {
            if FsPath::new(candidate).exists() {
                return Some(candidate.to_string());
            }
        } else if command_exists(candidate).await {
            return Some(candidate.to_string());
        }
    }
    None
}

async fn find_soffice_binary() -> Option<String> {
    if let Ok(path) = std::env::var("AOS_SOFFICE_BIN") {
        let trimmed = path.trim();
        if !trimmed.is_empty() && FsPath::new(trimmed).exists() {
            return Some(trimmed.to_string());
        }
    }
    for candidate in [
        "/Applications/LibreOffice.app/Contents/MacOS/soffice",
        "soffice",
        "libreoffice",
    ] {
        if candidate.starts_with('/') {
            if FsPath::new(candidate).exists() {
                return Some(candidate.to_string());
            }
        } else if command_exists(candidate).await {
            return Some(candidate.to_string());
        }
    }
    None
}

async fn run_command_with_timeout(
    mut command: Command,
    timeout_secs: u64,
) -> Result<Vec<u8>, AppError> {
    let output = tokio::time::timeout(Duration::from_secs(timeout_secs), command.output())
        .await
        .map_err(|_| AppError::Internal(format!("export command timed out after {timeout_secs}s")))?
        .map_err(|e| AppError::Internal(format!("failed to run export command: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Internal(format!(
            "export command failed: {}",
            stderr.trim()
        )));
    }
    Ok(output.stdout)
}

async fn export_html_to_pdf(html_path: &FsPath, output_path: &FsPath) -> Result<(), AppError> {
    let chrome = find_chrome_binary().await.ok_or_else(|| {
        AppError::ValidationError(
            "PDF export requires Chrome/Chromium. Set AOS_CHROME_BIN or install google-chrome/chromium."
                .to_string(),
        )
    })?;
    let mut command = Command::new(chrome);
    command
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--allow-file-access-from-files")
        .arg("--run-all-compositor-stages-before-draw")
        .arg("--virtual-time-budget=3000")
        .arg("--print-to-pdf-no-header")
        .arg(format!("--print-to-pdf={}", output_path.display()))
        .arg(format!("file://{}", html_path.display()));
    run_command_with_timeout(command, material_export_timeout_secs()).await?;
    if !output_path.exists() {
        return Err(AppError::Internal(
            "PDF export did not produce output file".to_string(),
        ));
    }
    Ok(())
}

async fn export_html_to_pptx(html_path: &FsPath, output_path: &FsPath) -> Result<(), AppError> {
    let soffice = find_soffice_binary().await.ok_or_else(|| {
        AppError::ValidationError(
            "PPTX export requires LibreOffice/soffice. HTML preview and PDF export remain available."
                .to_string(),
        )
    })?;
    let Some(out_dir) = output_path.parent() else {
        return Err(AppError::Internal("invalid PPTX export path".to_string()));
    };
    let mut command = Command::new(soffice);
    command
        .arg("--headless")
        .arg("--convert-to")
        .arg("pptx")
        .arg("--outdir")
        .arg(out_dir)
        .arg(html_path);
    run_command_with_timeout(command, material_export_timeout_secs()).await?;
    if output_path.exists() {
        return Ok(());
    }
    let fallback = out_dir.join(
        html_path
            .file_stem()
            .and_then(|v| v.to_str())
            .map(|stem| format!("{stem}.pptx"))
            .unwrap_or_else(|| "deck.pptx".to_string()),
    );
    if fallback.exists() {
        std::fs::rename(&fallback, output_path).map_err(|e| {
            AppError::Internal(format!(
                "failed to move PPTX export {} -> {}: {e}",
                fallback.display(),
                output_path.display()
            ))
        })?;
        return Ok(());
    }
    Err(AppError::Internal(
        "PPTX export did not produce output file".to_string(),
    ))
}

fn persist_generated_png_base64(
    state: &AppState,
    user_id: &str,
    image_base64: &str,
) -> Result<String, AppError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image_base64.as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(image_base64))
        .map_err(|e| AppError::Internal(format!("decode generated image base64 failed: {e}")))?;
    persist_generated_binary(state, user_id, &bytes, "png")
}

async fn run_pm_image_generation_with_key(
    state: &AppState,
    user_id: &str,
    entry: &agent_gateway::ApiKeyEntry,
    model_fallback: &str,
    prompt: &str,
    reference_images: &[PmMaterialReferenceImageInput],
) -> Result<PmImageRunResult, AppError> {
    let effective_model = effective_model_for_entry(entry, model_fallback);
    let base = entry
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1");
    let inferred_size = infer_image_size_from_prompt(prompt);
    let (request_body, endpoint_builder, image_mode, reference_image_count, reference_payloads) =
        if reference_images.is_empty() {
            (
                serde_json::json!({
                    "model": effective_model,
                    "prompt": prompt,
                    "size": inferred_size
                }),
                api::images_generations_endpoint as fn(&str) -> String,
                "generate".to_string(),
                0_usize,
                Vec::new(),
            )
        } else {
            let payloads = reference_image_payloads(state, user_id, reference_images)?;
            let image_inputs = reference_image_data_urls(&payloads)
                .into_iter()
                .map(|image_url| serde_json::json!({ "image_url": image_url }))
                .collect::<Vec<_>>();
            let count = image_inputs.len();
            (
                serde_json::json!({
                    "model": effective_model,
                    "prompt": prompt,
                    "size": inferred_size,
                    "images": image_inputs
                }),
                images_edits_endpoint as fn(&str) -> String,
                "edit".to_string(),
                count,
                payloads,
            )
        };
    let client = api::build_http_client_or_default();

    let mut last_error: Option<String> = None;
    for endpoint in openai_endpoint_candidates(base, endpoint_builder) {
        let mut variants: Vec<(&str, bool)> = Vec::new();
        if image_mode == "edit" {
            variants.push(("multipart:image", true));
            variants.push(("multipart:image[]", true));
            variants.push(("json", false));
        } else {
            variants.push(("json", false));
        }

        for (variant_label, use_multipart) in variants {
            let response = if use_multipart {
                let image_field_name = if variant_label.contains("image[]") {
                    "image[]"
                } else {
                    "image"
                };
                let form = build_images_edit_multipart_form(
                    &effective_model,
                    prompt,
                    &inferred_size,
                    &reference_payloads,
                    image_field_name,
                );
                match client
                    .post(&endpoint)
                    .header(AUTHORIZATION, format!("Bearer {}", entry.key))
                    .header(ACCEPT, "application/json")
                    .multipart(form)
                    .send()
                    .await
                {
                    Ok(resp) => resp,
                    Err(e) => {
                        last_error = Some(format!(
                            "PM image API connection failed for {} ({variant_label}): {}",
                            endpoint, e
                        ));
                        continue;
                    }
                }
            } else {
                match client
                    .post(&endpoint)
                    .header(AUTHORIZATION, format!("Bearer {}", entry.key))
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json")
                    .json(&request_body)
                    .send()
                    .await
                {
                    Ok(resp) => resp,
                    Err(e) => {
                        last_error = Some(format!(
                            "PM image API connection failed for {} ({variant_label}): {}",
                            endpoint, e
                        ));
                        continue;
                    }
                }
            };

            let status = response.status();
            let body = match response.text().await {
                Ok(text) => text,
                Err(e) => {
                    last_error = Some(format!(
                        "PM image API body read failed for {} ({variant_label}): {}",
                        endpoint, e
                    ));
                    continue;
                }
            };

            if !status.is_success() {
                let detail = excerpt_chars(&body, 240);
                last_error = Some(format!(
                    "image API returned status {} from {} ({variant_label}): {}",
                    status, endpoint, detail
                ));
                continue;
            }

            if is_html_document_body(&body) {
                let detail = excerpt_chars(&body, 240);
                last_error = Some(format!(
                    "image API returned HTML instead of JSON from {} ({variant_label}). base_url may point to a web page rather than an OpenAI-compatible API root (usually should end with /v1). body: {}",
                    endpoint, detail
                ));
                continue;
            }

            let payload: OpenAiImageGenerationResponse = match serde_json::from_str(&body) {
                Ok(parsed) => parsed,
                Err(e) => {
                    if let Ok(raw_json) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(error_obj) = raw_json.get("error") {
                            last_error = Some(format!(
                                "image API returned error object from {} ({variant_label}): {}",
                                endpoint,
                                excerpt_chars(&error_obj.to_string(), 240)
                            ));
                            continue;
                        }
                    }
                    last_error = Some(format!(
                        "failed to parse image generation response from {} ({variant_label}): {}; first 240 chars: {}",
                        endpoint,
                        e,
                        excerpt_chars(&body, 240)
                    ));
                    continue;
                }
            };

            let first = match payload.data.into_iter().next() {
                Some(item) => item,
                None => {
                    last_error = Some(format!(
                        "image generation returned empty data from {} ({variant_label})",
                        endpoint
                    ));
                    continue;
                }
            };
            let asset_url = if let Some(url) = first
                .url
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                url.to_string()
            } else if let Some(b64) = first
                .b64_json
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                persist_generated_png_base64(state, user_id, b64)?
            } else {
                last_error = Some(format!(
                    "image generation response missing url and b64_json from {} ({variant_label})",
                    endpoint
                ));
                continue;
            };
            let usage = payload.usage.unwrap_or(OpenAiImageUsage {
                input_tokens: 0,
                output_tokens: 0,
            });
            let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
            let cost = estimate_cost(
                usage.input_tokens,
                usage.output_tokens,
                0,
                0,
                &effective_model,
            );
            return Ok(PmImageRunResult {
                asset_url,
                revised_prompt: first.revised_prompt,
                image_size: inferred_size.clone(),
                image_mode: image_mode.clone(),
                reference_image_count,
                usage: PmUsageDto {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_tokens,
                    estimated_cost_usd: cost,
                    model: effective_model,
                },
                api_key_id: entry.id.clone(),
                provider_name: entry.provider.clone(),
            });
        }
    }

    Err(AppError::Internal(last_error.unwrap_or_else(|| {
        "image generation request failed".to_string()
    })))
}

async fn download_and_persist_audio_from_url(
    state: &AppState,
    user_id: &str,
    url: &str,
) -> Result<(String, String), AppError> {
    let client = api::build_http_client_or_default();
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("download generated audio failed: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::Internal(format!(
            "download generated audio returned status {} from {}",
            status, url
        )));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/mpeg")
        .to_string();
    let ext = infer_audio_extension_from_content_type(&content_type);
    let bytes = response
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("read generated audio bytes failed: {e}")))?;
    let persisted = persist_generated_binary(state, user_id, bytes.as_ref(), ext)?;
    Ok((persisted, ext.to_string()))
}

async fn read_suno_json_response(
    response: reqwest::Response,
    endpoint: &str,
) -> Result<Value, AppError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("read Suno response body failed: {e}")))?;

    if !status.is_success() {
        return Err(AppError::Internal(format!(
            "Suno API returned status {} from {}: {}",
            status,
            endpoint,
            excerpt_chars(&body, 240)
        )));
    }
    if is_html_document_body(&body) {
        return Err(AppError::Internal(format!(
            "Suno API returned HTML instead of JSON from {}. base_url may not point to a Suno API root: {}",
            endpoint,
            excerpt_chars(&body, 240)
        )));
    }
    serde_json::from_str(&body).map_err(|e| {
        AppError::Internal(format!(
            "parse Suno JSON response failed from {}: {}; body: {}",
            endpoint,
            e,
            excerpt_chars(&body, 240)
        ))
    })
}

async fn fetch_suno_task_snapshot(
    client: &reqwest::Client,
    entry: &agent_gateway::ApiKeyEntry,
    endpoint: &str,
    task_id: &str,
) -> Result<Value, AppError> {
    let get_response = client
        .get(endpoint)
        .header(AUTHORIZATION, format!("Bearer {}", entry.key))
        .query(&[("taskId", task_id)])
        .send()
        .await;
    if let Ok(resp) = get_response {
        if resp.status().is_success() {
            return read_suno_json_response(resp, endpoint).await;
        }
    }

    let post_response = client
        .post(endpoint)
        .header(AUTHORIZATION, format!("Bearer {}", entry.key))
        .header(CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({ "taskId": task_id }))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("query Suno task snapshot failed: {e}")))?;

    read_suno_json_response(post_response, endpoint).await
}

async fn run_pm_suno_audio_generation_with_key(
    state: &AppState,
    user_id: &str,
    entry: &agent_gateway::ApiKeyEntry,
    continuation_asset: Option<&PmContinuationAssetContext>,
    prompt: &str,
) -> Result<PmAudioRunResult, AppError> {
    let base = entry
        .base_url
        .as_deref()
        .ok_or_else(|| AppError::ValidationError("Suno audio key requires base_url".to_string()))?;
    let client = api::build_http_client_or_default();
    let prompt_text = compact_suno_prompt(prompt);
    let model_version = suno_generation_model_from_entry_model(entry.model.as_deref());
    let callback_url = suno_callback_url();
    let explicit_generate_path = entry.audio_generate_path.as_deref();
    let explicit_query_path = entry.audio_query_path.as_deref();
    let previous_audio_id = suno_extract_previous_audio_id(continuation_asset);
    let previous_task_id = suno_extract_previous_task_id(continuation_asset);
    let previous_audio_url = suno_extract_previous_audio_url(continuation_asset);
    let should_extend = previous_audio_id.is_some()
        && !prompt_requests_restart(&prompt_text)
        && prompt_requests_extend(&prompt_text);
    let generation_plan = build_suno_generation_plan(&prompt_text);
    let continue_at = parse_first_number_after_keywords(
        &prompt_text,
        &["continue at", "from second", "from", "at", "秒开始", "从"],
    );

    struct SunoSubmitPlan {
        operation: String,
        source_audio_id: Option<String>,
        source_task_id: Option<String>,
        source_audio_urls: Vec<String>,
        submit_endpoints: Vec<String>,
        submit_variants: Vec<Value>,
    }

    let mut last_error: Option<String> = None;
    let mut selected_operation = "generate".to_string();
    let mut selected_source_audio_id = None::<String>;
    let mut selected_source_task_id = None::<String>;
    let mut selected_source_audio_urls = Vec::<String>::new();
    let mut submit_plans: Vec<SunoSubmitPlan> = Vec::new();
    let default_style = generation_plan
        .style
        .clone()
        .unwrap_or_else(|| "Pop".to_string());
    let default_title = generation_plan
        .title
        .clone()
        .unwrap_or_else(|| "AI Generated Track".to_string());
    let default_negative_tags = generation_plan
        .negative_tags
        .clone()
        .unwrap_or_else(|| default_suno_negative_tags(generation_plan.instrumental));
    let default_tags = parse_comp_line_value(&prompt_text, "tags")
        .or_else(|| parse_comp_line_value(&prompt_text, "风格标签"))
        .or_else(|| generation_plan.style.clone())
        .unwrap_or_else(|| "Pop".to_string());
    let restart_requested = prompt_requests_restart(&prompt_text);
    let prompt_urls = extract_http_urls_from_prompt(&prompt_text);

    if !restart_requested && prompt_requests_replace_section(&prompt_text) {
        if let (Some(task_id), Some(audio_id), Some((start_s, end_s))) = (
            previous_task_id.clone(),
            previous_audio_id.clone(),
            parse_replace_section_range(&prompt_text),
        ) {
            submit_plans.push(SunoSubmitPlan {
                operation: "replace-section".to_string(),
                source_audio_id: Some(audio_id.clone()),
                source_task_id: Some(task_id.clone()),
                source_audio_urls: previous_audio_url
                    .clone()
                    .into_iter()
                    .collect::<Vec<String>>(),
                submit_endpoints: suno_submit_endpoint_candidates(
                    base,
                    explicit_generate_path,
                    &["api/v1/generate/replace-section"],
                ),
                submit_variants: vec![serde_json::json!({
                    "taskId": task_id,
                    "audioId": audio_id,
                    "prompt": generation_plan.prompt.chars().take(5000).collect::<String>(),
                    "tags": default_tags,
                    "title": generation_plan.title.clone().unwrap_or_else(|| "Edited Section".to_string()),
                    "infillStartS": start_s,
                    "infillEndS": end_s,
                    "callBackUrl": callback_url
                })],
            });
        }
    }

    if !restart_requested && prompt_requests_add_vocals(&prompt_text) {
        let upload_url = previous_audio_url
            .clone()
            .or_else(|| prompt_urls.first().cloned());
        if let Some(url) = upload_url {
            let mut payload = serde_json::json!({
                "uploadUrl": url,
                "callBackUrl": callback_url,
                "prompt": generation_plan.prompt.chars().take(5000).collect::<String>(),
                "title": generation_plan.title.clone().unwrap_or_else(|| "Vocal Version".to_string()),
                "negativeTags": default_negative_tags,
                "style": default_style
            });
            if let Some(vocal_gender) = generation_plan.vocal_gender.clone() {
                payload["vocalGender"] = Value::String(vocal_gender);
            }
            submit_plans.push(SunoSubmitPlan {
                operation: "add-vocals".to_string(),
                source_audio_id: previous_audio_id.clone(),
                source_task_id: previous_task_id.clone(),
                source_audio_urls: vec![url],
                submit_endpoints: suno_submit_endpoint_candidates(
                    base,
                    explicit_generate_path,
                    &["api/v1/generate/add-vocals"],
                ),
                submit_variants: vec![payload],
            });
        }
    }

    if !restart_requested && prompt_requests_add_instrumental(&prompt_text) {
        let upload_url = previous_audio_url
            .clone()
            .or_else(|| prompt_urls.first().cloned());
        if let Some(url) = upload_url {
            let mut payload = serde_json::json!({
                "uploadUrl": url,
                "title": generation_plan.title.clone().unwrap_or_else(|| "Instrumental Version".to_string()),
                "negativeTags": default_negative_tags,
                "tags": default_tags,
                "callBackUrl": callback_url
            });
            if let Some(vocal_gender) = generation_plan.vocal_gender.clone() {
                payload["vocalGender"] = Value::String(vocal_gender);
            }
            submit_plans.push(SunoSubmitPlan {
                operation: "add-instrumental".to_string(),
                source_audio_id: previous_audio_id.clone(),
                source_task_id: previous_task_id.clone(),
                source_audio_urls: vec![url],
                submit_endpoints: suno_submit_endpoint_candidates(
                    base,
                    explicit_generate_path,
                    &["api/v1/generate/add-instrumental"],
                ),
                submit_variants: vec![payload],
            });
        }
    }

    if !restart_requested && prompt_requests_mashup(&prompt_text) {
        let mut mashup_urls = prompt_urls.clone();
        if let Some(url) = previous_audio_url.clone() {
            if !mashup_urls.iter().any(|item| item == &url) {
                mashup_urls.push(url);
            }
        }
        if mashup_urls.len() >= 2 {
            let upload_url_list = mashup_urls.iter().take(2).cloned().collect::<Vec<String>>();
            submit_plans.push(SunoSubmitPlan {
                operation: "mashup".to_string(),
                source_audio_id: previous_audio_id.clone(),
                source_task_id: previous_task_id.clone(),
                source_audio_urls: upload_url_list.clone(),
                submit_endpoints: suno_submit_endpoint_candidates(
                    base,
                    explicit_generate_path,
                    &["api/v1/generate/mashup"],
                ),
                submit_variants: vec![serde_json::json!({
                    "uploadUrlList": upload_url_list,
                    "customMode": false,
                    "prompt": generation_plan.prompt.chars().take(500).collect::<String>(),
                    "model": model_version,
                    "callBackUrl": callback_url
                })],
            });
        }
    }

    if let Some(audio_id) = previous_audio_id.clone() {
        if should_extend {
            submit_plans.push(SunoSubmitPlan {
                operation: "extend".to_string(),
                source_audio_id: Some(audio_id.clone()),
                source_task_id: previous_task_id.clone(),
                source_audio_urls: previous_audio_url
                    .clone()
                    .into_iter()
                    .collect::<Vec<String>>(),
                submit_endpoints: suno_submit_endpoint_candidates(
                    base,
                    explicit_generate_path,
                    &["api/v1/generate/extend"],
                ),
                submit_variants: vec![
                    // Variant A: reuse source audio default settings (most stable).
                    serde_json::json!({
                        "defaultParamFlag": false,
                        "audioId": audio_id,
                        "model": model_version,
                        "callBackUrl": callback_url
                    }),
                    // Variant B: explicit extension controls from the prompt.
                    serde_json::json!({
                        "defaultParamFlag": true,
                        "audioId": previous_audio_id.clone(),
                        "prompt": generation_plan.prompt.chars().take(5000).collect::<String>(),
                        "style": generation_plan.style.clone().unwrap_or_else(|| "Pop".to_string()),
                        "title": generation_plan.title.clone().unwrap_or_else(|| "Extended Track".to_string()),
                        "continueAt": continue_at.unwrap_or(60.0),
                        "negativeTags": generation_plan.negative_tags.clone(),
                        "vocalGender": generation_plan.vocal_gender.clone(),
                        "model": model_version,
                        "callBackUrl": callback_url
                    }),
                ],
            });
        }
    }

    let mut generate_submit_variants: Vec<Value> = Vec::new();
    if generation_plan.custom_mode {
        let mut custom_payload = serde_json::json!({
            "customMode": true,
            "instrumental": generation_plan.instrumental,
            "prompt": generation_plan.prompt,
            "style": generation_plan.style.clone().unwrap_or_else(|| "Pop".to_string()),
            "title": default_title,
            "model": model_version,
            "negativeTags": default_negative_tags,
            "callBackUrl": callback_url
        });
        if let Some(vocal_gender) = generation_plan.vocal_gender.clone() {
            custom_payload["vocalGender"] = Value::String(vocal_gender);
        }
        generate_submit_variants.push(custom_payload);
    }
    generate_submit_variants.push(serde_json::json!({
        "prompt": generation_plan.prompt.chars().take(500).collect::<String>(),
        "customMode": false,
        "instrumental": generation_plan.instrumental,
        "model": model_version,
        "callBackUrl": callback_url
    }));
    generate_submit_variants.push(serde_json::json!({
        "prompt": generation_plan.prompt.chars().take(500).collect::<String>(),
        "customMode": false,
        "instrumental": generation_plan.instrumental,
        "model": model_version
    }));
    submit_plans.push(SunoSubmitPlan {
        operation: "generate".to_string(),
        source_audio_id: None,
        source_task_id: None,
        source_audio_urls: Vec::new(),
        submit_endpoints: suno_generate_endpoint_candidates(base, explicit_generate_path),
        submit_variants: generate_submit_variants,
    });

    let mut task_id: Option<String> = None;
    'plans: for plan in submit_plans {
        for endpoint in plan.submit_endpoints {
            for submit_body in &plan.submit_variants {
                let response = match client
                    .post(&endpoint)
                    .header(AUTHORIZATION, format!("Bearer {}", entry.key))
                    .header(CONTENT_TYPE, "application/json")
                    .json(submit_body)
                    .send()
                    .await
                {
                    Ok(resp) => resp,
                    Err(e) => {
                        last_error = Some(format!(
                            "Suno submission connection failed for {}: {}",
                            endpoint, e
                        ));
                        continue;
                    }
                };
                let payload = match read_suno_json_response(response, &endpoint).await {
                    Ok(value) => value,
                    Err(err) => {
                        last_error = Some(err.to_string());
                        continue;
                    }
                };
                if let Some(code) = payload
                    .get("code")
                    .and_then(Value::as_i64)
                    .filter(|code| *code != 200)
                {
                    last_error = Some(format!(
                        "Suno submission rejected (code={code}) from {}: {}",
                        endpoint,
                        excerpt_chars(&payload.to_string(), 240)
                    ));
                    continue;
                }
                task_id = suno_extract_task_id(&payload);
                if task_id.is_some() {
                    selected_operation = plan.operation.clone();
                    selected_source_audio_id = plan.source_audio_id.clone();
                    selected_source_task_id = plan.source_task_id.clone();
                    selected_source_audio_urls = plan.source_audio_urls.clone();
                    break 'plans;
                }
                if let Some(url) = suno_extract_audio_url(&payload) {
                    let (asset_url, ext) =
                        match download_and_persist_audio_from_url(state, user_id, &url).await {
                            Ok(saved) => saved,
                            Err(_) => (url, "mp3".to_string()),
                        };
                    return Ok(PmAudioRunResult {
                        asset_url,
                        audio_format: ext,
                        usage: PmUsageDto {
                            input_tokens: 0,
                            output_tokens: 0,
                            total_tokens: 0,
                            estimated_cost_usd: 0.0,
                            model: "suno".to_string(),
                        },
                        api_key_id: entry.id.clone(),
                        provider_name: entry.provider.clone(),
                        provider_meta: serde_json::json!({
                            "engine": "sunoapi",
                            "operation": plan.operation,
                            "taskId": Value::Null,
                            "sourceAudioId": plan.source_audio_id,
                            "sourceTaskId": plan.source_task_id,
                            "sourceAudioUrls": plan.source_audio_urls
                        }),
                    });
                }
                last_error = Some(format!(
                    "Suno submission response missing taskId from {} (operation={}): {}",
                    endpoint,
                    plan.operation,
                    excerpt_chars(&payload.to_string(), 240)
                ));
            }
        }
    }

    let task_id = task_id.ok_or_else(|| {
        AppError::Internal(
            last_error.unwrap_or_else(|| "Suno submission failed: missing taskId".to_string()),
        )
    })?;

    let timeout_at =
        std::time::Instant::now() + std::time::Duration::from_secs(suno_poll_timeout_secs());
    let poll_interval = std::time::Duration::from_millis(suno_poll_interval_ms());
    let mut last_snapshot_error: Option<String> = None;
    let mut first_ready_audio_url: Option<String> = None;
    let mut observed_status: Option<String> = None;
    let mut selected_track_meta = Value::Null;

    'polling: while std::time::Instant::now() < timeout_at {
        for endpoint in suno_record_info_endpoint_candidates(base, explicit_query_path, &task_id) {
            match fetch_suno_task_snapshot(&client, entry, &endpoint, &task_id).await {
                Ok(snapshot) => {
                    if let Some(status) = suno_extract_status(&snapshot) {
                        observed_status = Some(status.clone());
                        if suno_is_failure_status(&status) {
                            return Err(AppError::Internal(format!(
                                "Suno generation failed (taskId={}, status={}): {}",
                                task_id,
                                status,
                                excerpt_chars(&snapshot.to_string(), 240)
                            )));
                        }
                        if suno_is_success_status(&status) {
                            if let Some(audio_url) = suno_extract_audio_url(&snapshot) {
                                first_ready_audio_url = Some(audio_url);
                                selected_track_meta = suno_track_to_meta(
                                    suno_extract_primary_track(&snapshot).as_ref(),
                                );
                                break 'polling;
                            }
                        }
                        if suno_is_processing_status(&status) {
                            continue;
                        }
                    }
                    if let Some(audio_url) = suno_extract_audio_url(&snapshot) {
                        first_ready_audio_url = Some(audio_url);
                        selected_track_meta =
                            suno_track_to_meta(suno_extract_primary_track(&snapshot).as_ref());
                        break 'polling;
                    }
                    last_snapshot_error = Some(format!(
                        "Suno snapshot has no playable audio yet (taskId={}): {}",
                        task_id,
                        excerpt_chars(&snapshot.to_string(), 240)
                    ));
                }
                Err(err) => {
                    last_snapshot_error = Some(err.to_string());
                }
            }
        }
        tokio::time::sleep(poll_interval).await;
    }

    let audio_url = first_ready_audio_url.ok_or_else(|| {
        AppError::Internal(format!(
            "Suno generation timeout (taskId={}, status={}){}",
            task_id,
            observed_status.unwrap_or_else(|| "unknown".to_string()),
            last_snapshot_error
                .map(|v| format!(": {v}"))
                .unwrap_or_default()
        ))
    })?;
    let (asset_url, audio_format) =
        match download_and_persist_audio_from_url(state, user_id, &audio_url).await {
            Ok(saved) => saved,
            Err(_) => (audio_url, "mp3".to_string()),
        };
    Ok(PmAudioRunResult {
        asset_url,
        audio_format,
        usage: PmUsageDto {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            estimated_cost_usd: 0.0,
            model: "suno".to_string(),
        },
        api_key_id: entry.id.clone(),
        provider_name: entry.provider.clone(),
        provider_meta: serde_json::json!({
            "engine": "sunoapi",
            "operation": selected_operation,
            "taskId": task_id,
            "sourceAudioId": selected_source_audio_id,
            "sourceTaskId": selected_source_task_id,
            "sourceAudioUrls": selected_source_audio_urls,
            "track": selected_track_meta
        }),
    })
}

async fn run_pm_audio_generation_with_key(
    state: &AppState,
    user_id: &str,
    entry: &agent_gateway::ApiKeyEntry,
    model_fallback: &str,
    continuation_asset: Option<&PmContinuationAssetContext>,
    prompt: &str,
) -> Result<PmAudioRunResult, AppError> {
    let effective_model = effective_model_for_entry(entry, model_fallback);
    if is_suno_audio_model(&effective_model) {
        return run_pm_suno_audio_generation_with_key(
            state,
            user_id,
            entry,
            continuation_asset,
            prompt,
        )
        .await;
    }
    let base = entry
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1");
    let client = api::build_http_client_or_default();
    let response_format = sanitize_audio_format(
        std::env::var("PM_MUSIC_AUDIO_FORMAT")
            .ok()
            .as_deref()
            .unwrap_or("mp3"),
    );
    let voice = std::env::var("PM_MUSIC_AUDIO_VOICE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "alloy".to_string());

    let request_variants: Vec<serde_json::Value> = vec![
        serde_json::json!({
            "model": effective_model.clone(),
            "input": prompt,
            "voice": voice,
            "response_format": response_format
        }),
        serde_json::json!({
            "model": effective_model.clone(),
            "input": prompt,
            "response_format": response_format
        }),
        serde_json::json!({
            "model": effective_model.clone(),
            "prompt": prompt,
            "voice": voice,
            "response_format": response_format
        }),
        serde_json::json!({
            "model": effective_model.clone(),
            "prompt": prompt,
            "response_format": response_format
        }),
    ];

    let endpoint_builders: [fn(&str) -> String; 2] =
        [audio_speech_endpoint, audio_generations_endpoint];
    let mut last_error: Option<String> = None;

    for endpoint_builder in endpoint_builders {
        for endpoint in openai_endpoint_candidates(base, endpoint_builder) {
            for request_body in &request_variants {
                let response = match client
                    .post(&endpoint)
                    .header(AUTHORIZATION, format!("Bearer {}", entry.key))
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "*/*")
                    .json(request_body)
                    .send()
                    .await
                {
                    Ok(resp) => resp,
                    Err(e) => {
                        last_error = Some(format!(
                            "PM audio API connection failed for {}: {}",
                            endpoint, e
                        ));
                        continue;
                    }
                };

                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    last_error = Some(format!(
                        "audio API returned status {} from {}: {}",
                        status,
                        endpoint,
                        excerpt_chars(&body, 240)
                    ));
                    continue;
                }

                let content_type = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let content_type_lower = content_type.to_ascii_lowercase();

                if content_type_lower.starts_with("audio/")
                    || content_type_lower.contains("mpeg")
                    || content_type_lower.contains("wav")
                    || content_type_lower.contains("ogg")
                    || content_type_lower.contains("flac")
                    || content_type_lower.contains("octet-stream")
                {
                    let bytes = match response.bytes().await {
                        Ok(body) => body,
                        Err(e) => {
                            last_error = Some(format!(
                                "audio API body read failed for {}: {}",
                                endpoint, e
                            ));
                            continue;
                        }
                    };
                    let ext = infer_audio_extension_from_content_type(&content_type);
                    let asset_url = persist_generated_binary(state, user_id, bytes.as_ref(), ext)?;
                    let total_tokens = 0_u32;
                    let cost = estimate_cost(0, 0, 0, 0, &effective_model);
                    return Ok(PmAudioRunResult {
                        asset_url,
                        audio_format: ext.to_string(),
                        usage: PmUsageDto {
                            input_tokens: 0,
                            output_tokens: 0,
                            total_tokens,
                            estimated_cost_usd: cost,
                            model: effective_model.clone(),
                        },
                        api_key_id: entry.id.clone(),
                        provider_name: entry.provider.clone(),
                        provider_meta: Value::Null,
                    });
                }

                let body = match response.text().await {
                    Ok(text) => text,
                    Err(e) => {
                        last_error = Some(format!(
                            "audio API body read failed for {}: {}",
                            endpoint, e
                        ));
                        continue;
                    }
                };

                if is_html_document_body(&body) {
                    last_error = Some(format!(
                        "audio API returned HTML instead of JSON from {}. base_url may point to a web page rather than an OpenAI-compatible API root (usually should end with /v1). body: {}",
                        endpoint,
                        excerpt_chars(&body, 240)
                    ));
                    continue;
                }

                let payload: serde_json::Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(e) => {
                        last_error = Some(format!(
                            "failed to parse audio generation response from {}: {}; first 240 chars: {}",
                            endpoint,
                            e,
                            excerpt_chars(&body, 240)
                        ));
                        continue;
                    }
                };

                if let Some(error_obj) = payload.get("error") {
                    last_error = Some(format!(
                        "audio API returned error object from {}: {}",
                        endpoint,
                        excerpt_chars(&error_obj.to_string(), 240)
                    ));
                    continue;
                }

                if let Some(url) = payload
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    let usage_val = payload
                        .get("usage")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let usage = serde_json::from_value::<OpenAiAudioUsage>(usage_val).unwrap_or(
                        OpenAiAudioUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                        },
                    );
                    let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
                    let cost = estimate_cost(
                        usage.input_tokens,
                        usage.output_tokens,
                        0,
                        0,
                        &effective_model,
                    );
                    return Ok(PmAudioRunResult {
                        asset_url: url.to_string(),
                        audio_format: response_format.to_string(),
                        usage: PmUsageDto {
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            total_tokens,
                            estimated_cost_usd: cost,
                            model: effective_model.clone(),
                        },
                        api_key_id: entry.id.clone(),
                        provider_name: entry.provider.clone(),
                        provider_meta: Value::Null,
                    });
                }

                let b64_audio = payload
                    .get("audio")
                    .and_then(|v| v.get("data"))
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("b64_json").and_then(Value::as_str))
                    .or_else(|| {
                        payload
                            .get("data")
                            .and_then(Value::as_array)
                            .and_then(|arr| arr.first())
                            .and_then(|item| item.get("b64_json").and_then(Value::as_str))
                    });

                if let Some(b64) = b64_audio.map(str::trim).filter(|v| !v.is_empty()) {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(b64.as_bytes())
                        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64))
                        .map_err(|e| {
                            AppError::Internal(format!("decode generated audio base64 failed: {e}"))
                        })?;
                    let asset_url =
                        persist_generated_binary(state, user_id, &bytes, response_format)?;
                    let usage_val = payload
                        .get("usage")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let usage = serde_json::from_value::<OpenAiAudioUsage>(usage_val).unwrap_or(
                        OpenAiAudioUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                        },
                    );
                    let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
                    let cost = estimate_cost(
                        usage.input_tokens,
                        usage.output_tokens,
                        0,
                        0,
                        &effective_model,
                    );
                    return Ok(PmAudioRunResult {
                        asset_url,
                        audio_format: response_format.to_string(),
                        usage: PmUsageDto {
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            total_tokens,
                            estimated_cost_usd: cost,
                            model: effective_model.clone(),
                        },
                        api_key_id: entry.id.clone(),
                        provider_name: entry.provider.clone(),
                        provider_meta: Value::Null,
                    });
                }

                last_error = Some(format!(
                    "audio generation response missing playable payload from {}: {}",
                    endpoint,
                    excerpt_chars(&payload.to_string(), 240)
                ));
            }
        }
    }

    Err(AppError::Internal(last_error.unwrap_or_else(|| {
        "audio generation request failed".to_string()
    })))
}

fn mission_from_row(row: &sqlx::sqlite::SqliteRow) -> PmMissionRecord {
    PmMissionRecord {
        id: row.get::<i64, _>(0),
        mission_name: row.get::<String, _>(1),
        intent: row.get::<String, _>(2),
        country_code: row.get::<String, _>(3),
        schedule_cron: row.get::<Option<String>, _>(4),
        lookback_days: row.get::<i32, _>(5),
        max_sources: row.get::<i32, _>(6),
        max_signals_per_source: row.get::<i32, _>(7),
        auto_discovery: row.get::<bool, _>(8),
        enabled: row.get::<bool, _>(9),
        created_by: row.get::<Option<String>, _>(10),
        created_at: parse_dt_opt(row.get::<Option<String>, _>(11)).unwrap_or_default(),
        updated_at: parse_dt_opt(row.get::<Option<String>, _>(12)).unwrap_or_default(),
    }
}

#[derive(Debug)]
struct PmMaterialJobChainInfo {
    thread_id: Option<i64>,
    iteration_no: i32,
    asset_type: String,
}

#[derive(Debug, Clone)]
struct PmContinuationAssetContext {
    asset_type: String,
    url: Option<String>,
    content_text: Option<String>,
    meta: Value,
}

impl PmContinuationAssetLike for PmContinuationAssetContext {
    fn meta(&self) -> &Value {
        &self.meta
    }
}

impl PmContinuationContentLike for PmContinuationAssetContext {
    fn content_text(&self) -> Option<&str> {
        self.content_text.as_deref()
    }
}

async fn load_material_job_chain_info(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    job_id: i64,
) -> Result<Option<PmMaterialJobChainInfo>, AppError> {
    let row = sqlx::query(
        "SELECT CAST(thread_id AS INTEGER), CAST(iteration_no AS INTEGER), asset_type
         FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND id = ?
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(job_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| PmMaterialJobChainInfo {
        thread_id: r.get::<Option<i64>, _>(0),
        iteration_no: r.get::<i32, _>(1),
        asset_type: r.get::<String, _>(2),
    }))
}

async fn load_material_job_original_prompt(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    job_id: i64,
) -> Result<Option<String>, AppError> {
    let Some(chain) = load_material_job_chain_info(state, tenant_id, user_id, job_id).await? else {
        return Ok(None);
    };
    let thread_id = normalize_thread_id(chain.thread_id, job_id);
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT prompt_text
         FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ?
           AND thread_id = ?
           AND TRIM(prompt_text) <> ''
         ORDER BY iteration_no ASC, id ASC
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .fetch_optional(&state.db)
    .await?;

    Ok(row
        .map(|(prompt,)| prompt.trim().to_string())
        .filter(|prompt| !prompt.is_empty()))
}

async fn load_latest_job_iteration_in_thread(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: i64,
) -> Result<i32, AppError> {
    let row = sqlx::query_as::<_, (Option<i32>,)>(
        "SELECT CAST(MAX(iteration_no) AS INTEGER)
         FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .fetch_one(&state.db)
    .await?;
    Ok(row.0.unwrap_or(0))
}

async fn load_continuation_asset_context(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    asset_id: i64,
) -> Result<Option<PmContinuationAssetContext>, AppError> {
    let row = sqlx::query(
        "SELECT a.asset_type, a.url, a.content_text, CAST(a.meta_json AS TEXT)
         FROM pm_material_assets a
         INNER JOIN pm_material_jobs j ON j.id = a.job_id AND j.tenant_id = a.tenant_id
         WHERE a.tenant_id = ? AND j.created_by = ? AND a.id = ?
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(asset_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| PmContinuationAssetContext {
        asset_type: r.get::<String, _>(0),
        url: r.get::<Option<String>, _>(1),
        content_text: r.get::<Option<String>, _>(2),
        meta: parse_json_str(r.get::<Option<String>, _>(3))
            .unwrap_or(Value::Object(Default::default())),
    }))
}

async fn load_material_thread_text_context(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    thread_id: i64,
    current_job_id: i64,
    asset_type: &str,
) -> Result<Vec<PmMaterialThreadTextItem>, AppError> {
    let rows = sqlx::query(
        "SELECT CAST(j.iteration_no AS INTEGER), j.prompt_text, a.content_text, CAST(a.meta_json AS TEXT)
         FROM pm_material_jobs j
         INNER JOIN pm_material_assets a
            ON a.tenant_id = j.tenant_id AND a.job_id = j.id
         WHERE j.tenant_id = ? AND j.created_by = ?
           AND j.thread_id = ?
           AND j.id <> ?
           AND j.asset_type = ?
           AND a.content_text IS NOT NULL
           AND TRIM(a.content_text) <> ''
         ORDER BY j.iteration_no ASC, a.id ASC",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .bind(current_job_id)
    .bind(asset_type)
    .fetch_all(&state.db)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let content = row.get::<Option<String>, _>(2)?.trim().to_string();
            if content.is_empty() {
                return None;
            }
            let meta = parse_json_str(row.get::<Option<String>, _>(3))
                .unwrap_or(Value::Object(Default::default()));
            let workflow_stage = meta
                .get("workflowStage")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string);
            Some(PmMaterialThreadTextItem {
                iteration_no: row.get::<i32, _>(0),
                workflow_stage,
                prompt_text: row.get::<String, _>(1),
                content_text: content,
            })
        })
        .collect())
}

async fn load_material_job_thread_key(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    job_id: i64,
) -> Result<i64, AppError> {
    let row = sqlx::query_as::<_, (i64,)>(
        "SELECT CAST(COALESCE(thread_id, id) AS INTEGER)
         FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND id = ?
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(job_id)
    .fetch_one(&state.db)
    .await?;
    Ok(row.0)
}

fn spawn_material_job_completed_notification(
    state: AppState,
    tenant_id: String,
    job_id: i64,
    asset_type: String,
) {
    let text = format!(
        "素材任务已完成\n\n任务ID: {job_id}\n类型: {asset_type}\n\n请在 AOS 任务中心查看结果。"
    );
    tokio::spawn(async move {
        if !crate::routes::task_control_worker::legacy_capability_notifications_enabled(
            &state, &tenant_id,
        )
        .await
        {
            return;
        }
        match crate::routes::bot_agents_outbound::notify_capability_event(
            &state,
            &tenant_id,
            "materials",
            "materials.job_completed",
            crate::routes::bot_agents_outbound::BotOutboundMessage {
                title: Some("素材任务完成".to_string()),
                text,
                external_conversation_id: None,
            },
        )
        .await
        {
            Ok(summary) if summary.attempted > 0 => {
                tracing::info!(
                    tenant_id = %tenant_id,
                    job_id = job_id,
                    attempted = summary.attempted,
                    sent = summary.sent,
                    failed = summary.failed,
                    "material job completion bot notification dispatched"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    job_id = job_id,
                    "material job completion bot notification skipped: {}",
                    error
                );
            }
        }
    });
}

fn material_job_from_row(row: &sqlx::sqlite::SqliteRow) -> PmMaterialJobRecord {
    PmMaterialJobRecord {
        id: row.get::<i64, _>(0),
        mission_run_id: row.get::<Option<i64>, _>(1),
        thread_id: row.get::<Option<i64>, _>(2),
        parent_job_id: row.get::<Option<i64>, _>(3),
        iteration_no: row.get::<i32, _>(4),
        prompt_text: row.get::<String, _>(5),
        model: row.get::<Option<String>, _>(6),
        asset_type: row.get::<String, _>(7),
        status: row.get::<String, _>(8),
        result_count: row.get::<i32, _>(9),
        error_message: row.get::<Option<String>, _>(10),
        created_by: row.get::<Option<String>, _>(11),
        created_at: parse_dt_opt(row.get::<Option<String>, _>(12)).unwrap_or_default(),
        updated_at: parse_dt_opt(row.get::<Option<String>, _>(13)).unwrap_or_default(),
    }
}

fn material_thread_from_row(row: &sqlx::sqlite::SqliteRow) -> PmMaterialThreadRecord {
    PmMaterialThreadRecord {
        thread_id: row.get::<i64, _>(0),
        latest_job_id: row.get::<i64, _>(1),
        mission_run_id: row.get::<Option<i64>, _>(2),
        version_count: row.get::<i64, _>(3),
        latest_iteration_no: row.get::<i32, _>(4),
        prompt_text: row.get::<String, _>(5),
        model: row.get::<Option<String>, _>(6),
        asset_type: row.get::<String, _>(7),
        status: row.get::<String, _>(8),
        result_count: row.get::<i32, _>(9),
        error_message: row.get::<Option<String>, _>(10),
        created_by: row.get::<Option<String>, _>(11),
        created_at: parse_dt_opt(row.get::<Option<String>, _>(12)).unwrap_or_default(),
        updated_at: parse_dt_opt(row.get::<Option<String>, _>(13)).unwrap_or_default(),
    }
}

fn material_asset_from_row(row: &sqlx::sqlite::SqliteRow) -> PmMaterialAssetRecord {
    PmMaterialAssetRecord {
        id: row.get::<i64, _>(0),
        job_id: row.get::<i64, _>(1),
        asset_type: row.get::<String, _>(2),
        url: row.get::<Option<String>, _>(3),
        content_text: row.get::<Option<String>, _>(4),
        meta: parse_json_str(row.get::<Option<String>, _>(5))
            .unwrap_or(Value::Object(Default::default())),
        created_at: parse_dt_opt(row.get::<Option<String>, _>(6)).unwrap_or_default(),
    }
}

fn mission_task_run_from_row(row: &sqlx::sqlite::SqliteRow) -> PmMissionTaskRunRecord {
    PmMissionTaskRunRecord {
        task_id: row.get::<String, _>(0),
        status: row.get::<String, _>(1),
        stage: row.get::<Option<String>, _>(2),
        attempt: row.get::<Option<i32>, _>(3),
        elapsed_ms: row.get::<i64, _>(4),
        stage_elapsed_ms: row.get::<Option<i64>, _>(5),
        error_message: row.get::<Option<String>, _>(6),
        detail: parse_json_str(row.get::<Option<String>, _>(7)),
        response: parse_json_str(row.get::<Option<String>, _>(8)),
        created_at: parse_dt_opt(row.get::<Option<String>, _>(9)).unwrap_or_default(),
        updated_at: parse_dt_opt(row.get::<Option<String>, _>(10)).unwrap_or_default(),
        completed_at: parse_dt_opt(row.get::<Option<String>, _>(11)),
    }
}

fn mission_task_event_from_row(row: &sqlx::sqlite::SqliteRow) -> PmMissionTaskEventRecord {
    PmMissionTaskEventRecord {
        seq: row.get::<i64, _>(0),
        status: row.get::<String, _>(1),
        stage: row.get::<Option<String>, _>(2),
        attempt: row.get::<Option<i32>, _>(3),
        message: row.get::<Option<String>, _>(4),
        elapsed_ms: row.get::<i64, _>(5),
        stage_elapsed_ms: row.get::<Option<i64>, _>(6),
        detail: parse_json_str(row.get::<Option<String>, _>(7)),
        response: parse_json_str(row.get::<Option<String>, _>(8)),
        error_message: row.get::<Option<String>, _>(9),
        created_at: parse_dt_opt(row.get::<Option<String>, _>(10)).unwrap_or_default(),
    }
}

fn mission_task_id_prefix(mission_id: i64) -> String {
    format!("pm-mission-{mission_id}-")
}

fn mission_execution_task_id(
    tenant_id: &str,
    user_id: &str,
    mission_id: i64,
    execution_key: Option<&str>,
) -> String {
    let suffix = execution_key
        .map(|key| {
            let digest =
                Sha256::digest(format!("{tenant_id}\0{user_id}\0{mission_id}\0{key}").as_bytes());
            hex::encode(digest).chars().take(32).collect::<String>()
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    format!("{}{suffix}", mission_task_id_prefix(mission_id))
}

#[cfg(test)]
mod mission_execution_id_tests {
    use super::{latest_due_mission_occurrence, mission_execution_task_id, parse_schedule_cron};
    use chrono::{TimeZone, Utc};

    #[test]
    fn scheduled_mission_execution_ids_are_stable_and_tenant_scoped() {
        let first =
            mission_execution_task_id("tenant-a", "user-a", 42, Some("2026-07-26T10:00:00+08:00"));
        let replay =
            mission_execution_task_id("tenant-a", "user-a", 42, Some("2026-07-26T10:00:00+08:00"));
        let other_tenant =
            mission_execution_task_id("tenant-b", "user-a", 42, Some("2026-07-26T10:00:00+08:00"));
        assert_eq!(first, replay);
        assert_ne!(first, other_tenant);
    }

    #[test]
    fn manual_mission_runs_create_distinct_attempt_ids() {
        let first = mission_execution_task_id("tenant-a", "user-a", 42, None);
        let second = mission_execution_task_id("tenant-a", "user-a", 42, None);
        assert_ne!(first, second);
    }

    #[test]
    fn scheduler_does_not_replay_days_old_missed_occurrences() {
        let schedule = parse_schedule_cron("0 0 9 * * *").expect("valid daily schedule");
        let anchor = Utc
            .with_ymd_and_hms(2026, 7, 24, 5, 6, 23)
            .single()
            .expect("valid anchor");
        let now = Utc
            .with_ymd_and_hms(2026, 7, 27, 2, 41, 35)
            .single()
            .expect("valid now");

        assert_eq!(
            latest_due_mission_occurrence(&schedule, anchor, now, 300),
            None
        );
    }

    #[test]
    fn scheduler_coalesces_recent_misfires_to_the_latest_occurrence() {
        let schedule = parse_schedule_cron("* * * * *").expect("valid minute schedule");
        let anchor = Utc
            .with_ymd_and_hms(2026, 7, 27, 9, 0, 0)
            .single()
            .expect("valid anchor");
        let now = Utc
            .with_ymd_and_hms(2026, 7, 27, 9, 4, 20)
            .single()
            .expect("valid now");

        assert_eq!(
            latest_due_mission_occurrence(&schedule, anchor, now, 300),
            Utc.with_ymd_and_hms(2026, 7, 27, 9, 4, 0).single()
        );
    }
}

const PM_MISSION_INTERNAL_SESSION_SOURCE: &str = "pm_internal_mission";

fn mission_task_id_like(mission_id: i64) -> String {
    format!("{}%", mission_task_id_prefix(mission_id))
}

async fn resolve_latest_mission_session_id(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    mission_id: i64,
) -> Result<Option<String>, AppError> {
    let task_like = mission_task_id_like(mission_id);
    let row = sqlx::query(
        "SELECT session_id
         FROM pm_research_tasks
         WHERE tenant_id = ? AND user_id = ? AND task_id LIKE ?
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(task_like)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| r.get::<String, _>(0)))
}

async fn ensure_mission_runtime_session_id(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    mission_id: i64,
) -> Result<String, AppError> {
    if let Some(existing_id) =
        resolve_latest_mission_session_id(state, tenant_id, user_id, mission_id).await?
    {
        if let Some(handle) = state.agent_manager().get_session(&existing_id).await {
            if handle.tenant_id == tenant_id
                && handle.user_id == user_id
                && handle.source == PM_MISSION_INTERNAL_SESSION_SOURCE
            {
                return Ok(existing_id);
            }
        }
    }

    let handle = state
        .agent_manager()
        .create_session(
            user_id,
            tenant_id,
            None,
            None,
            PM_MISSION_INTERNAL_SESSION_SOURCE,
            Some("chat"),
            None,
            None,
        )
        .await
        .map_err(|e| AppError::Internal(format!("create mission runtime failed: {e}")))?;
    Ok(handle.session_id)
}

async fn enqueue_mission_pm_task(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    mission_id: i64,
    mission_name: &str,
    session_id: &str,
    intent: &str,
    trigger: &str,
    execution_key: Option<&str>,
) -> Result<String, AppError> {
    #[cfg(feature = "bot-agents")]
    if let Some(command) =
        crate::routes::super_assistant::parse_super_assistant_slash_command(intent)
    {
        if command.prompt.is_empty() {
            return Err(AppError::ValidationError(
                "slash command requires a task description".to_string(),
            ));
        }
        if command.mode == crate::routes::super_assistant::SuperAssistantSlashMode::SuperAdversarial
        {
            crate::routes::agent::default_chat_adversarial_models(state, tenant_id).await?;
        }
    }

    let task_id = mission_execution_task_id(tenant_id, user_id, mission_id, execution_key);
    let detail_json = serde_json::json!({
        "missionId": mission_id,
        "missionName": mission_name,
        "trigger": trigger,
        "executionKey": execution_key,
        "executionEngine": "super_assistant",
        "parentTurnId": task_id,
    })
    .to_string();

    let mut tx = state.db.begin().await?;
    // Keep the legacy PM row terminal so its recovery worker cannot claim the
    // same mission. The task-center APIs project the live parent-turn status.
    sqlx::query(
        "INSERT INTO pm_research_tasks
            (task_id, tenant_id, user_id, session_id, message, status, stage, attempt,
             elapsed_ms, stage_elapsed_ms, detail_json, response_json, error_message,
             cancel_requested, event_seq, checkpoint_json, completed_at)
         VALUES (?, ?, ?, ?, ?, 'completed', 'super_assistant', 1, 0, 0, ?, NULL, NULL, 0, 1, NULL, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            status = excluded.status,
            stage = excluded.stage,
            attempt = excluded.attempt,
            elapsed_ms = excluded.elapsed_ms,
            stage_elapsed_ms = excluded.stage_elapsed_ms,
            detail_json = excluded.detail_json,
            response_json = excluded.response_json,
            error_message = excluded.error_message,
            cancel_requested = excluded.cancel_requested,
            event_seq = excluded.event_seq,
            checkpoint_json = excluded.checkpoint_json,
            completed_at = excluded.completed_at,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&task_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(intent.trim())
    .bind(detail_json.clone())
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO pm_research_task_events
            (task_id, tenant_id, user_id, seq, status, stage, attempt, message,
             elapsed_ms, stage_elapsed_ms, detail_json, response_json, error_message)
         VALUES (?, ?, ?, 1, 'queued', 'queued', 1, ?, 0, 0, ?, NULL, NULL)
         ON CONFLICT DO UPDATE SET
            status = excluded.status,
            stage = excluded.stage,
            attempt = excluded.attempt,
            message = excluded.message,
            elapsed_ms = excluded.elapsed_ms,
            stage_elapsed_ms = excluded.stage_elapsed_ms,
            detail_json = excluded.detail_json,
            response_json = excluded.response_json,
            error_message = excluded.error_message",
    )
    .bind(&task_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(format!("任务「{}」已交给超级助手", mission_name))
    .bind(detail_json)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    #[cfg(feature = "bot-agents")]
    {
        let user = sqlx::query(
            "SELECT email, role FROM users
             WHERE tenant_id = ? AND id = ? AND is_active = 1 LIMIT 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;
        let claims = Claims::new(
            user_id,
            &user.get::<String, _>("email"),
            &user.get::<String, _>("role"),
            tenant_id,
        );
        let enqueue_result =
            crate::routes::super_assistant::enqueue_background_super_assistant_turn(
                state.clone(),
                claims,
                crate::routes::super_assistant::SuperAssistantMessageRequest {
                    session_id: session_id.to_string(),
                    text: intent.trim().to_string(),
                    images: Vec::new(),
                    documents: Vec::new(),
                    display_text: None,
                    turn_id: Some(task_id.clone()),
                    explicit_capability: None,
                    app: Some("chat".to_string()),
                    model: None,
                    data_source_id: None,
                    data_attribution: false,
                    router_config: None,
                },
            )
            .await;
        if let Err(error) = enqueue_result {
            sqlx::query(
                "UPDATE pm_research_tasks
                 SET status = 'failed', stage = 'super_assistant', error_message = ?,
                     completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND user_id = ? AND task_id = ?",
            )
            .bind(error.to_string())
            .bind(tenant_id)
            .bind(user_id)
            .bind(&task_id)
            .execute(&state.db)
            .await?;
            return Err(error);
        }
        let agent_task_id = sqlx::query_scalar::<_, String>(
            "SELECT id FROM agent_tasks
             WHERE tenant_id = ? AND owner_user_id = ?
               AND source = 'super_assistant' AND source_ref = ?
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&task_id)
        .fetch_optional(&state.db)
        .await?;
        if let Some(agent_task_id) = agent_task_id {
            for (resource_type, resource_id) in [
                ("pm_mission", mission_id.to_string()),
                ("pm_mission_run", task_id.clone()),
            ] {
                if let Err(error) = crate::routes::agent_ops::link_task_secondary_resource(
                    state,
                    tenant_id,
                    &agent_task_id,
                    resource_type,
                    &resource_id,
                )
                .await
                {
                    tracing::error!(
                        tenant_id,
                        user_id,
                        mission_id,
                        task_id,
                        agent_task_id,
                        resource_type,
                        error = %error,
                        error_debug = ?error,
                        "mission turn was accepted but secondary AgentOps lineage failed"
                    );
                }
            }
        } else {
            tracing::warn!(
                tenant_id,
                user_id,
                mission_id,
                task_id,
                "mission turn completed through a task-control fast path without creating a new AgentOps root"
            );
        }
    }
    #[cfg(not(feature = "bot-agents"))]
    {
        sqlx::query(
            "UPDATE pm_research_tasks
             SET status = 'failed', error_message = 'unified super assistant is not enabled',
                 completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND task_id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&task_id)
        .execute(&state.db)
        .await?;
        return Err(AppError::ValidationError(
            "task center requires the bot-agents feature".to_string(),
        ));
    }

    Ok(task_id)
}

async fn run_mission_once(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    mission_id: i64,
    mission_name: &str,
    intent: &str,
    trigger: &str,
    execution_key: Option<&str>,
) -> Result<String, AppError> {
    let session_id =
        ensure_mission_runtime_session_id(state, tenant_id, user_id, mission_id).await?;
    let task_id = enqueue_mission_pm_task(
        state,
        tenant_id,
        user_id,
        mission_id,
        mission_name,
        &session_id,
        intent,
        trigger,
        execution_key,
    )
    .await?;
    Ok(task_id)
}

pub async fn dispatch_due_pm_missions(state: &AppState, tenant_id: &str) -> Result<(), AppError> {
    let rows = sqlx::query(
        "SELECT CAST(id AS INTEGER), mission_name, intent, schedule_cron, created_by, created_at
         FROM pm_missions
         WHERE tenant_id = ? AND enabled = 1
           AND schedule_cron IS NOT NULL AND TRIM(schedule_cron) <> ''",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    let misfire_grace_seconds = mission_misfire_grace_seconds();
    for row in rows {
        let mission_id = row.get::<i64, _>(0);
        let mission_name = row.get::<String, _>(1);
        let intent = row.get::<String, _>(2);
        let schedule_cron = row.get::<String, _>(3);
        let created_by = row
            .get::<Option<String>, _>(4)
            .unwrap_or_default()
            .trim()
            .to_string();
        let mission_created_at = row.get::<NaiveDateTime, _>(5).and_utc();

        if created_by.is_empty() {
            tracing::warn!(
                mission_id,
                mission_name = %mission_name,
                tenant_id = %tenant_id,
                "skip scheduled mission because created_by is empty"
            );
            continue;
        }

        let schedule = match parse_schedule_cron(&schedule_cron) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    mission_id,
                    mission_name = %mission_name,
                    tenant_id = %tenant_id,
                    schedule_cron = %schedule_cron,
                    "skip scheduled mission due to invalid cron expression"
                );
                continue;
            }
        };

        let anchor_row = sqlx::query(
            "SELECT created_at
             FROM pm_research_tasks
             WHERE tenant_id = ? AND task_id LIKE ?
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(tenant_id)
        .bind(mission_task_id_like(mission_id))
        .fetch_optional(&state.db)
        .await?;

        let anchor = anchor_row
            .map(|r| r.get::<NaiveDateTime, _>(0).and_utc())
            .unwrap_or(mission_created_at);

        let Some(next_due_at) =
            latest_due_mission_occurrence(&schedule, anchor, now, misfire_grace_seconds)
        else {
            continue;
        };

        let execution_key = next_due_at.to_rfc3339();
        if let Err(error) = run_mission_once(
            state,
            tenant_id,
            &created_by,
            mission_id,
            &mission_name,
            &intent,
            "schedule",
            Some(&execution_key),
        )
        .await
        {
            tracing::warn!(
                mission_id,
                mission_name = %mission_name,
                tenant_id = %tenant_id,
                user_id = %created_by,
                error = %error,
                "scheduled mission enqueue failed"
            );
        }
    }

    Ok(())
}

async fn list_missions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmMissionQuery>,
) -> Result<Json<PmListResponse<PmMissionRecord>>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = i64::from(per_page);
    let offset = i64::from((page.saturating_sub(1)) * per_page);
    let country_code = query
        .country_code
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    let has_country = !country_code.is_empty();

    let mut count_sql =
        String::from("SELECT COUNT(*) FROM pm_missions WHERE tenant_id = ? AND created_by = ?");
    let mut list_sql = String::from(
        "SELECT CAST(id AS INTEGER), mission_name, intent, country_code, schedule_cron, lookback_days, max_sources, max_signals_per_source,
                auto_discovery, enabled, created_by, CAST(created_at AS TEXT), CAST(updated_at AS TEXT)
         FROM pm_missions WHERE tenant_id = ? AND created_by = ?",
    );
    if has_country {
        count_sql.push_str(" AND country_code = ?");
        list_sql.push_str(" AND country_code = ?");
    }
    if query.enabled.is_some() {
        count_sql.push_str(" AND enabled = ?");
        list_sql.push_str(" AND enabled = ?");
    }
    list_sql.push_str(" ORDER BY updated_at DESC LIMIT ? OFFSET ?");

    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    let mut list_q = sqlx::query(&list_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    if has_country {
        count_q = count_q.bind(&country_code);
        list_q = list_q.bind(&country_code);
    }
    if let Some(enabled) = query.enabled {
        count_q = count_q.bind(enabled);
        list_q = list_q.bind(enabled);
    }
    let total = count_q.fetch_one(&state.db).await?.0;
    let rows = list_q.bind(limit).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(PmListResponse {
        items: rows.iter().map(mission_from_row).collect(),
        total,
    }))
}

async fn mission_summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<PmMissionSummaryResponse>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;

    let mission_row = sqlx::query(
        "SELECT CAST(COUNT(*) AS INTEGER),
                CAST(COALESCE(SUM(CASE WHEN enabled = 1 THEN 1 ELSE 0 END), 0) AS INTEGER),
                CAST(COALESCE(SUM(CASE WHEN enabled = 0 THEN 1 ELSE 0 END), 0) AS INTEGER)
         FROM pm_missions
         WHERE tenant_id = ? AND created_by = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;

    let run_row = sqlx::query(
        "SELECT
            CAST(COALESCE(SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'cancelling' THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'completed' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'failed' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'cancelled' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(AVG(CASE WHEN status IN ('completed', 'failed', 'cancelled') AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN elapsed_ms ELSE NULL END) AS INTEGER),
            CAST(MAX(created_at) AS TEXT)
         FROM (
           SELECT p.created_at,
                  CASE
                    WHEN JSON_EXTRACT(p.detail_json, '$.executionEngine') = 'super_assistant'
                      THEN CASE
                        WHEN sa.status IN ('queued') THEN 'queued'
                        WHEN sa.status IN ('completed', 'failed', 'cancelled') THEN sa.status
                        ELSE 'running'
                      END
                    ELSE p.status
                  END AS status,
                  CASE
                    WHEN sa.turn_id IS NOT NULL THEN ((julianday(COALESCE(sa.completed_at, CURRENT_TIMESTAMP)) - julianday(p.created_at)) * 86400000000) / 1000
                    ELSE p.elapsed_ms
                  END AS elapsed_ms
           FROM pm_research_tasks p
           LEFT JOIN super_assistant_turns sa
             ON sa.tenant_id = p.tenant_id AND sa.user_id = p.user_id
            AND sa.session_id = p.session_id AND sa.turn_id = p.task_id
           WHERE p.tenant_id = ? AND p.user_id = ? AND p.task_id LIKE 'pm-mission-%'
         ) mission_runs",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;

    let completed_runs_30d = run_row.get::<i64, _>(3);
    let failed_runs_30d = run_row.get::<i64, _>(4);
    let cancelled_runs_30d = run_row.get::<i64, _>(5);
    let terminal_runs_30d = completed_runs_30d + failed_runs_30d + cancelled_runs_30d;
    let success_rate_30d = if terminal_runs_30d > 0 {
        completed_runs_30d as f64 / terminal_runs_30d as f64
    } else {
        0.0
    };

    Ok(Json(PmMissionSummaryResponse {
        total_missions: mission_row.get::<i64, _>(0),
        enabled_missions: mission_row.get::<i64, _>(1),
        disabled_missions: mission_row.get::<i64, _>(2),
        queued_runs: run_row.get::<i64, _>(0),
        running_runs: run_row.get::<i64, _>(1),
        cancelling_runs: run_row.get::<i64, _>(2),
        completed_runs_30d,
        failed_runs_30d,
        cancelled_runs_30d,
        success_rate_30d,
        avg_elapsed_ms_30d: run_row.get::<Option<i64>, _>(6),
        latest_run_at: parse_dt_opt(run_row.get::<Option<String>, _>(7)),
    }))
}

async fn create_mission(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PmMissionCreateRequest>,
) -> Result<Json<PmMissionRecord>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let mission_name = req.mission_name.trim().to_string();
    let intent = req.intent.trim().to_string();
    let country_code = req
        .country_code
        .as_deref()
        .unwrap_or("GLOBAL")
        .trim()
        .to_ascii_uppercase();
    let country_code = if country_code.is_empty() {
        "GLOBAL".to_string()
    } else {
        country_code
    };
    if mission_name.is_empty() || intent.is_empty() {
        return Err(AppError::ValidationError(
            "mission_name and intent are required".to_string(),
        ));
    }
    if let Some(cron) = req.schedule_cron.as_ref().filter(|v| !v.trim().is_empty()) {
        parse_schedule_cron(cron)?;
    }
    let id = sqlx::query(
        "INSERT INTO pm_missions
            (tenant_id, mission_name, intent, country_code, schedule_cron, lookback_days, max_sources, max_signals_per_source, auto_discovery, enabled, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&claims.tenant_id)
    .bind(mission_name)
    .bind(intent)
    .bind(country_code)
    .bind(req.schedule_cron.as_ref().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
    .bind(req.lookback_days.unwrap_or(7).clamp(1, 90))
    .bind(req.max_sources.unwrap_or(4).clamp(1, 20))
    .bind(req.max_signals_per_source.unwrap_or(5).clamp(1, 50))
    .bind(req.auto_discovery.unwrap_or(true))
    .bind(req.enabled.unwrap_or(true))
    .bind(&claims.sub)
    .execute(&state.db)
    .await?
    .last_insert_rowid();
    let row = sqlx::query(
        "SELECT CAST(id AS INTEGER), mission_name, intent, country_code, schedule_cron, lookback_days, max_sources, max_signals_per_source,
                auto_discovery, enabled, created_by, CAST(created_at AS TEXT), CAST(updated_at AS TEXT)
         FROM pm_missions WHERE id = ? AND tenant_id = ? AND created_by = ?",
    )
    .bind(id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(mission_from_row(&row)))
}

async fn update_mission(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
    Json(req): Json<PmMissionUpdateRequest>,
) -> Result<Json<PmMissionRecord>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let mut set = Vec::new();
    if req.mission_name.is_some() {
        set.push("mission_name = ?");
    }
    if req.intent.is_some() {
        set.push("intent = ?");
    }
    if req.country_code.is_some() {
        set.push("country_code = ?");
    }
    if req.schedule_cron.is_some() {
        set.push("schedule_cron = ?");
    }
    if req.lookback_days.is_some() {
        set.push("lookback_days = ?");
    }
    if req.max_sources.is_some() {
        set.push("max_sources = ?");
    }
    if req.max_signals_per_source.is_some() {
        set.push("max_signals_per_source = ?");
    }
    if req.auto_discovery.is_some() {
        set.push("auto_discovery = ?");
    }
    if req.enabled.is_some() {
        set.push("enabled = ?");
    }
    if set.is_empty() {
        return Err(AppError::ValidationError("no fields to update".to_string()));
    }
    let sql = format!(
        "UPDATE pm_missions SET {} WHERE id = ? AND tenant_id = ? AND created_by = ?",
        set.join(", ")
    );
    let mut q = sqlx::query(&sql);
    if let Some(v) = req.mission_name {
        q = q.bind(trim_text(v.trim(), 128));
    }
    if let Some(v) = req.intent {
        q = q.bind(v.trim().to_string());
    }
    if let Some(v) = req.country_code {
        let normalized = {
            let c = v.trim().to_ascii_uppercase();
            if c.is_empty() {
                "GLOBAL".to_string()
            } else {
                c
            }
        };
        q = q.bind(normalized);
    }
    if let Some(v) = req.schedule_cron {
        let normalized = if v.trim().is_empty() {
            None
        } else {
            parse_schedule_cron(v.trim())?;
            Some(v.trim().to_string())
        };
        q = q.bind(normalized);
    }
    if let Some(v) = req.lookback_days {
        q = q.bind(v.clamp(1, 90));
    }
    if let Some(v) = req.max_sources {
        q = q.bind(v.clamp(1, 20));
    }
    if let Some(v) = req.max_signals_per_source {
        q = q.bind(v.clamp(1, 50));
    }
    if let Some(v) = req.auto_discovery {
        q = q.bind(v);
    }
    if let Some(v) = req.enabled {
        q = q.bind(v);
    }
    q.bind(id)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .execute(&state.db)
        .await?;
    let row = sqlx::query(
        "SELECT CAST(id AS INTEGER), mission_name, intent, country_code, schedule_cron, lookback_days, max_sources, max_signals_per_source,
                auto_discovery, enabled, created_by, CAST(created_at AS TEXT), CAST(updated_at AS TEXT)
         FROM pm_missions WHERE id = ? AND tenant_id = ? AND created_by = ?",
    )
    .bind(id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound("mission not found".to_string()));
    };
    Ok(Json(mission_from_row(&row)))
}

async fn delete_mission(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let result =
        sqlx::query("DELETE FROM pm_missions WHERE id = ? AND tenant_id = ? AND created_by = ?")
            .bind(id)
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .execute(&state.db)
            .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("mission not found".to_string()));
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn run_mission_now(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> Result<Json<PmMissionRunNowResponse>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let row_opt = sqlx::query(
        "SELECT mission_name, intent
         FROM pm_missions
         WHERE id = ? AND tenant_id = ? AND created_by = ?",
    )
    .bind(id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row_opt else {
        return Err(AppError::NotFound("mission not found".to_string()));
    };
    let mission_name = row.get::<String, _>(0);
    let intent = row.get::<String, _>(1);
    if intent.trim().is_empty() {
        return Err(AppError::ValidationError(
            "mission intent cannot be empty".to_string(),
        ));
    }
    let task_id = run_mission_once(
        &state,
        &claims.tenant_id,
        &claims.sub,
        id,
        &mission_name,
        &intent,
        "manual",
        None,
    )
    .await?;
    Ok(Json(PmMissionRunNowResponse {
        mission_id: id,
        task_id,
        status: "queued".to_string(),
    }))
}

async fn list_mission_task_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
    Query(query): Query<PmMissionTaskRunQuery>,
) -> Result<Json<PmListResponse<PmMissionTaskRunRecord>>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let exists = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM pm_missions WHERE id = ? AND tenant_id = ? AND created_by = ?",
    )
    .bind(id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?
    .0;
    if exists == 0 {
        return Err(AppError::NotFound("mission not found".to_string()));
    }

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = i64::from(per_page);
    let offset = i64::from((page.saturating_sub(1)) * per_page);
    let task_like = mission_task_id_like(id);

    let status_filter = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let effective_status = "CASE
        WHEN JSON_EXTRACT(p.detail_json, '$.executionEngine') = 'super_assistant'
          THEN CASE
            WHEN sa.status = 'queued' THEN 'queued'
            WHEN sa.status IN ('completed', 'failed', 'cancelled') THEN sa.status
            ELSE 'running'
          END
        ELSE p.status
      END";
    let mut count_sql = format!(
        "SELECT COUNT(*)
         FROM pm_research_tasks p
         LEFT JOIN super_assistant_turns sa
           ON sa.tenant_id = p.tenant_id AND sa.user_id = p.user_id
          AND sa.session_id = p.session_id AND sa.turn_id = p.task_id
         WHERE p.tenant_id = ? AND p.user_id = ? AND p.task_id LIKE ?"
    );
    let mut list_sql = String::from(
        "SELECT p.task_id,
                CASE
                  WHEN JSON_EXTRACT(p.detail_json, '$.executionEngine') = 'super_assistant'
                    THEN CASE
                      WHEN sa.status = 'queued' THEN 'queued'
                      WHEN sa.status IN ('completed', 'failed', 'cancelled') THEN sa.status
                      ELSE 'running'
                    END
                  ELSE p.status
                END AS status,
                CASE WHEN sa.turn_id IS NOT NULL THEN sa.status ELSE p.stage END AS stage,
                CAST(COALESCE(sa.attempt, p.attempt) AS INTEGER) AS attempt,
                CAST(CASE
                  WHEN sa.turn_id IS NOT NULL THEN ((julianday(COALESCE(sa.completed_at, CURRENT_TIMESTAMP)) - julianday(p.created_at)) * 86400000000) / 1000
                  ELSE p.elapsed_ms
                END AS INTEGER) AS elapsed_ms,
                CAST(p.stage_elapsed_ms AS INTEGER), COALESCE(sa.error, p.error_message) AS error_message,
                CAST(p.detail_json AS TEXT),
                CAST(CASE
                  WHEN sa.turn_id IS NOT NULL THEN JSON_OBJECT('text', sa.final_text, 'turnId', sa.turn_id, 'status', sa.status)
                  ELSE p.response_json
                END AS TEXT) AS response_json,
                CAST(p.created_at AS TEXT),
                CAST(CASE WHEN sa.turn_id IS NOT NULL THEN MAX(p.updated_at, sa.updated_at) ELSE p.updated_at END AS TEXT),
                CAST(CASE WHEN sa.turn_id IS NOT NULL THEN sa.completed_at ELSE p.completed_at END AS TEXT)
         FROM pm_research_tasks p
         LEFT JOIN super_assistant_turns sa
           ON sa.tenant_id = p.tenant_id AND sa.user_id = p.user_id
          AND sa.session_id = p.session_id AND sa.turn_id = p.task_id
         WHERE p.tenant_id = ? AND p.user_id = ? AND p.task_id LIKE ?",
    );
    if status_filter.is_some() {
        count_sql.push_str(&format!(" AND ({effective_status}) = ?"));
        list_sql.push_str(&format!(" AND ({effective_status}) = ?"));
    }
    list_sql.push_str(" ORDER BY p.updated_at DESC LIMIT ? OFFSET ?");

    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&task_like);
    let mut list_q = sqlx::query(&list_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&task_like);
    if let Some(status) = status_filter {
        count_q = count_q.bind(status);
        list_q = list_q.bind(status);
    }

    let total = count_q.fetch_one(&state.db).await?.0;
    let rows = list_q.bind(limit).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(PmListResponse {
        items: rows.iter().map(mission_task_run_from_row).collect(),
        total,
    }))
}

async fn list_mission_task_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((mission_id, task_id)): Path<(i64, String)>,
    Query(query): Query<PmMissionTaskEventQuery>,
) -> Result<Json<PmListResponse<PmMissionTaskEventRecord>>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let prefix = mission_task_id_prefix(mission_id);
    if !task_id.starts_with(&prefix) {
        return Err(AppError::ValidationError(
            "task_id does not belong to mission".to_string(),
        ));
    }

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 200);
    let limit = i64::from(per_page);
    let offset = i64::from((page.saturating_sub(1)) * per_page);

    let delegated = sqlx::query(
        "SELECT user_id, session_id, CAST(detail_json AS TEXT) AS detail_json
         FROM pm_research_tasks
         WHERE tenant_id = ? AND user_id = ? AND task_id = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&task_id)
    .fetch_optional(&state.db)
    .await?;
    if let Some(task) = delegated {
        let detail = task
            .get::<Option<String>, _>("detail_json")
            .and_then(|value| serde_json::from_str::<Value>(&value).ok());
        if detail
            .as_ref()
            .and_then(|value| value.get("executionEngine"))
            .and_then(Value::as_str)
            == Some("super_assistant")
        {
            let owner_user_id = task.get::<String, _>("user_id");
            let session_id = task.get::<String, _>("session_id");
            let total = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM super_assistant_turn_events
                 WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND turn_id = ?",
            )
            .bind(&claims.tenant_id)
            .bind(&owner_user_id)
            .bind(&session_id)
            .bind(&task_id)
            .fetch_one(&state.db)
            .await?;
            let rows = sqlx::query(
                "SELECT CAST(seq AS INTEGER) AS seq, event_type,
                        CAST(event_data AS TEXT) AS event_data,
                        CAST(created_at AS TEXT) AS created_at
                 FROM super_assistant_turn_events
                 WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND turn_id = ?
                 ORDER BY seq DESC LIMIT ? OFFSET ?",
            )
            .bind(&claims.tenant_id)
            .bind(&owner_user_id)
            .bind(&session_id)
            .bind(&task_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&state.db)
            .await?;
            let items = rows
                .into_iter()
                .map(|row| {
                    let event_type = row.get::<String, _>("event_type");
                    let data = row
                        .get::<Option<String>, _>("event_data")
                        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
                        .unwrap_or(Value::Null);
                    let status = data
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| match event_type.as_str() {
                            "turn_completed" => "completed".to_string(),
                            "turn_failed" => "failed".to_string(),
                            "turn_cancelled" => "cancelled".to_string(),
                            "turn_queued" => "queued".to_string(),
                            _ => "running".to_string(),
                        });
                    let message = ["text", "message", "humanSummary", "error"]
                        .iter()
                        .find_map(|key| data.get(*key).and_then(Value::as_str))
                        .map(str::to_string);
                    let response = matches!(event_type.as_str(), "final_delta" | "turn_completed")
                        .then(|| data.clone());
                    PmMissionTaskEventRecord {
                        seq: row.get::<i64, _>("seq"),
                        status,
                        stage: data
                            .get("stage")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(event_type.clone())),
                        attempt: data
                            .get("attempt")
                            .and_then(Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        message,
                        elapsed_ms: data.get("elapsedMs").and_then(Value::as_i64).unwrap_or(0),
                        stage_elapsed_ms: data.get("stageElapsedMs").and_then(Value::as_i64),
                        detail: Some(data),
                        response,
                        error_message: None,
                        created_at: row.get::<String, _>("created_at"),
                    }
                })
                .collect();
            return Ok(Json(PmListResponse { items, total }));
        }
    }

    let total = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*)
         FROM pm_research_task_events
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&task_id)
    .fetch_one(&state.db)
    .await?
    .0;

    let rows = sqlx::query(
        "SELECT CAST(seq AS INTEGER), status, stage, attempt, message,
                CAST(elapsed_ms AS INTEGER), CAST(stage_elapsed_ms AS INTEGER),
                CAST(detail_json AS TEXT), CAST(response_json AS TEXT), error_message,
                CAST(created_at AS TEXT)
         FROM pm_research_task_events
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?
         ORDER BY id DESC
         LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&task_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(PmListResponse {
        items: rows.iter().map(mission_task_event_from_row).collect(),
        total,
    }))
}

async fn preview_mission_cron(
    Query(query): Query<PmCronPreviewQuery>,
) -> Result<Json<PmCronPreviewResponse>, AppError> {
    let schedule_raw = query.schedule_cron.trim().to_string();
    let normalized = normalize_cron_for_parser(&schedule_raw)?;
    let schedule = cron::Schedule::from_str(&normalized)
        .map_err(|_| AppError::ValidationError("invalid schedule_cron".to_string()))?;
    let count = query.count.unwrap_or(7).clamp(1, 20) as usize;
    let next_runs = schedule
        .after(&Utc::now())
        .take(count)
        .map(|dt| dt.to_rfc3339())
        .collect();
    Ok(Json(PmCronPreviewResponse {
        schedule_cron: schedule_raw,
        normalized_cron: normalized,
        next_runs,
    }))
}

async fn list_material_jobs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmMaterialJobQuery>,
) -> Result<Json<PmListResponse<PmMaterialJobRecord>>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = i64::from(per_page);
    let offset = i64::from((page.saturating_sub(1)) * per_page);
    let mut count_sql = format!(
        "SELECT COUNT(*) FROM pm_material_jobs WHERE tenant_id = ? AND created_by = ? AND asset_type IN ({PUBLIC_MATERIAL_ASSET_TYPES_SQL})",
    );
    let mut list_sql = format!(
        "SELECT CAST(id AS INTEGER), CAST(mission_run_id AS INTEGER), CAST(thread_id AS INTEGER), CAST(parent_job_id AS INTEGER),
                CAST(iteration_no AS INTEGER), prompt_text, model, asset_type, status, result_count,
                error_message, created_by, CAST(created_at AS TEXT), CAST(updated_at AS TEXT)
         FROM pm_material_jobs WHERE tenant_id = ? AND created_by = ? AND asset_type IN ({PUBLIC_MATERIAL_ASSET_TYPES_SQL})",
    );
    if query.mission_run_id.is_some() {
        count_sql.push_str(" AND mission_run_id = ?");
        list_sql.push_str(" AND mission_run_id = ?");
    }
    if query.thread_id.is_some() {
        count_sql.push_str(" AND COALESCE(thread_id, id) = ?");
        list_sql.push_str(" AND COALESCE(thread_id, id) = ?");
    }
    if query
        .asset_type
        .as_ref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        count_sql.push_str(" AND asset_type = ?");
        list_sql.push_str(" AND asset_type = ?");
    }
    if query
        .status
        .as_ref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        count_sql.push_str(" AND status = ?");
        list_sql.push_str(" AND status = ?");
    }
    list_sql.push_str(" ORDER BY id DESC LIMIT ? OFFSET ?");
    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    let mut list_q = sqlx::query(&list_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    if let Some(mission_run_id) = query.mission_run_id {
        count_q = count_q.bind(mission_run_id);
        list_q = list_q.bind(mission_run_id);
    }
    if let Some(thread_id) = query.thread_id {
        count_q = count_q.bind(thread_id);
        list_q = list_q.bind(thread_id);
    }
    if let Some(asset_type) = query.asset_type.filter(|v| !v.trim().is_empty()) {
        if !is_public_material_asset_type(asset_type.trim()) {
            return Err(AppError::ValidationError(public_material_asset_type_error()));
        }
        count_q = count_q.bind(asset_type.clone());
        list_q = list_q.bind(asset_type);
    }
    if let Some(status) = query.status.filter(|v| !v.trim().is_empty()) {
        count_q = count_q.bind(status.clone());
        list_q = list_q.bind(status);
    }
    let total = count_q.fetch_one(&state.db).await?.0;
    let rows = list_q.bind(limit).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(PmListResponse {
        items: rows.iter().map(material_job_from_row).collect(),
        total,
    }))
}

async fn material_jobs_summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<PmMaterialSummaryResponse>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;

    let job_summary_sql = format!(
        "SELECT
            CAST(COUNT(*) AS INTEGER),
            CAST(COUNT(DISTINCT COALESCE(thread_id, id)) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'completed' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'failed' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN asset_type = 'text' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN asset_type = 'image' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN asset_type = 'music' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN asset_type = 'ppt' AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days') THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN iteration_no > 1 THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(MAX(created_at) AS TEXT)
         FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND asset_type IN ({PUBLIC_MATERIAL_ASSET_TYPES_SQL})",
    );
    let job_row = sqlx::query(&job_summary_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .fetch_one(&state.db)
        .await?;

    let asset_count_sql = format!(
        "SELECT CAST(COUNT(*) AS INTEGER)
         FROM pm_material_assets a
         INNER JOIN pm_material_jobs j ON j.tenant_id = a.tenant_id AND j.id = a.job_id
         WHERE a.tenant_id = ? AND j.created_by = ? AND a.asset_type IN ({PUBLIC_MATERIAL_ASSET_TYPES_SQL}) AND a.created_at >= datetime(CURRENT_TIMESTAMP, '-30 days')",
    );
    let asset_count_30d = sqlx::query_as::<_, (i64,)>(&asset_count_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .fetch_one(&state.db)
        .await?
        .0;

    let completed_jobs_30d = job_row.get::<i64, _>(3);
    let failed_jobs_30d = job_row.get::<i64, _>(4);
    let terminal_jobs_30d = completed_jobs_30d + failed_jobs_30d;
    let success_rate_30d = if terminal_jobs_30d > 0 {
        completed_jobs_30d as f64 / terminal_jobs_30d as f64
    } else {
        0.0
    };

    Ok(Json(PmMaterialSummaryResponse {
        total_jobs: job_row.get::<i64, _>(0),
        total_threads: job_row.get::<i64, _>(1),
        running_jobs: job_row.get::<i64, _>(2),
        completed_jobs_30d,
        failed_jobs_30d,
        success_rate_30d,
        text_jobs_30d: job_row.get::<i64, _>(5),
        image_jobs_30d: job_row.get::<i64, _>(6),
        music_jobs_30d: job_row.get::<i64, _>(7),
        ppt_jobs_30d: job_row.get::<i64, _>(8),
        asset_count_30d,
        versioned_jobs: job_row.get::<i64, _>(9),
        latest_job_at: parse_dt_opt(job_row.get::<Option<String>, _>(10)),
    }))
}

async fn list_material_threads(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmMaterialJobQuery>,
) -> Result<Json<PmListResponse<PmMaterialThreadRecord>>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = i64::from(per_page);
    let offset = i64::from((page.saturating_sub(1)) * per_page);

    let mut group_sql = format!(
        "SELECT COALESCE(thread_id, id) AS thread_key, MAX(id) AS latest_job_id, COUNT(*) AS version_count
         FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND asset_type IN ({PUBLIC_MATERIAL_ASSET_TYPES_SQL})",
    );
    if query.mission_run_id.is_some() {
        group_sql.push_str(" AND mission_run_id = ?");
    }
    if query.thread_id.is_some() {
        group_sql.push_str(" AND COALESCE(thread_id, id) = ?");
    }
    group_sql.push_str(" GROUP BY COALESCE(thread_id, id)");

    let mut count_sql = format!(
        "SELECT COUNT(*) FROM ({group_sql}) grp
         INNER JOIN pm_material_jobs j
           ON j.tenant_id = ? AND j.id = grp.latest_job_id
         WHERE 1=1"
    );
    if query
        .asset_type
        .as_ref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        count_sql.push_str(" AND j.asset_type = ?");
    }
    if query
        .status
        .as_ref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        count_sql.push_str(" AND j.status = ?");
    }

    let mut list_sql = format!(
        "SELECT CAST(grp.thread_key AS INTEGER),
                CAST(j.id AS INTEGER),
                CAST(j.mission_run_id AS INTEGER),
                CAST(grp.version_count AS INTEGER),
                CAST(j.iteration_no AS INTEGER),
                j.prompt_text, j.model, j.asset_type, j.status, j.result_count,
                j.error_message, j.created_by,
                CAST(j.created_at AS TEXT), CAST(j.updated_at AS TEXT)
         FROM ({group_sql}) grp
         INNER JOIN pm_material_jobs j
           ON j.tenant_id = ? AND j.id = grp.latest_job_id
         WHERE 1=1"
    );
    if query
        .asset_type
        .as_ref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        list_sql.push_str(" AND j.asset_type = ?");
    }
    if query
        .status
        .as_ref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        list_sql.push_str(" AND j.status = ?");
    }
    list_sql.push_str(" ORDER BY j.updated_at DESC, j.id DESC LIMIT ? OFFSET ?");

    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    if let Some(mission_run_id) = query.mission_run_id {
        count_q = count_q.bind(mission_run_id);
    }
    if let Some(thread_id) = query.thread_id {
        count_q = count_q.bind(thread_id);
    }
    count_q = count_q.bind(&claims.tenant_id);
    if let Some(asset_type) = query.asset_type.as_ref().filter(|v| !v.trim().is_empty()) {
        if !is_public_material_asset_type(asset_type.trim()) {
            return Err(AppError::ValidationError(public_material_asset_type_error()));
        }
        count_q = count_q.bind(asset_type);
    }
    if let Some(status) = query.status.as_ref().filter(|v| !v.trim().is_empty()) {
        count_q = count_q.bind(status);
    }
    let total = count_q.fetch_one(&state.db).await?.0;

    let mut list_q = sqlx::query(&list_sql)
        .bind(&claims.tenant_id)
        .bind(&claims.sub);
    if let Some(mission_run_id) = query.mission_run_id {
        list_q = list_q.bind(mission_run_id);
    }
    if let Some(thread_id) = query.thread_id {
        list_q = list_q.bind(thread_id);
    }
    list_q = list_q.bind(&claims.tenant_id);
    if let Some(asset_type) = query.asset_type.as_ref().filter(|v| !v.trim().is_empty()) {
        if !is_public_material_asset_type(asset_type.trim()) {
            return Err(AppError::ValidationError(public_material_asset_type_error()));
        }
        list_q = list_q.bind(asset_type);
    }
    if let Some(status) = query.status.as_ref().filter(|v| !v.trim().is_empty()) {
        list_q = list_q.bind(status);
    }
    let rows = list_q.bind(limit).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(PmListResponse {
        items: rows.iter().map(material_thread_from_row).collect(),
        total,
    }))
}

async fn list_material_models(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmMaterialModelQuery>,
) -> Result<Json<PmMaterialModelListResponse>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let asset_type = query
        .asset_type
        .unwrap_or_else(|| "text".to_string())
        .trim()
        .to_ascii_lowercase();
    if !is_public_material_asset_type(&asset_type) {
        return Ok(Json(PmMaterialModelListResponse { items: Vec::new() }));
    }
    let Some(model_type) = material_model_type_for_asset_and_stage(
        asset_type.as_str(),
        query.workflow_stage.as_deref(),
    ) else {
        return Ok(Json(PmMaterialModelListResponse { items: Vec::new() }));
    };
    let entries = state
        .config_registry()
        .resolve_api_keys_by_model_type(&claims.tenant_id, Some("pm"), model_type)
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "failed to load PM models (asset_type={asset_type}, model_type={model_type}): {e}"
            ))
        })?;
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for entry in entries {
        let Some(model_name) = entry
            .model
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let model_key = model_name.to_ascii_lowercase();
        if seen.insert(model_key) {
            items.push(PmMaterialModelOption {
                model: model_name.to_string(),
            });
        }
    }
    Ok(Json(PmMaterialModelListResponse { items }))
}

async fn run_material_job_generation(
    state: AppState,
    tenant_id: String,
    user_id: String,
    job_id: i64,
    asset_type: String,
    prompt_text: String,
    base_prompt: String,
    audio_input_text: Option<String>,
    selected_model: String,
    candidates: Vec<agent_gateway::ApiKeyEntry>,
    reference_images: Vec<PmMaterialReferenceImageInput>,
    continuation_asset: Option<PmContinuationAssetContext>,
    workflow_stage: Option<String>,
) -> Result<(), AppError> {
    if settle_material_job_cancellation(&state, &tenant_id, job_id).await? {
        return Ok(());
    }
    let system_prompt = "You are a product operations copilot. Always ground your answer in available evidence and clearly separate facts from hypotheses.";
    let mut last_error: Option<String> = None;
    let mut result: Option<PmMaterialRunResult> = None;

    let ppt_final_prompt = if asset_type == "ppt"
        && workflow_stage
            .as_deref()
            .is_some_and(|stage| stage.eq_ignore_ascii_case("generate"))
    {
        let thread_id = load_material_job_thread_key(&state, &tenant_id, &user_id, job_id)
            .await
            .unwrap_or(job_id);
        let thread_context = load_material_thread_text_context(
            &state, &tenant_id, &user_id, thread_id, job_id, "ppt",
        )
        .await
        .unwrap_or_default();
        Some(build_ppt_final_deck_input(
            &prompt_text,
            continuation_asset.as_ref(),
            &thread_context,
        ))
    } else {
        None
    };

    for entry in &candidates {
        if settle_material_job_cancellation(&state, &tenant_id, job_id).await? {
            return Ok(());
        }
        let run_result = if asset_type == "image" {
            run_pm_image_generation_with_key(
                &state,
                &user_id,
                entry,
                &selected_model,
                &base_prompt,
                &reference_images,
            )
            .await
            .map(PmMaterialRunResult::Image)
        } else if asset_type == "music"
            && workflow_stage
                .as_deref()
                .is_some_and(|stage| stage.eq_ignore_ascii_case("generate"))
        {
            let music_input = audio_input_text
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or(base_prompt.as_str());
            run_pm_audio_generation_with_key(
                &state,
                &user_id,
                entry,
                &selected_model,
                continuation_asset.as_ref(),
                music_input,
            )
            .await
            .map(PmMaterialRunResult::Audio)
        } else if asset_type == "ppt"
            && workflow_stage
                .as_deref()
                .is_some_and(|stage| stage.eq_ignore_ascii_case("generate"))
        {
            let prompt = ppt_final_prompt.as_deref().unwrap_or(base_prompt.as_str());
            tracing::info!(
                job_id,
                key_id = %entry.id,
                provider = %entry.provider,
                model = %selected_model,
                prompt_chars = prompt.chars().count(),
                search_stage = "pm_material_ppt_html_stream",
                "PM material PPT final deck generation attempting streaming LLM call"
            );
            let api_messages = to_api_messages(&[crate::routes::chat::ChatMessage {
                role: "user".to_string(),
                content: serde_json::json!(prompt),
            }]);
            run_pm_stream_completion_with_key(
                entry,
                &selected_model,
                api_messages,
                "You are an expert presentation designer and HTML deck engineer. Return only a complete, self-contained HTML presentation document when asked for PPT generation.",
                6144,
                default_pm_rules(),
            )
            .await
            .and_then(|text_res| {
                let html = normalize_ppt_html(&text_res.answer);
                let asset_url = persist_generated_html(&state, &user_id, &html)?;
                Ok(PmMaterialRunResult::Ppt(PmPptRunResult {
                    asset_url,
                    html,
                    usage: text_res.usage,
                    api_key_id: text_res.api_key_id,
                    provider_name: text_res.provider_name,
                }))
            })
        } else {
            let api_messages = to_api_messages(&[crate::routes::chat::ChatMessage {
                role: "user".to_string(),
                content: serde_json::json!(base_prompt.clone()),
            }]);
            run_pm_completion_with_key(
                entry,
                &selected_model,
                api_messages,
                system_prompt,
                4096,
                default_pm_rules(),
                None,
            )
            .await
            .map(PmMaterialRunResult::Text)
        };
        if settle_material_job_cancellation(&state, &tenant_id, job_id).await? {
            return Ok(());
        }
        match run_result {
            Ok(res) => {
                result = Some(res);
                break;
            }
            Err(err) => {
                last_error = Some(err.to_string());
                tracing::warn!(
                    key_id = %entry.id,
                    provider = %entry.provider,
                    model = %entry.model.as_deref().unwrap_or(selected_model.as_str()),
                    error = %err,
                    "PM material generation failed on candidate key, trying failover"
                );
            }
        }
    }

    let res = result.ok_or_else(|| {
        AppError::Internal(format!(
            "all PM material model candidates failed: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        ))
    })?;

    let (asset_url, content_text, usage, api_key_id, provider_name, extra_meta) = match res {
        PmMaterialRunResult::Text(text_res) => (
            None,
            Some(text_res.answer),
            text_res.usage,
            text_res.api_key_id,
            text_res.provider_name,
            Value::Null,
        ),
        PmMaterialRunResult::Image(image_res) => (
            Some(image_res.asset_url),
            image_res.revised_prompt,
            image_res.usage,
            image_res.api_key_id,
            image_res.provider_name,
            serde_json::json!({
                "generatedKind": "image",
                "imageSize": image_res.image_size,
                "imageMode": image_res.image_mode,
                "referenceImageCount": image_res.reference_image_count
            }),
        ),
        PmMaterialRunResult::Audio(audio_res) => (
            Some(audio_res.asset_url),
            None,
            audio_res.usage,
            audio_res.api_key_id,
            audio_res.provider_name,
            serde_json::json!({
                "generatedKind": "audio",
                "audioFormat": audio_res.audio_format,
                "audioProviderMeta": audio_res.provider_meta
            }),
        ),
        PmMaterialRunResult::Ppt(ppt_res) => (
            Some(ppt_res.asset_url),
            Some(ppt_res.html),
            ppt_res.usage,
            ppt_res.api_key_id,
            ppt_res.provider_name,
            serde_json::json!({
                "generatedKind": "html_ppt",
                "exportFormats": ["html", "pdf", "pptx"],
                "renderEngine": "html-ppt"
            }),
        ),
    };

    let mut tx = state.db.begin().await.map_err(|e| {
        AppError::Internal(format!("failed to start material job transaction: {e}"))
    })?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM pm_material_jobs
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&tenant_id)
    .bind(job_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("PM material job not found".to_string()))?;
    if matches!(status.as_str(), "cancelling" | "cancelled") {
        sqlx::query(
            "UPDATE pm_material_jobs
             SET status = 'cancelled', error_message = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    if status != "running" {
        tx.rollback().await?;
        return Err(AppError::Conflict(format!(
            "PM material job cannot persist a result while status is '{status}'"
        )));
    }

    sqlx::query(
        "INSERT INTO pm_material_assets
            (tenant_id, job_id, asset_type, url, content_text, meta_json)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&tenant_id)
    .bind(job_id)
    .bind(&asset_type)
    .bind(asset_url.as_deref())
    .bind(content_text.as_deref())
    .bind(
        serde_json::json!({
            "model": usage.model,
            "inputTokens": usage.input_tokens,
            "outputTokens": usage.output_tokens,
            "workflowStage": workflow_stage,
            "generationCount": if material_workflow_is_generate_stage(workflow_stage.as_deref()) { 1 } else { 0 },
            "referenceImages": reference_images,
            "apiKeyId": api_key_id,
            "provider": provider_name,
            "extra": extra_meta
        })
        .to_string(),
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(format!("failed to persist material asset: {e}")))?;

    sqlx::query(
        "UPDATE pm_material_jobs
         SET status = 'completed', result_count = ?, model = ?, error_message = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND tenant_id = ? AND status = 'running'",
    )
    .bind(1_i32)
    .bind(trim_text(&usage.model, 128))
    .bind(job_id)
    .bind(&tenant_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(format!("failed to complete material job: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit material job: {e}")))?;

    spawn_material_job_completed_notification(
        state.clone(),
        tenant_id.clone(),
        job_id,
        asset_type.clone(),
    );

    match run_lifecycle_hooks(
        &state,
        &tenant_id,
        "materials",
        HookEventType::TaskCompleted,
        "materials.job_completed",
        serde_json::json!({
            "jobId": job_id,
            "assetType": &asset_type,
            "workflowStage": &workflow_stage,
            "model": &usage.model,
            "apiKeyId": &api_key_id,
            "provider": &provider_name,
        }),
        Some(serde_json::json!({
            "assetUrl": &asset_url,
            "contentText": &content_text,
            "usage": &usage,
        })),
        false,
    )
    .await
    {
        Ok(hook_result) if hook_result.is_failed() || hook_result.is_cancelled() => {
            tracing::warn!(
                tenant_id = %tenant_id,
                job_id,
                "materials task_completed hook completed with warning: {}",
                hook_result.messages().join("\n")
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                job_id,
                error = %error,
                "materials task_completed hook failed to execute"
            );
        }
    }

    let usage_record = crate::routes::chat::TokenUsageRecord {
        tenant_id,
        user_id,
        session_id: format!("pm-material-job-{job_id}"),
        request_id: None,
        model: usage.model.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        total_tokens: usage.total_tokens,
        estimated_cost_usd: usage.estimated_cost_usd,
        api_key_id: Some(api_key_id),
        provider: provider_name,
        created_at: Utc::now(),
    };
    let _ = state.usage_writer().write(&usage_record).await;

    Ok(())
}

async fn settle_material_job_cancellation(
    state: &AppState,
    tenant_id: &str,
    job_id: i64,
) -> Result<bool, AppError> {
    let status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM pm_material_jobs
         WHERE tenant_id = ? AND id = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(job_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("PM material job not found".to_string()))?;
    if status == "cancelling" {
        sqlx::query(
            "UPDATE pm_material_jobs
             SET status = 'cancelled', error_message = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND id = ? AND status = 'cancelling'",
        )
        .bind(tenant_id)
        .bind(job_id)
        .execute(&state.db)
        .await?;
        return Ok(true);
    }
    Ok(status == "cancelled")
}

async fn abort_material_job_intake(
    state: &AppState,
    tenant_id: &str,
    job_id: i64,
    agent_task_id: Option<&str>,
    error: &AppError,
) {
    let error_message = error.to_string();
    let persist_result = sqlx::query(
        "UPDATE pm_material_jobs
         SET error_message = CASE WHEN status = 'cancelling' THEN NULL ELSE ? END,
             status = CASE WHEN status = 'cancelling' THEN 'cancelled' ELSE 'failed' END,
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?
           AND status IN ('queued','running','cancelling')",
    )
    .bind(&error_message)
    .bind(tenant_id)
    .bind(job_id)
    .execute(&state.db)
    .await;
    if let Err(persist_error) = persist_result {
        tracing::error!(
            tenant_id,
            job_id,
            error = %persist_error,
            error_debug = ?persist_error,
            "failed to persist PM material intake failure"
        );
    }
    if let Some(task_id) = agent_task_id {
        let material_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM pm_material_jobs WHERE tenant_id = ? AND id = ? LIMIT 1",
        )
        .bind(tenant_id)
        .bind(job_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        let agent_ops_result = if material_status.as_deref() == Some("cancelled") {
            crate::routes::agent_ops::mark_task_cancelled(
                state,
                tenant_id,
                task_id,
                "素材生成已取消",
                Some(serde_json::json!({ "materialJobId": job_id })),
            )
            .await
        } else {
            crate::routes::agent_ops::fail_task(
                state,
                tenant_id,
                task_id,
                "material_intake_failed",
                &error_message,
            )
            .await
        };
        if let Err(agent_ops_error) = agent_ops_result {
            tracing::error!(
                tenant_id,
                job_id,
                task_id,
                error = %agent_ops_error,
                error_debug = ?agent_ops_error,
                "failed to mark AgentOps material task failed"
            );
        }
    }
}

async fn create_material_job(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PmMaterialJobCreateRequest>,
) -> Result<Json<PmMaterialJobRecord>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let mut prompt_text = req.prompt_text.trim().to_string();
    let asset_type = req
        .asset_type
        .as_deref()
        .unwrap_or("text")
        .trim()
        .to_ascii_lowercase();
    if !is_public_material_asset_type(asset_type.as_str()) {
        return Err(AppError::ValidationError(public_material_asset_type_error()));
    }
    let workflow_stage =
        normalize_material_workflow_stage(asset_type.as_str(), req.workflow_stage.as_deref())
            .map_err(|error| AppError::ValidationError(error.to_string()))?;
    let workflow_payload = req.workflow_payload.clone();
    let continuation_asset = if let Some(asset_id) = req.continue_from_asset_id {
        let asset =
            load_continuation_asset_context(&state, &claims.tenant_id, &claims.sub, asset_id)
                .await?
                .ok_or_else(|| {
                    AppError::ValidationError(
                        "continue_from_asset_id does not exist under current tenant".to_string(),
                    )
                })?;
        if !asset.asset_type.eq_ignore_ascii_case(&asset_type) {
            return Err(AppError::ValidationError(
                "continue_from_asset_id asset_type does not match current asset_type".to_string(),
            ));
        }
        Some(asset)
    } else {
        None
    };
    if prompt_text.is_empty() {
        if let Some(parent_job_id) = req.parent_job_id {
            if let Some(inherited_prompt) = load_material_job_original_prompt(
                &state,
                &claims.tenant_id,
                &claims.sub,
                parent_job_id,
            )
            .await?
            {
                prompt_text = inherited_prompt;
            }
        }
    }
    if prompt_text.is_empty() {
        return Err(AppError::ValidationError(
            "prompt_text cannot be empty".to_string(),
        ));
    }

    let mut reference_images = req.reference_images.clone();
    if asset_type == "image" {
        if reference_images.len() > 3 {
            return Err(AppError::ValidationError(
                "reference_images supports up to 3 images".to_string(),
            ));
        }
        let expected_prefix = format!("/api/v1/uploads/{}/", claims.sub);
        for (idx, image) in reference_images.iter().enumerate() {
            let url = image.url.trim();
            if url.is_empty() {
                return Err(AppError::ValidationError(format!(
                    "reference_images[{idx}] url cannot be empty"
                )));
            }
            if !url.starts_with(&expected_prefix) {
                return Err(AppError::ValidationError(
                    "reference_images must use your uploaded assets".to_string(),
                ));
            }
        }
        if reference_images.is_empty() {
            if let Some(continuation_url) = continuation_asset
                .as_ref()
                .and_then(|ctx| ctx.url.as_deref())
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                let expected_prefix = format!("/api/v1/uploads/{}/", claims.sub);
                if continuation_url.starts_with(&expected_prefix) {
                    reference_images.push(PmMaterialReferenceImageInput {
                        url: continuation_url.to_string(),
                        media_type: None,
                        name: Some("上一版生成结果".to_string()),
                        size_bytes: None,
                    });
                }
            }
        }
    } else {
        reference_images.clear();
    }
    let reference_block = if reference_images.is_empty() {
        String::new()
    } else {
        let mut lines = String::new();
        for (idx, image) in reference_images.iter().enumerate() {
            let label = image
                .name
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or("参考图");
            lines.push_str(&format!("{}. {} ({})\n", idx + 1, label, image.url.trim()));
        }
        format!("参考图（最多3张）如下，可作为风格、元素、构图等参考信息，请结合用户需求灵活创作：\n{lines}\n")
    };
    let base_prompt = build_material_base_prompt(
        asset_type.as_str(),
        workflow_stage.as_deref(),
        &prompt_text,
        &reference_block,
        continuation_asset.as_ref(),
        workflow_payload.as_ref(),
    );
    let selected_model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| AppError::ValidationError("model cannot be empty".to_string()))?;
    let model_type =
        material_model_type_for_asset_and_stage(asset_type.as_str(), workflow_stage.as_deref())
            .ok_or_else(|| AppError::ValidationError(public_material_asset_type_error()))?;
    let mut candidates =
        resolve_pm_scoped_api_keys_by_model_type(&state, &claims.tenant_id, model_type).await?;
    {
        let selected = selected_model.as_str();
        candidates.retain(|entry| {
            entry
                .model
                .as_deref()
                .map(str::trim)
                .is_some_and(|m| m.eq_ignore_ascii_case(selected))
        });
        if candidates.is_empty() {
            return Err(AppError::ValidationError(format!(
                "selected model '{selected}' is unavailable; please check API key management"
            )));
        }
    }

    let mut resolved_thread_id = req.thread_id;
    let resolved_parent_job_id = req.parent_job_id;
    let mut iteration_no = 1_i32;
    if let Some(parent_job_id) = resolved_parent_job_id {
        let parent =
            load_material_job_chain_info(&state, &claims.tenant_id, &claims.sub, parent_job_id)
                .await?
                .ok_or_else(|| {
                    AppError::ValidationError(
                        "parent_job_id does not exist under current tenant".to_string(),
                    )
                })?;
        if !parent.asset_type.eq_ignore_ascii_case(&asset_type) {
            return Err(AppError::ValidationError(
                "parent_job_id asset_type does not match current asset_type".to_string(),
            ));
        }
        let parent_thread_id = normalize_thread_id(parent.thread_id, parent_job_id);
        if let Some(thread_id) = resolved_thread_id {
            if thread_id != parent_thread_id {
                return Err(AppError::ValidationError(
                    "thread_id and parent_job_id are inconsistent".to_string(),
                ));
            }
        } else {
            resolved_thread_id = Some(parent_thread_id);
        }
        iteration_no = parent.iteration_no.saturating_add(1).max(2);
    }
    if let Some(thread_id) = resolved_thread_id {
        let max_iter =
            load_latest_job_iteration_in_thread(&state, &claims.tenant_id, &claims.sub, thread_id)
                .await?
                .max(0);
        if max_iter > 0 {
            iteration_no = iteration_no.max(max_iter.saturating_add(1));
        }
    }

    let job_id_u64 = sqlx::query(
        "INSERT INTO pm_material_jobs
            (tenant_id, mission_run_id, thread_id, parent_job_id, iteration_no, prompt_text, model, asset_type, status, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?)",
    )
    .bind(&claims.tenant_id)
    .bind(req.mission_run_id)
    .bind(resolved_thread_id)
    .bind(resolved_parent_job_id)
    .bind(iteration_no)
    .bind(&prompt_text)
    .bind(Some(selected_model.clone()))
    .bind(&asset_type)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?
    .last_insert_rowid();
    let job_id = i64::try_from(job_id_u64).map_err(|_| {
        AppError::Internal("PM material job id exceeds supported range".to_string())
    })?;

    // Root jobs default to self-thread for version lineage.
    if resolved_thread_id.is_none() {
        if let Err(error) = sqlx::query(
            "UPDATE pm_material_jobs
             SET thread_id = ?
             WHERE id = ? AND tenant_id = ?",
        )
        .bind(job_id)
        .bind(job_id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await
        {
            let error = AppError::Database(error);
            abort_material_job_intake(&state, &claims.tenant_id, job_id, None, &error).await;
            return Err(error);
        }
    }

    let music_audio_input_text = if asset_type == "music"
        && workflow_stage
            .as_deref()
            .is_some_and(|stage| stage.eq_ignore_ascii_case("generate"))
    {
        let thread_id_for_context = resolved_thread_id.unwrap_or(job_id);
        let thread_context = match load_material_thread_text_context(
            &state,
            &claims.tenant_id,
            &claims.sub,
            thread_id_for_context,
            job_id,
            "music",
        )
        .await
        {
            Ok(context) => context,
            Err(error) => {
                abort_material_job_intake(&state, &claims.tenant_id, job_id, None, &error).await;
                return Err(error);
            }
        };
        build_music_audio_generation_input(
            &prompt_text,
            continuation_asset.as_ref(),
            &thread_context,
        )
    } else {
        None
    };

    let agent_task = match crate::routes::agent_ops::create_task_with_outcome(
        &state,
        crate::routes::agent_ops_types::CreateAgentTaskInput {
            tenant_id: claims.tenant_id.clone(),
            source: "pm_materials".to_string(),
            source_ref: Some(job_id.to_string()),
            source_label: Some("Materials Studio".to_string()),
            capability_key: "materials".to_string(),
            agent_id: None,
            agent_name: Some("素材生成".to_string()),
            title: format!("生成 {asset_type} 素材"),
            summary: None,
            owner_user_id: Some(claims.sub.clone()),
            correlation_id: Some(format!(
                "pm-material-thread-{}",
                resolved_thread_id.unwrap_or(job_id)
            )),
            parent_task_id: None,
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
            external_message_id: None,
            idempotency_key: Some(format!("pm-material-job:{}:{job_id}", claims.tenant_id)),
            input_json: Some(serde_json::json!({
                "materialJobId": job_id,
                "assetType": asset_type,
                "workflowStage": workflow_stage,
                "model": selected_model,
            })),
        },
    )
    .await
    {
        Ok(task) => task,
        Err(error) => {
            abort_material_job_intake(&state, &claims.tenant_id, job_id, None, &error).await;
            return Err(error);
        }
    };
    if let Err(error) = crate::routes::agent_ops::link_task_resource(
        &state,
        &claims.tenant_id,
        &agent_task.id,
        "pm_material_job",
        &job_id.to_string(),
    )
    .await
    {
        abort_material_job_intake(
            &state,
            &claims.tenant_id,
            job_id,
            Some(&agent_task.id),
            &error,
        )
        .await;
        return Err(error);
    }
    let started = match sqlx::query(
        "UPDATE pm_material_jobs SET status = 'running', updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ? AND status = 'queued'",
    )
    .bind(&claims.tenant_id)
    .bind(job_id)
    .execute(&state.db)
    .await
    {
        Ok(started) => started,
        Err(error) => {
            let error = AppError::Database(error);
            abort_material_job_intake(
                &state,
                &claims.tenant_id,
                job_id,
                Some(&agent_task.id),
                &error,
            )
            .await;
            return Err(error);
        }
    };
    if started.rows_affected() != 1 {
        let error = AppError::Conflict("PM material job did not enter running state".to_string());
        abort_material_job_intake(
            &state,
            &claims.tenant_id,
            job_id,
            Some(&agent_task.id),
            &error,
        )
        .await;
        return Err(error);
    }
    if let Err(error) = crate::routes::agent_ops::sync_linked_resource_status(
        &state,
        &claims.tenant_id,
        &agent_task.id,
    )
    .await
    {
        abort_material_job_intake(
            &state,
            &claims.tenant_id,
            job_id,
            Some(&agent_task.id),
            &error,
        )
        .await;
        return Err(error);
    }

    let row = match sqlx::query(
        "SELECT CAST(id AS INTEGER), CAST(mission_run_id AS INTEGER), CAST(thread_id AS INTEGER), CAST(parent_job_id AS INTEGER),
                CAST(iteration_no AS INTEGER), prompt_text, model, asset_type, status, result_count,
                error_message, created_by, CAST(created_at AS TEXT), CAST(updated_at AS TEXT)
         FROM pm_material_jobs
         WHERE id = ? AND tenant_id = ?",
    )
    .bind(job_id)
    .bind(&claims.tenant_id)
    .fetch_one(&state.db)
    .await
    {
        Ok(row) => row,
        Err(error) => {
            let error = AppError::Database(error);
            abort_material_job_intake(
                &state,
                &claims.tenant_id,
                job_id,
                Some(&agent_task.id),
                &error,
            )
            .await;
            return Err(error);
        }
    };

    let state_for_bg = state.clone();
    let tenant_id_for_bg = claims.tenant_id.clone();
    let user_id_for_bg = claims.sub.clone();
    let asset_type_for_bg = asset_type.clone();
    let prompt_text_for_bg = prompt_text.clone();
    let base_prompt_for_bg = base_prompt.clone();
    let audio_input_text_for_bg = music_audio_input_text.clone();
    let selected_model_for_bg = selected_model.clone();
    let reference_images_for_bg = reference_images.clone();
    let continuation_asset_for_bg = continuation_asset.clone();
    let workflow_stage_for_bg = workflow_stage.clone();
    let agent_task_id_for_bg = agent_task.id.clone();
    tokio::spawn(async move {
        let generation_result = std::panic::AssertUnwindSafe(run_material_job_generation(
            state_for_bg.clone(),
            tenant_id_for_bg.clone(),
            user_id_for_bg.clone(),
            job_id,
            asset_type_for_bg,
            prompt_text_for_bg,
            base_prompt_for_bg,
            audio_input_text_for_bg,
            selected_model_for_bg,
            candidates,
            reference_images_for_bg,
            continuation_asset_for_bg,
            workflow_stage_for_bg,
        ))
        .catch_unwind()
        .await;
        let failure_message = match generation_result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("PM material worker panicked".to_string()),
        };
        if let Some(error_message) = failure_message {
            tracing::error!(
                tenant_id = %tenant_id_for_bg,
                user_id = %user_id_for_bg,
                job_id = job_id,
                error = %error_message,
                "PM material job generation failed"
            );
            let _ = sqlx::query(
                "UPDATE pm_material_jobs
                 SET error_message = CASE WHEN status = 'cancelling' THEN NULL ELSE ? END,
                     status = CASE WHEN status = 'cancelling' THEN 'cancelled' ELSE 'failed' END,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ? AND tenant_id = ?
                   AND status IN ('queued','running','cancelling')",
            )
            .bind(error_message)
            .bind(job_id)
            .bind(&tenant_id_for_bg)
            .execute(&state_for_bg.db)
            .await;
        }
        if let Err(error) = crate::routes::agent_ops::sync_linked_resource_status(
            &state_for_bg,
            &tenant_id_for_bg,
            &agent_task_id_for_bg,
        )
        .await
        {
            tracing::error!(
                tenant_id = %tenant_id_for_bg,
                user_id = %user_id_for_bg,
                job_id,
                agent_task_id = %agent_task_id_for_bg,
                error = %error,
                error_debug = ?error,
                "failed to project PM material job terminal status"
            );
        }
    });

    Ok(Json(material_job_from_row(&row)))
}

async fn list_material_assets(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(job_id): Path<i64>,
) -> Result<Json<PmListResponse<PmMaterialAssetRecord>>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let rows = sqlx::query(
        "SELECT CAST(id AS INTEGER), CAST(job_id AS INTEGER), asset_type, url, content_text, CAST(meta_json AS TEXT), CAST(created_at AS TEXT)
         FROM pm_material_assets a
         WHERE a.tenant_id = ? AND a.job_id = ?
           AND EXISTS (
             SELECT 1 FROM pm_material_jobs j
             WHERE j.tenant_id = a.tenant_id AND j.id = a.job_id AND j.created_by = ?
           )
         ORDER BY id ASC",
    )
    .bind(&claims.tenant_id)
    .bind(job_id)
    .bind(&claims.sub)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(PmListResponse {
        total: i64::try_from(rows.len()).unwrap_or(0),
        items: rows.iter().map(material_asset_from_row).collect(),
    }))
}

async fn export_material_asset(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(asset_id): Path<i64>,
    Query(query): Query<PmMaterialAssetExportQuery>,
) -> Result<Json<PmMaterialAssetExportResponse>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let format = query
        .format
        .as_deref()
        .unwrap_or("pdf")
        .trim()
        .to_ascii_lowercase();
    if !matches!(format.as_str(), "pdf" | "pptx") {
        return Err(AppError::ValidationError(
            "format must be pdf or pptx".to_string(),
        ));
    }

    let row = sqlx::query(
        "SELECT a.asset_type, a.url, a.content_text, CAST(a.meta_json AS TEXT), j.created_by
         FROM pm_material_assets a
         INNER JOIN pm_material_jobs j
            ON j.tenant_id = a.tenant_id AND j.id = a.job_id
         WHERE a.tenant_id = ? AND j.created_by = ? AND a.id = ?
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(asset_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::ValidationError("material asset not found".to_string()))?;

    let asset_type = row.get::<String, _>(0);
    if !asset_type.eq_ignore_ascii_case("ppt") {
        return Err(AppError::ValidationError(
            "only PPT material assets can be exported".to_string(),
        ));
    }
    let owner_user_id = row
        .get::<Option<String>, _>(4)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| claims.sub.clone());
    if owner_user_id != claims.sub {
        return Err(AppError::ValidationError("unauthorized".to_string()));
    }

    let asset_url = row
        .get::<Option<String>, _>(1)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let asset_content = row.get::<Option<String>, _>(2);
    let asset_meta = parse_json_str(row.get::<Option<String>, _>(3))
        .unwrap_or(Value::Object(Default::default()));
    if !is_final_ppt_html_asset(&asset_meta, asset_url.as_deref(), asset_content.as_deref()) {
        return Err(AppError::ValidationError(
            "only final generated PPT HTML decks can be exported".to_string(),
        ));
    }

    let html_url = if let Some(url) = asset_url {
        url
    } else if let Some(content) = asset_content
        .map(|v| normalize_ppt_html(&v))
        .filter(|v| !v.trim().is_empty())
    {
        persist_generated_html(&state, &claims.sub, &content)?
    } else {
        return Err(AppError::ValidationError(
            "PPT asset has no HTML content to export".to_string(),
        ));
    };
    let html_path = generated_upload_path_for_url(&state, &claims.sub, &html_url)?;
    if !html_path.exists() {
        return Err(AppError::ValidationError(
            "PPT HTML asset file not found".to_string(),
        ));
    }

    let output_filename = format!("{}.{format}", uuid::Uuid::new_v4());
    let output_path = uploads_dir_for_user(&state.data_dir, &claims.sub).join(&output_filename);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AppError::Internal(format!(
                "failed to create export directory {}: {e}",
                parent.display()
            ))
        })?;
    }

    match format.as_str() {
        "pdf" => export_html_to_pdf(&html_path, &output_path).await?,
        "pptx" => export_html_to_pptx(&html_path, &output_path).await?,
        _ => unreachable!(),
    }

    let url = format!("/api/v1/uploads/{}/{}", claims.sub, output_filename);
    Ok(Json(PmMaterialAssetExportResponse {
        asset_id,
        format,
        url,
    }))
}

async fn delete_material_job(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(job_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;
    let status_row = sqlx::query_as::<_, (String,)>(
        "SELECT status FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND id = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(job_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((status,)) = status_row else {
        return Err(AppError::ValidationError(
            "material job not found".to_string(),
        ));
    };
    if matches!(status.as_str(), "queued" | "running" | "cancelling") {
        return Err(AppError::ValidationError(
            "cannot delete active material job".to_string(),
        ));
    }

    let mut tx = state.db.begin().await?;
    sqlx::query(
        "DELETE FROM pm_material_assets WHERE tenant_id = ? AND job_id = ?
         AND EXISTS (
           SELECT 1 FROM pm_material_jobs j
           WHERE j.tenant_id = pm_material_assets.tenant_id
             AND j.id = pm_material_assets.job_id AND j.created_by = ?
         )",
    )
    .bind(&claims.tenant_id)
    .bind(job_id)
    .bind(&claims.sub)
    .execute(&mut *tx)
    .await?;
    let affected = sqlx::query(
        "DELETE FROM pm_material_jobs WHERE tenant_id = ? AND created_by = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(job_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    if affected == 0 {
        return Err(AppError::ValidationError(
            "material job not found".to_string(),
        ));
    }
    Ok(Json(serde_json::json!({
        "ok": true,
        "id": job_id
    })))
}

async fn delete_material_thread(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(thread_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    require_pm_v2(&state, &claims.tenant_id).await?;

    let deleted_versions =
        delete_material_thread_rows(&state.db, &claims.tenant_id, &claims.sub, thread_id).await?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "threadId": thread_id,
        "deletedVersions": deleted_versions
    })))
}

async fn delete_material_thread_rows(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    thread_id: i64,
) -> Result<u64, AppError> {
    let mut tx = db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;

    let (version_count, active_count) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT CAST(COUNT(*) AS INTEGER),
                CAST(COALESCE(SUM(CASE WHEN status IN ('queued','running','cancelling') THEN 1 ELSE 0 END), 0) AS INTEGER)
         FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND COALESCE(thread_id, id) = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .fetch_one(&mut *tx)
    .await?;
    if version_count == 0 {
        return Err(AppError::ValidationError(
            "material thread not found".to_string(),
        ));
    }
    if active_count > 0 {
        return Err(AppError::ValidationError(
            "cannot delete a material thread with active jobs".to_string(),
        ));
    }

    sqlx::query(
        "DELETE FROM pm_material_assets
         WHERE tenant_id = ? AND job_id IN (
           SELECT id FROM pm_material_jobs
           WHERE tenant_id = ? AND created_by = ? AND COALESCE(thread_id, id) = ?
         )",
    )
    .bind(tenant_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .execute(&mut *tx)
    .await?;
    let deleted_versions = sqlx::query(
        "DELETE FROM pm_material_jobs
         WHERE tenant_id = ? AND created_by = ? AND COALESCE(thread_id, id) = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(thread_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;

    Ok(deleted_versions)
}

#[cfg(test)]
mod material_thread_delete_tests {
    use super::delete_material_thread_rows;

    async fn insert_job(
        db: &sqlx::SqlitePool,
        id: i64,
        tenant_id: &str,
        user_id: &str,
        thread_id: Option<i64>,
        status: &str,
    ) {
        sqlx::query(
            "INSERT INTO pm_material_jobs
                (id, tenant_id, created_by, thread_id, prompt_text, asset_type, status)
             VALUES (?, ?, ?, ?, 'test material', 'text', ?)",
        )
        .bind(id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(thread_id)
        .bind(status)
        .execute(db)
        .await
        .expect("insert material job fixture");
    }

    #[tokio::test]
    async fn deleting_material_thread_removes_every_version_and_asset() {
        let db = crate::test_sqlite_pool().await;
        insert_job(&db, 101, "tenant-a", "user-a", None, "completed").await;
        insert_job(&db, 102, "tenant-a", "user-a", Some(101), "failed").await;
        for job_id in [101_i64, 102_i64] {
            sqlx::query(
                "INSERT INTO pm_material_assets
                    (tenant_id, job_id, asset_type, content_text)
                 VALUES ('tenant-a', ?, 'text', 'result')",
            )
            .bind(job_id)
            .execute(&db)
            .await
            .expect("insert material asset fixture");
        }

        let deleted = delete_material_thread_rows(&db, "tenant-a", "user-a", 101)
            .await
            .expect("delete complete material thread");

        assert_eq!(deleted, 2);
        let remaining_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pm_material_jobs WHERE tenant_id = 'tenant-a'",
        )
        .fetch_one(&db)
        .await
        .expect("count remaining material jobs");
        let remaining_assets: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pm_material_assets WHERE tenant_id = 'tenant-a'",
        )
        .fetch_one(&db)
        .await
        .expect("count remaining material assets");
        assert_eq!((remaining_jobs, remaining_assets), (0, 0));
        db.close().await;
    }

    #[tokio::test]
    async fn deleting_material_thread_rejects_any_active_version() {
        let db = crate::test_sqlite_pool().await;
        insert_job(&db, 201, "tenant-a", "user-a", None, "completed").await;
        insert_job(&db, 202, "tenant-a", "user-a", Some(201), "running").await;

        let result = delete_material_thread_rows(&db, "tenant-a", "user-a", 201).await;

        assert!(result.is_err());
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pm_material_jobs WHERE tenant_id = 'tenant-a'",
        )
        .fetch_one(&db)
        .await
        .expect("count preserved material jobs");
        assert_eq!(remaining, 2);
        db.close().await;
    }

    #[tokio::test]
    async fn deleting_material_thread_is_tenant_scoped() {
        let db = crate::test_sqlite_pool().await;
        insert_job(&db, 301, "tenant-a", "user-a", Some(42), "completed").await;
        insert_job(&db, 302, "tenant-b", "user-a", Some(42), "completed").await;
        sqlx::query(
            "INSERT INTO pm_material_assets
                (tenant_id, job_id, asset_type, content_text)
             VALUES ('tenant-b', 302, 'text', 'private result')",
        )
        .execute(&db)
        .await
        .expect("insert other tenant material asset");

        let deleted = delete_material_thread_rows(&db, "tenant-a", "user-a", 42)
            .await
            .expect("delete tenant material thread");

        assert_eq!(deleted, 1);
        let tenant_b_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pm_material_jobs WHERE tenant_id = 'tenant-b'",
        )
        .fetch_one(&db)
        .await
        .expect("count other tenant material jobs");
        let tenant_b_assets: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pm_material_assets WHERE tenant_id = 'tenant-b'",
        )
        .fetch_one(&db)
        .await
        .expect("count other tenant material assets");
        assert_eq!((tenant_b_jobs, tenant_b_assets), (1, 1));
        db.close().await;
    }

    #[tokio::test]
    async fn deleting_material_thread_cannot_delete_another_users_versions() {
        let db = crate::test_sqlite_pool().await;
        insert_job(&db, 401, "tenant-a", "other-user", Some(77), "completed").await;

        let result = delete_material_thread_rows(&db, "tenant-a", "current-user", 77).await;

        assert!(result.is_err());
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pm_material_jobs
             WHERE tenant_id = 'tenant-a' AND created_by = 'other-user'",
        )
        .fetch_one(&db)
        .await
        .expect("count other user's material jobs");
        assert_eq!(remaining, 1);
        db.close().await;
    }
}

fn pm_search_provider_templates() -> Vec<PmSearchProviderTemplate> {
    PM_SEARCH_PROVIDER_TEMPLATES
        .iter()
        .map(
            |(provider_type, label, default_base_url, default_method)| PmSearchProviderTemplate {
                provider_type,
                label,
                default_base_url,
                default_method,
            },
        )
        .collect()
}

fn parse_json_cell(raw: Option<String>) -> Option<Value> {
    raw.as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| serde_json::from_str(value).ok())
}

fn normalize_provider_type(raw: &str) -> Result<String, AppError> {
    let normalized = raw.trim().to_ascii_lowercase();
    if PM_SEARCH_PROVIDER_TYPES
        .iter()
        .any(|provider_type| *provider_type == normalized)
    {
        Ok(normalized)
    } else {
        Err(AppError::ValidationError(format!(
            "unsupported provider_type '{raw}'"
        )))
    }
}

fn normalize_provider_method(raw: Option<&str>, default: &str) -> Result<String, AppError> {
    let method = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_ascii_uppercase();
    match method.as_str() {
        "GET" | "POST" | "PUT" => Ok(method),
        _ => Err(AppError::ValidationError(
            "method must be GET, POST, or PUT".to_string(),
        )),
    }
}

fn normalize_auth_type(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("api_key")
        .to_ascii_lowercase()
}

fn json_bind(value: Option<&Value>) -> Result<Option<String>, AppError> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(AppError::from)
}

fn key_hint(secret: &str) -> Option<String> {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect(),
    )
}

fn decrypt_optional_secret(ciphertext: Option<String>) -> Option<String> {
    ciphertext.and_then(|raw| {
        if raw.trim().is_empty() {
            None
        } else {
            agent_gateway::decrypt(&raw).ok()
        }
    })
}

fn search_provider_type_to_tool_type(raw: &str) -> Option<tools::WebSearchProviderType> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "brave" => Some(tools::WebSearchProviderType::Brave),
        "tavily" => Some(tools::WebSearchProviderType::Tavily),
        "serper" => Some(tools::WebSearchProviderType::Serper),
        "exa" => Some(tools::WebSearchProviderType::Exa),
        "searxng" => Some(tools::WebSearchProviderType::Searxng),
        "generic_json" => Some(tools::WebSearchProviderType::GenericJson),
        "internal_http" => Some(tools::WebSearchProviderType::InternalHttp),
        _ => None,
    }
}

fn value_as_string_vec(value: Option<Value>) -> Option<Vec<String>> {
    let Value::Array(items) = value? else {
        return None;
    };
    Some(
        items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::trim).map(ToOwned::to_owned))
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

fn provider_select_sql() -> &'static str {
    r#"
    SELECT id, name, provider_type, enabled, priority, base_url, method, auth_type,
           auth_secret_ref, auth_secret_ciphertext, key_hint,
           CAST(headers_json AS TEXT) AS headers_json,
           CAST(query_template_json AS TEXT) AS query_template_json,
           CAST(response_mapping_json AS TEXT) AS response_mapping_json,
           timeout_secs, max_results, fetch_content_enabled, content_extract_mode,
           CAST(domain_allowlist_json AS TEXT) AS domain_allowlist_json,
           CAST(domain_blocklist_json AS TEXT) AS domain_blocklist_json,
           CAST(rate_limit_json AS TEXT) AS rate_limit_json,
           health_status, last_error, created_by, created_at, updated_at
    FROM pm_search_provider_configs
    "#
}

fn row_to_search_provider_record(row: &sqlx::sqlite::SqliteRow) -> PmSearchProviderRecord {
    let created_at: NaiveDateTime = row.get("created_at");
    let updated_at: NaiveDateTime = row.get("updated_at");
    let ciphertext: Option<String> = row.get("auth_secret_ciphertext");
    PmSearchProviderRecord {
        id: row.get("id"),
        name: row.get("name"),
        provider_type: row.get("provider_type"),
        enabled: row.get("enabled"),
        priority: row.get("priority"),
        base_url: row.get("base_url"),
        method: row.get("method"),
        auth_type: row.get("auth_type"),
        auth_secret_ref: row.get("auth_secret_ref"),
        has_secret: ciphertext
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        key_hint: row.get("key_hint"),
        headers_json: parse_json_cell(row.get("headers_json")),
        query_template_json: parse_json_cell(row.get("query_template_json")),
        response_mapping_json: parse_json_cell(row.get("response_mapping_json")),
        timeout_secs: row.get("timeout_secs"),
        max_results: row.get("max_results"),
        fetch_content_enabled: row.get("fetch_content_enabled"),
        content_extract_mode: row.get("content_extract_mode"),
        domain_allowlist_json: parse_json_cell(row.get("domain_allowlist_json")),
        domain_blocklist_json: parse_json_cell(row.get("domain_blocklist_json")),
        rate_limit_json: parse_json_cell(row.get("rate_limit_json")),
        health_status: row.get("health_status"),
        last_error: row.get("last_error"),
        created_by: row.get("created_by"),
        created_at: created_at.to_string(),
        updated_at: updated_at.to_string(),
    }
}

async fn load_pm_search_provider_rows(
    state: &AppState,
    tenant_id: &str,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, AppError> {
    let sql = format!(
        "{} WHERE tenant_id = ? ORDER BY priority ASC, created_at ASC",
        provider_select_sql()
    );
    Ok(sqlx::query(&sql)
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?)
}

async fn load_pm_search_provider_row(
    state: &AppState,
    tenant_id: &str,
    id: &str,
) -> Result<sqlx::sqlite::SqliteRow, AppError> {
    let sql = format!("{} WHERE tenant_id = ? AND id = ?", provider_select_sql());
    sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("search provider not found".into()))
}

fn row_to_tool_provider_config(
    row: &sqlx::sqlite::SqliteRow,
    query_max_results: Option<usize>,
) -> Result<tools::WebSearchProviderConfig, AppError> {
    let provider_type_raw: String = row.get("provider_type");
    let provider_type = search_provider_type_to_tool_type(&provider_type_raw)
        .ok_or_else(|| AppError::ValidationError("unsupported provider_type".into()))?;
    let max_results = query_max_results.or_else(|| {
        let raw: i32 = row.get("max_results");
        usize::try_from(raw.max(1)).ok()
    });
    Ok(tools::WebSearchProviderConfig {
        id: row.get("id"),
        name: row.get("name"),
        provider_type,
        enabled: row.get("enabled"),
        priority: row.get("priority"),
        base_url: row.get("base_url"),
        method: Some(row.get("method")),
        auth_type: Some(row.get("auth_type")),
        auth_secret: decrypt_optional_secret(row.get("auth_secret_ciphertext")),
        headers_json: parse_json_cell(row.get("headers_json")),
        query_template_json: parse_json_cell(row.get("query_template_json")),
        response_mapping_json: parse_json_cell(row.get("response_mapping_json")),
        timeout_secs: {
            let raw: i32 = row.get("timeout_secs");
            u64::try_from(raw.max(1)).ok()
        },
        max_results,
        fetch_content_enabled: Some(row.get("fetch_content_enabled")),
        content_extract_mode: Some(row.get("content_extract_mode")),
        domain_allowlist: value_as_string_vec(parse_json_cell(row.get("domain_allowlist_json"))),
        domain_blocklist: value_as_string_vec(parse_json_cell(row.get("domain_blocklist_json"))),
        rate_limit_json: parse_json_cell(row.get("rate_limit_json")),
    })
}

fn search_provider_row_is_runnable(row: &sqlx::sqlite::SqliteRow) -> bool {
    row.get::<bool, _>("enabled") && row.get::<String, _>("health_status") != "unhealthy"
}

fn validate_pm_search_provider_secret_ref(
    req: &PmSearchProviderUpsertRequest,
) -> Result<(), AppError> {
    if req
        .auth_secret_ref
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return Err(AppError::ValidationError(
            "authSecretRef is reserved for a future secret-store integration; provide API Key directly for now"
                .to_string(),
        ));
    }
    Ok(())
}

async fn notify_search_providers_changed(state: &AppState, tenant_id: &str) {
    if let Some(registry) = state.config_registry.as_ref() {
        registry.invalidate_tenant_cache(tenant_id).await;
    }
    if let Some(manager) = state.agent_manager.as_ref() {
        manager.reload_search_providers(tenant_id).await;
    }
    system_events::broadcast_search_providers_updated(tenant_id);
}

async fn list_search_providers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<PmSearchProviderListResponse>, AppError> {
    let rows = load_pm_search_provider_rows(&state, &claims.tenant_id).await?;
    let items = rows
        .iter()
        .map(row_to_search_provider_record)
        .collect::<Vec<_>>();
    Ok(Json(PmSearchProviderListResponse {
        total: i64::try_from(items.len()).unwrap_or(i64::MAX),
        items,
        templates: pm_search_provider_templates(),
    }))
}

async fn create_search_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PmSearchProviderUpsertRequest>,
) -> Result<Json<PmSearchProviderRecord>, AppError> {
    validate_pm_search_provider_secret_ref(&req)?;
    let provider_type = normalize_provider_type(
        req.provider_type
            .as_deref()
            .ok_or_else(|| AppError::ValidationError("providerType is required".into()))?,
    )?;
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::ValidationError("name is required".into()))?;
    let method = normalize_provider_method(req.method.as_deref(), "GET")?;
    let id = uuid::Uuid::new_v4().to_string();
    let encrypted = req
        .auth_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(agent_gateway::encrypt)
        .transpose()
        .map_err(|error| AppError::Internal(format!("encryption failed: {error}")))?;
    let hint = req.auth_secret.as_deref().and_then(key_hint);
    sqlx::query(
        r#"
        INSERT INTO pm_search_provider_configs
          (id, tenant_id, name, provider_type, enabled, priority, base_url, method, auth_type,
           auth_secret_ref, auth_secret_ciphertext, key_hint, headers_json, query_template_json,
           response_mapping_json, timeout_secs, max_results, fetch_content_enabled,
           content_extract_mode, domain_allowlist_json, domain_blocklist_json, rate_limit_json,
           created_by)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(name)
    .bind(&provider_type)
    .bind(req.enabled.unwrap_or(true))
    .bind(req.priority.unwrap_or(100))
    .bind(
        req.base_url
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(method)
    .bind(normalize_auth_type(req.auth_type.as_deref()))
    .bind(req.auth_secret_ref)
    .bind(encrypted)
    .bind(hint)
    .bind(json_bind(req.headers_json.as_ref())?)
    .bind(json_bind(req.query_template_json.as_ref())?)
    .bind(json_bind(req.response_mapping_json.as_ref())?)
    .bind(req.timeout_secs.unwrap_or(12).clamp(1, 60))
    .bind(req.max_results.unwrap_or(10).clamp(1, 40))
    .bind(req.fetch_content_enabled.unwrap_or(true))
    .bind(
        req.content_extract_mode
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("auto"),
    )
    .bind(json_bind(req.domain_allowlist_json.as_ref())?)
    .bind(json_bind(req.domain_blocklist_json.as_ref())?)
    .bind(json_bind(req.rate_limit_json.as_ref())?)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    notify_search_providers_changed(&state, &claims.tenant_id).await;
    let row = load_pm_search_provider_row(&state, &claims.tenant_id, &id).await?;
    Ok(Json(row_to_search_provider_record(&row)))
}

async fn update_search_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<PmSearchProviderUpsertRequest>,
) -> Result<Json<PmSearchProviderRecord>, AppError> {
    validate_pm_search_provider_secret_ref(&req)?;
    let existing = load_pm_search_provider_row(&state, &claims.tenant_id, &id).await?;
    let provider_type = match req.provider_type.as_deref() {
        Some(value) => normalize_provider_type(value)?,
        None => existing.get("provider_type"),
    };
    let existing_method: String = existing.get("method");
    let method = normalize_provider_method(req.method.as_deref(), &existing_method)?;
    let encrypted = req
        .auth_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(agent_gateway::encrypt)
        .transpose()
        .map_err(|error| AppError::Internal(format!("encryption failed: {error}")))?;
    let hint = req.auth_secret.as_deref().and_then(key_hint);
    sqlx::query(
        r#"
        UPDATE pm_search_provider_configs SET
          name = COALESCE(?, name),
          provider_type = ?,
          enabled = COALESCE(?, enabled),
          priority = COALESCE(?, priority),
          base_url = COALESCE(?, base_url),
          method = ?,
          auth_type = COALESCE(?, auth_type),
          auth_secret_ref = COALESCE(?, auth_secret_ref),
          auth_secret_ciphertext = COALESCE(?, auth_secret_ciphertext),
          key_hint = COALESCE(?, key_hint),
          headers_json = COALESCE(?, headers_json),
          query_template_json = COALESCE(?, query_template_json),
          response_mapping_json = COALESCE(?, response_mapping_json),
          timeout_secs = COALESCE(?, timeout_secs),
          max_results = COALESCE(?, max_results),
          fetch_content_enabled = COALESCE(?, fetch_content_enabled),
          content_extract_mode = COALESCE(?, content_extract_mode),
          domain_allowlist_json = COALESCE(?, domain_allowlist_json),
          domain_blocklist_json = COALESCE(?, domain_blocklist_json),
          rate_limit_json = COALESCE(?, rate_limit_json)
        WHERE tenant_id = ? AND id = ?
        "#,
    )
    .bind(req.name.as_deref().map(str::trim).filter(|v| !v.is_empty()))
    .bind(provider_type)
    .bind(req.enabled)
    .bind(req.priority)
    .bind(
        req.base_url
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(method)
    .bind(
        req.auth_type
            .as_deref()
            .map(|value| normalize_auth_type(Some(value))),
    )
    .bind(req.auth_secret_ref)
    .bind(encrypted)
    .bind(hint)
    .bind(json_bind(req.headers_json.as_ref())?)
    .bind(json_bind(req.query_template_json.as_ref())?)
    .bind(json_bind(req.response_mapping_json.as_ref())?)
    .bind(req.timeout_secs.map(|v| v.clamp(1, 60)))
    .bind(req.max_results.map(|v| v.clamp(1, 40)))
    .bind(req.fetch_content_enabled)
    .bind(
        req.content_extract_mode
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(json_bind(req.domain_allowlist_json.as_ref())?)
    .bind(json_bind(req.domain_blocklist_json.as_ref())?)
    .bind(json_bind(req.rate_limit_json.as_ref())?)
    .bind(&claims.tenant_id)
    .bind(&id)
    .execute(&state.db)
    .await?;
    notify_search_providers_changed(&state, &claims.tenant_id).await;
    let row = load_pm_search_provider_row(&state, &claims.tenant_id, &id).await?;
    Ok(Json(row_to_search_provider_record(&row)))
}

async fn delete_search_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let result =
        sqlx::query("DELETE FROM pm_search_provider_configs WHERE tenant_id = ? AND id = ?")
            .bind(&claims.tenant_id)
            .bind(&id)
            .execute(&state.db)
            .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("search provider not found".into()));
    }
    notify_search_providers_changed(&state, &claims.tenant_id).await;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn reorder_search_providers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PmSearchProviderReorderRequest>,
) -> Result<Json<Value>, AppError> {
    for (idx, provider_id) in req.provider_ids.iter().enumerate() {
        let priority = i32::try_from((idx + 1) * 10).unwrap_or(i32::MAX);
        sqlx::query(
            "UPDATE pm_search_provider_configs SET priority = ? WHERE tenant_id = ? AND id = ?",
        )
        .bind(priority)
        .bind(&claims.tenant_id)
        .bind(provider_id)
        .execute(&state.db)
        .await?;
    }
    notify_search_providers_changed(&state, &claims.tenant_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn run_search_with_provider_config(
    provider: tools::WebSearchProviderConfig,
    query: &str,
) -> Result<Value, String> {
    let output = tools::with_strict_web_search_provider_override(vec![provider], || {
        tools::execute_tool("WebSearch", &serde_json::json!({ "query": query }))
    })?;
    serde_json::from_str(&output).map_err(|error| error.to_string())
}

fn extract_search_result_count(output: &Value) -> usize {
    output
        .get("results")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("content").and_then(Value::as_array))
                .map(Vec::len)
                .sum()
        })
        .unwrap_or(0)
}

async fn test_search_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<PmSearchProviderTestResponse>, AppError> {
    let row = load_pm_search_provider_row(&state, &claims.tenant_id, &id).await?;
    let provider = row_to_tool_provider_config(&row, Some(3))?;
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        run_search_with_provider_config(provider, "AOS search provider health check")
    })
    .await
    .map_err(|error| AppError::Internal(format!("search provider test join failed: {error}")))?;
    let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    match result {
        Ok(output) => {
            let result_count = extract_search_result_count(&output);
            let provider_trace = output
                .get("providerTrace")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            sqlx::query(
                "UPDATE pm_search_provider_configs SET health_status = 'healthy', last_error = NULL WHERE tenant_id = ? AND id = ?",
            )
            .bind(&claims.tenant_id)
            .bind(&id)
            .execute(&state.db)
            .await?;
            notify_search_providers_changed(&state, &claims.tenant_id).await;
            Ok(Json(PmSearchProviderTestResponse {
                ok: true,
                latency_ms,
                result_count,
                error: None,
                provider_trace,
            }))
        }
        Err(error) => {
            sqlx::query(
                "UPDATE pm_search_provider_configs SET health_status = 'unhealthy', last_error = ? WHERE tenant_id = ? AND id = ?",
            )
            .bind(&error)
            .bind(&claims.tenant_id)
            .bind(&id)
            .execute(&state.db)
            .await?;
            notify_search_providers_changed(&state, &claims.tenant_id).await;
            Ok(Json(PmSearchProviderTestResponse {
                ok: false,
                latency_ms,
                result_count: 0,
                error: Some(error),
                provider_trace: Vec::new(),
            }))
        }
    }
}

async fn search_doctor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<PmSearchDoctorResponse>, AppError> {
    let snapshot =
        crate::routes::search_orchestrator_runtime::build_unified_search_capability_snapshot(
            &state,
            &claims.tenant_id,
            None,
            true,
            true,
        )
        .await;
    let native_search = PmSearchLayerStatus {
        available: snapshot.native_search.available,
        status: snapshot.native_search.status.clone(),
        detail: snapshot.native_search.detail.clone(),
    };
    let builtin_web_search = PmSearchLayerStatus {
        available: snapshot.builtin_web_search.available,
        status: snapshot.builtin_web_search.status.clone(),
        detail: snapshot.builtin_web_search.detail.clone(),
    };
    let mcp_search = PmSearchLayerStatus {
        available: snapshot.mcp_search.available,
        status: snapshot.mcp_search.status.clone(),
        detail: snapshot.mcp_search.detail.clone(),
    };
    let rag_local = PmSearchLayerStatus {
        available: snapshot.rag_local.available,
        status: snapshot.rag_local.status.clone(),
        detail: snapshot.rag_local.detail.clone(),
    };
    let configured_providers = snapshot
        .configured_providers
        .iter()
        .map(|provider| PmSearchProviderHealth {
            id: provider.id.clone(),
            name: provider.name.clone(),
            provider_type: provider.provider_type.clone(),
            enabled: provider.enabled,
            priority: provider.priority,
            health_status: provider.health_status.clone(),
            has_secret: provider.has_secret,
            last_error: provider.last_error.clone(),
        })
        .collect::<Vec<_>>();
    Ok(Json(PmSearchDoctorResponse {
        effective_order: snapshot.effective_order.clone(),
        degraded_reason: snapshot.degraded_reason.clone(),
        orchestrator: snapshot.orchestrator,
        builtin_web_search,
        native_search,
        mcp_search,
        configured_providers,
        rag_local,
    }))
}

async fn search_capabilities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<PmSearchDoctorResponse>, AppError> {
    search_doctor(State(state), Extension(claims)).await
}

async fn search_query(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PmSearchQueryRequest>,
) -> Result<Json<PmSearchQueryResponse>, AppError> {
    let query = req.query.trim();
    if query.is_empty() {
        return Err(AppError::ValidationError("query is required".into()));
    }
    let rows = if let Some(provider_id) = req.provider_id.as_deref() {
        let row = load_pm_search_provider_row(&state, &claims.tenant_id, provider_id).await?;
        if !search_provider_row_is_runnable(&row) {
            return Ok(Json(PmSearchQueryResponse {
                ok: false,
                output: None,
                error: Some(
                    "configured search provider is disabled or marked unhealthy".to_string(),
                ),
            }));
        }
        vec![row]
    } else {
        load_pm_search_provider_rows(&state, &claims.tenant_id)
            .await?
            .into_iter()
            .filter(search_provider_row_is_runnable)
            .collect::<Vec<_>>()
    };
    if rows.is_empty() {
        return Ok(Json(PmSearchQueryResponse {
            ok: false,
            output: None,
            error: Some("no enabled configured search provider".to_string()),
        }));
    }
    let providers = rows
        .iter()
        .map(|row| row_to_tool_provider_config(row, req.max_results))
        .collect::<Result<Vec<_>, _>>()?;
    let query_owned = query.to_string();
    let result = tokio::task::spawn_blocking(move || {
        tools::with_strict_web_search_provider_override(providers, || {
            tools::execute_tool("WebSearch", &serde_json::json!({ "query": query_owned }))
        })
    })
    .await
    .map_err(|error| AppError::Internal(format!("search query join failed: {error}")))?;
    match result {
        Ok(output) => Ok(Json(PmSearchQueryResponse {
            ok: true,
            output: serde_json::from_str(&output).ok(),
            error: None,
        })),
        Err(error) => Ok(Json(PmSearchQueryResponse {
            ok: false,
            output: None,
            error: Some(error),
        })),
    }
}

async fn report_extract(
    Json(req): Json<PmReportTextRequest>,
) -> Result<Json<PmReportExtractResponse>, AppError> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(AppError::ValidationError("text is required".into()));
    }
    let signal = detect_pm_report_strategy_signal(text);
    Ok(Json(PmReportExtractResponse {
        mode: if signal.matched {
            "business_report_strategy"
        } else {
            "general_pm_question"
        }
        .to_string(),
        matched: signal.matched,
        score: signal.score,
        reasons: signal.reasons,
        primary_terms: signal.primary_terms,
        first_party_evidence: extract_pm_first_party_evidence(text),
    }))
}

async fn report_search_plan(
    Json(req): Json<PmReportTextRequest>,
) -> Result<Json<PmReportSearchPlanResponse>, AppError> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(AppError::ValidationError("text is required".into()));
    }
    let signal = detect_pm_report_strategy_signal(text);
    Ok(Json(PmReportSearchPlanResponse {
        mode: if signal.matched {
            "business_report_strategy"
        } else {
            "general_pm_question"
        }
        .to_string(),
        matched: signal.matched,
        targeted_queries: signal.targeted_queries,
        fallback_order: pm_search_fallback_keys(),
        first_party_is_primary: true,
    }))
}

fn pm_answer_contains_tool_diagnostics(answer: &str) -> bool {
    [
        "durationMs",
        "duration_seconds",
        "toolCallCount",
        "providerTrace",
        "contentChars",
        "sourceSlotBudgetSecs",
        "pipelineTimeoutSecs",
        "routeAllowlist",
    ]
    .iter()
    .any(|token| answer.contains(token))
}

async fn quality_check(
    Json(req): Json<PmQualityCheckRequest>,
) -> Result<Json<PmQualityCheckResponse>, AppError> {
    let question = req.question.trim();
    let answer = req.answer.trim();
    if question.is_empty() || answer.is_empty() {
        return Err(AppError::ValidationError(
            "question and answer are required".into(),
        ));
    }
    let signal = detect_pm_report_strategy_signal(question);
    let mut missing = Vec::<String>::new();
    let answer_lower = answer.to_ascii_lowercase();
    if pm_answer_contains_tool_diagnostics(answer) {
        missing.push("tool_diagnostic_leaked_into_answer".to_string());
    }
    if signal.matched {
        let first_party = extract_pm_first_party_evidence(question);
        let metrics = first_party
            .get("metrics")
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0);
        let metric_mentions = ["roi", "roas", "aipu", "ecpm", "arpu", "dau"]
            .iter()
            .filter(|token| answer_lower.contains(**token))
            .count();
        if metric_mentions < metrics.min(3).max(2) {
            missing.push("insufficient_first_party_metric_usage".to_string());
        }
        if !(answer_lower.contains("ecpm") && answer_lower.contains("aipu")
            || answer.contains("分层")
            || answer.contains("人群")
            || answer_lower.contains("segment"))
        {
            missing.push("missing_segment_level_strategy".to_string());
        }
        if !(answer.contains("实验")
            || answer.contains("灰度")
            || answer_lower.contains("experiment")
            || answer_lower.contains("rollout"))
        {
            missing.push("missing_experiment_ready_plan".to_string());
        }
        if !(answer.contains("保护指标")
            || answer.contains("停止")
            || answer_lower.contains("guardrail")
            || answer_lower.contains("kill"))
        {
            missing.push("missing_guardrails_or_kill_criteria".to_string());
        }
    }
    let passed = missing.is_empty();
    Ok(Json(PmQualityCheckResponse {
        passed,
        matched: signal.matched,
        missing_checks: missing,
        notes: vec![if signal.matched {
            "first-party report strategy quality checks applied".to_string()
        } else {
            "generic PM quality checks applied".to_string()
        }],
    }))
}

async fn research_run_search_trace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> Result<Json<PmResearchTraceResponse>, AppError> {
    let run_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pm_research_runs WHERE tenant_id = ? AND run_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&run_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if run_exists == 0 {
        return Err(AppError::NotFound("research run not found".into()));
    }
    let stage_rows = sqlx::query(
        r#"
        SELECT id, stage, attempt_no, status, strategy, route_key, channel, variant,
               elapsed_ms, CAST(detail_json AS TEXT) AS detail_json, error_code, error_message,
               started_at, ended_at, created_at
        FROM pm_research_stage_attempts
        WHERE run_id = ?
        ORDER BY id ASC
        "#,
    )
    .bind(&run_id)
    .fetch_all(&state.db)
    .await?;
    let stages = stage_rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.get::<u64, _>("id"),
                "stage": row.get::<String, _>("stage"),
                "attemptNo": row.get::<i32, _>("attempt_no"),
                "status": row.get::<String, _>("status"),
                "strategy": row.get::<Option<String>, _>("strategy"),
                "routeKey": row.get::<Option<String>, _>("route_key"),
                "channel": row.get::<Option<String>, _>("channel"),
                "variant": row.get::<Option<String>, _>("variant"),
                "elapsedMs": row.get::<Option<i64>, _>("elapsed_ms"),
                "detail": parse_json_str(row.get::<Option<String>, _>("detail_json")),
                "errorCode": row.get::<Option<String>, _>("error_code"),
                "errorMessage": row.get::<Option<String>, _>("error_message"),
                "startedAt": row.get::<Option<String>, _>("started_at"),
                "endedAt": row.get::<Option<String>, _>("ended_at"),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    let tool_rows = sqlx::query(
        r#"
        SELECT CAST(call_seq AS INTEGER) AS call_seq,
               tool_name, input_preview, output_preview, is_error, error_code,
               error_message, latency_ms, route_key, channel, url, domain, created_at
        FROM pm_research_tool_call_ledger
        WHERE run_id = ?
        ORDER BY call_seq ASC
        "#,
    )
    .bind(&run_id)
    .fetch_all(&state.db)
    .await?;
    let tool_calls = tool_rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "callSeq": row.get::<i64, _>("call_seq"),
                "toolName": row.get::<String, _>("tool_name"),
                "inputPreview": row.get::<Option<String>, _>("input_preview"),
                "outputPreview": row.get::<Option<String>, _>("output_preview"),
                "isError": row.get::<bool, _>("is_error"),
                "errorCode": row.get::<Option<String>, _>("error_code"),
                "errorMessage": row.get::<Option<String>, _>("error_message"),
                "latencyMs": row.get::<Option<i64>, _>("latency_ms"),
                "routeKey": row.get::<Option<String>, _>("route_key"),
                "channel": row.get::<Option<String>, _>("channel"),
                "url": row.get::<Option<String>, _>("url"),
                "domain": row.get::<Option<String>, _>("domain"),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(PmResearchTraceResponse {
        run_id,
        stages,
        tool_calls,
    }))
}

fn sanitized_evidence_excerpt(input: Option<String>) -> Option<String> {
    let input = input?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if pm_answer_contains_tool_diagnostics(trimmed) {
        return None;
    }
    Some(trimmed.chars().take(1200).collect())
}

async fn research_run_evidence(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> Result<Json<PmResearchEvidenceResponse>, AppError> {
    let run_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pm_research_runs WHERE tenant_id = ? AND run_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&run_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if run_exists == 0 {
        return Err(AppError::NotFound("research run not found".into()));
    }
    let rows = sqlx::query(
        r#"
        SELECT CAST(call_seq AS INTEGER) AS call_seq,
               tool_name, input_preview, output_preview, is_error,
               route_key, channel, url, domain, created_at
        FROM pm_research_tool_call_ledger
        WHERE run_id = ? AND is_error = 0
        ORDER BY call_seq ASC
        "#,
    )
    .bind(&run_id)
    .fetch_all(&state.db)
    .await?;
    let evidence = rows
        .into_iter()
        .filter_map(|row| {
            let excerpt =
                sanitized_evidence_excerpt(row.get::<Option<String>, _>("output_preview"))?;
            Some(serde_json::json!({
                "sourceType": "search_tool",
                "sourceName": row.get::<String, _>("tool_name"),
                "query": row.get::<Option<String>, _>("input_preview"),
                "excerpt": excerpt,
                "url": row.get::<Option<String>, _>("url"),
                "domain": row.get::<Option<String>, _>("domain"),
                "routeKey": row.get::<Option<String>, _>("route_key"),
                "channel": row.get::<Option<String>, _>("channel"),
                "createdAt": row.get::<String, _>("created_at"),
            }))
        })
        .collect::<Vec<_>>();
    Ok(Json(PmResearchEvidenceResponse { run_id, evidence }))
}

// Legacy PM chat session persistence was removed with legacy PM tables.

async fn chat(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PmChatRequest>,
) -> impl IntoResponse {
    if req.messages.is_empty() {
        return AppError::ValidationError("messages cannot be empty".into()).into_response();
    }
    let tenant_id = claims.tenant_id.clone();
    let user_id = claims.sub.clone();
    let model = req.model.unwrap_or_else(|| state.default_model.clone());
    let message_hook = match run_lifecycle_hooks(
        &state,
        &tenant_id,
        "pm",
        HookEventType::MessageReceived,
        "pm.chat",
        serde_json::json!({
            "model": &model,
            "userId": &user_id,
            "messages": &req.messages,
        }),
        None,
        false,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => return e.into_response(),
    };
    if let Some(error) = hook_blocking_error("message_received", &message_hook) {
        return error.into_response();
    }
    let mut result =
        match run_pm_chat_completion(&state, &tenant_id, &user_id, model.clone(), req.messages)
            .await
        {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        };
    let final_hook = match run_lifecycle_hooks(
        &state,
        &tenant_id,
        "pm",
        HookEventType::BeforeFinalAnswer,
        "pm.final_answer",
        serde_json::json!({
            "model": &model,
            "userId": &user_id,
            "usage": &result.usage,
            "appliedRules": &result.applied_rules,
        }),
        Some(serde_json::json!({
            "answer": &result.answer,
        })),
        false,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => return e.into_response(),
    };
    if let Some(error) = hook_blocking_error("before_final_answer", &final_hook) {
        return error.into_response();
    }
    if let Some(updated_answer) = hook_updated_answer(&final_hook) {
        result.answer = updated_answer;
    }
    let usage_record = crate::routes::chat::TokenUsageRecord {
        tenant_id: tenant_id.clone(),
        user_id: user_id.clone(),
        session_id: "pm-copilot".to_string(),
        request_id: None,
        model: result.usage.model.clone(),
        input_tokens: result.usage.input_tokens,
        output_tokens: result.usage.output_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        total_tokens: result.usage.total_tokens,
        estimated_cost_usd: result.usage.estimated_cost_usd,
        api_key_id: Some(result.api_key_id),
        provider: result.provider_name,
        created_at: Utc::now(),
    };
    let _ = state.usage_writer().write(&usage_record).await;
    match run_lifecycle_hooks(
        &state,
        &tenant_id,
        "pm",
        HookEventType::AfterFinalAnswer,
        "pm.final_answer",
        serde_json::json!({
            "model": &model,
            "userId": &user_id,
            "usage": &result.usage,
            "appliedRules": &result.applied_rules,
        }),
        Some(serde_json::json!({
            "answer": &result.answer,
        })),
        false,
    )
    .await
    {
        Ok(hook_result) if hook_result.is_failed() || hook_result.is_cancelled() => {
            tracing::warn!(
                tenant_id = %tenant_id,
                user_id = %user_id,
                "after_final_answer hook completed with warning: {}",
                hook_result.messages().join("\n")
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                user_id = %user_id,
                error = %error,
                "after_final_answer hook failed to execute"
            );
        }
    }
    Json(PmChatResponse {
        answer: result.answer,
        usage: Some(result.usage),
        applied_rules: result.applied_rules,
    })
    .into_response()
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/chat", routing_post(chat))
        .route(
            "/search/providers",
            routing_get(list_search_providers).post(create_search_provider),
        )
        .route(
            "/search/providers/reorder",
            routing_post(reorder_search_providers),
        )
        .route(
            "/search/providers/{id}",
            routing_patch(update_search_provider).delete(delete_search_provider),
        )
        .route(
            "/search/providers/{id}/test",
            routing_post(test_search_provider),
        )
        .route("/search/doctor", routing_get(search_doctor))
        .route("/search/capabilities", routing_get(search_capabilities))
        .route("/search/query", routing_post(search_query))
        .route("/report/extract", routing_post(report_extract))
        .route("/report/search-plan", routing_post(report_search_plan))
        .route("/quality-check", routing_post(quality_check))
        .route(
            "/research-runs/{id}/search-trace",
            routing_get(research_run_search_trace),
        )
        .route(
            "/research-runs/{id}/evidence",
            routing_get(research_run_evidence),
        )
        .route("/cron/preview", routing_get(preview_mission_cron))
        .route("/missions", routing_get(list_missions).post(create_mission))
        .route("/missions/summary", routing_get(mission_summary))
        .route("/missions/{id}/run-now", routing_post(run_mission_now))
        .route(
            "/missions/{id}/task-runs",
            routing_get(list_mission_task_runs),
        )
        .route(
            "/missions/{id}/task-runs/{task_id}/events",
            routing_get(list_mission_task_events),
        )
        .route(
            "/missions/{id}",
            routing_patch(update_mission).delete(delete_mission),
        )
        .route(
            "/material-jobs",
            routing_get(list_material_jobs).post(create_material_job),
        )
        .route("/material-jobs/summary", routing_get(material_jobs_summary))
        .route("/material-threads", routing_get(list_material_threads))
        .route(
            "/material-threads/{id}",
            routing_delete(delete_material_thread),
        )
        .route("/material-jobs/{id}", routing_delete(delete_material_job))
        .route("/material-models", routing_get(list_material_models))
        .route(
            "/material-jobs/{id}/assets",
            routing_get(list_material_assets),
        )
        .route(
            "/material-assets/{id}/export",
            routing_post(export_material_asset),
        )
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}
