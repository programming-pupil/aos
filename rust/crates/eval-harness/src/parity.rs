//! Real AOS/Codex side-by-side runner.
//!
//! Unlike the deterministic synthetic smoke harness, this module invokes real
//! adapters, persists every raw trace, and emits an anonymized packet for human
//! blind review. It deliberately does not manufacture a correctness score.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParityDataset {
    pub name: String,
    pub seed: u64,
    #[serde(default = "default_repetitions")]
    pub repetitions: usize,
    pub families: Vec<ParityFamily>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParityFamily {
    pub category: String,
    pub expected_count: usize,
    pub prompts: Vec<String>,
    pub variants: Vec<ParityVariant>,
    #[serde(default)]
    pub fixture_profile: Option<String>,
    #[serde(default)]
    pub expected_tools: Vec<String>,
    #[serde(default)]
    pub rubric: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParityVariant {
    pub id: String,
    #[serde(default)]
    pub suffix: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpandedParityCase {
    pub case_id: String,
    pub category: String,
    pub prompt: String,
    pub fixture_profile: Option<String>,
    pub expected_tools: Vec<String>,
    pub rubric: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum AdapterConfig {
    Command(CommandAdapterConfig),
    AosHttp(AosHttpAdapterConfig),
    CodexCli(CodexCliAdapterConfig),
    Recorded(RecordedAdapterConfig),
}

/// Deterministic adapter outcomes exported from an explicitly reviewed trace
/// fixture. This mode exercises the complete parity/blinding pipeline without
/// network access; it never assigns or manufactures a correctness score.
#[derive(Debug, Clone)]
pub struct RecordedAdapterConfig {
    pub model: String,
    pub outcomes: BTreeMap<String, AdapterOutcome>,
}

#[derive(Debug, Clone)]
pub struct CommandAdapterConfig {
    pub program: String,
    pub args: Vec<String>,
    pub model: String,
    pub reasoning_effort: String,
    pub fixture_root: PathBuf,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct AosHttpAdapterConfig {
    pub base_url: String,
    pub bearer_token: String,
    pub model: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct CodexCliAdapterConfig {
    pub program: String,
    pub model: String,
    pub reasoning_effort: String,
    pub fixture_root: PathBuf,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ParityRunConfig {
    pub aos: AdapterConfig,
    pub codex: AdapterConfig,
    pub output_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdapterRequest<'a> {
    case_id: &'a str,
    category: &'a str,
    prompt: &'a str,
    model: &'a str,
    reasoning_effort: &'a str,
    seed: u64,
    repetition: usize,
    fixture_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterOutcome {
    pub completed: bool,
    pub answer: String,
    pub raw_trace: String,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BlindPair {
    case_id: String,
    category: String,
    repetition: usize,
    prompt: String,
    answer_a: String,
    answer_b: String,
    rubric: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HiddenPairKey {
    case_id: String,
    repetition: usize,
    answer_a_source: String,
    answer_b_source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParityRunSummary {
    pub dataset: String,
    pub seed: u64,
    pub case_count: usize,
    pub repetitions: usize,
    pub attempted_adapter_runs: usize,
    pub completed_adapter_runs: usize,
    pub failed_adapter_runs: usize,
    pub output_directory: String,
    pub correctness_status: String,
}

fn default_repetitions() -> usize {
    3
}

pub fn load_parity_dataset(path: &Path) -> Result<ParityDataset, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("failed to read parity dataset {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse parity dataset {}: {error}", path.display()))
}

pub fn expand_parity_cases(dataset: &ParityDataset) -> Result<Vec<ExpandedParityCase>, String> {
    if dataset.repetitions != 3 {
        return Err("parity dataset must run every case exactly three times".to_string());
    }
    let mut cases = Vec::new();
    for family in &dataset.families {
        let actual_count = family.prompts.len().saturating_mul(family.variants.len());
        if actual_count != family.expected_count {
            return Err(format!(
                "family {} expands to {actual_count} cases, expected {}",
                family.category, family.expected_count
            ));
        }
        for (prompt_index, prompt) in family.prompts.iter().enumerate() {
            for variant in &family.variants {
                cases.push(ExpandedParityCase {
                    case_id: format!("{}-{:02}-{}", family.category, prompt_index + 1, variant.id),
                    category: family.category.clone(),
                    prompt: format!("{}{}", prompt.trim(), variant.suffix),
                    fixture_profile: family.fixture_profile.clone(),
                    expected_tools: family.expected_tools.clone(),
                    rubric: family.rubric.clone(),
                });
            }
        }
    }
    if cases.len() != 180 {
        return Err(format!(
            "parity dataset must expand to exactly 180 cases, got {}",
            cases.len()
        ));
    }
    let unique = cases
        .iter()
        .map(|case| case.case_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if unique.len() != cases.len() {
        return Err("parity dataset contains duplicate case IDs".to_string());
    }
    Ok(cases)
}

pub async fn run_real_parity(
    dataset: &ParityDataset,
    config: &ParityRunConfig,
) -> Result<ParityRunSummary, String> {
    let cases = expand_parity_cases(dataset)?;
    let run_id = format!(
        "{}-{}",
        dataset.seed,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let output_directory = config.output_root.join(run_id);
    std::fs::create_dir_all(output_directory.join("raw/aos"))
        .map_err(|error| format!("failed to create AOS trace directory: {error}"))?;
    std::fs::create_dir_all(output_directory.join("raw/codex"))
        .map_err(|error| format!("failed to create Codex trace directory: {error}"))?;

    let mut blind_pairs = Vec::new();
    let mut hidden_keys = Vec::new();
    let mut metrics = Vec::new();
    let mut completed = 0_usize;
    let mut failed = 0_usize;
    for case in &cases {
        for repetition in 1..=dataset.repetitions {
            let aos = run_adapter(&config.aos, case, dataset.seed, repetition).await;
            let codex = run_adapter(&config.codex, case, dataset.seed, repetition).await;
            persist_raw_outcome(&output_directory, "aos", case, repetition, &aos)?;
            persist_raw_outcome(&output_directory, "codex", case, repetition, &codex)?;
            completed += usize::from(aos.completed) + usize::from(codex.completed);
            failed += usize::from(!aos.completed) + usize::from(!codex.completed);

            let swap = blind_swap(dataset.seed, &case.case_id, repetition);
            let (answer_a, answer_b, source_a, source_b) = if swap {
                (&codex.answer, &aos.answer, "codex", "aos")
            } else {
                (&aos.answer, &codex.answer, "aos", "codex")
            };
            blind_pairs.push(BlindPair {
                case_id: case.case_id.clone(),
                category: case.category.clone(),
                repetition,
                prompt: case.prompt.clone(),
                answer_a: answer_a.clone(),
                answer_b: answer_b.clone(),
                rubric: case.rubric.clone(),
            });
            hidden_keys.push(HiddenPairKey {
                case_id: case.case_id.clone(),
                repetition,
                answer_a_source: source_a.to_string(),
                answer_b_source: source_b.to_string(),
            });
            metrics.push(json!({
                "caseId": case.case_id,
                "category": case.category,
                "repetition": repetition,
                "expectedTools": case.expected_tools,
                "aos": {"completed": aos.completed, "elapsedMs": aos.elapsed_ms, "error": aos.error},
                "codex": {"completed": codex.completed, "elapsedMs": codex.elapsed_ms, "error": codex.error},
            }));
        }
    }
    write_pretty_json(output_directory.join("blind-review.json"), &blind_pairs)?;
    write_pretty_json(output_directory.join("blind-key.json"), &hidden_keys)?;
    write_pretty_json(output_directory.join("operational-metrics.json"), &metrics)?;
    let summary = ParityRunSummary {
        dataset: dataset.name.clone(),
        seed: dataset.seed,
        case_count: cases.len(),
        repetitions: dataset.repetitions,
        attempted_adapter_runs: cases.len() * dataset.repetitions * 2,
        completed_adapter_runs: completed,
        failed_adapter_runs: failed,
        output_directory: output_directory.display().to_string(),
        correctness_status: "pending_blind_review".to_string(),
    };
    write_pretty_json(output_directory.join("summary.json"), &summary)?;
    Ok(summary)
}

async fn run_adapter(
    config: &AdapterConfig,
    case: &ExpandedParityCase,
    seed: u64,
    repetition: usize,
) -> AdapterOutcome {
    match config {
        AdapterConfig::Command(config) => run_command_adapter(config, case, seed, repetition).await,
        AdapterConfig::AosHttp(config) => {
            run_aos_http_adapter(config, case, seed, repetition).await
        }
        AdapterConfig::CodexCli(config) => run_codex_cli_adapter(config, case).await,
        AdapterConfig::Recorded(config) => config
            .outcomes
            .get(&recorded_outcome_key(&case.case_id, repetition))
            .cloned()
            .unwrap_or_else(|| AdapterOutcome {
                completed: false,
                answer: String::new(),
                raw_trace: String::new(),
                elapsed_ms: 0,
                error: Some(format!(
                    "recorded adapter {} has no outcome for {} repetition {}",
                    config.model, case.case_id, repetition
                )),
            }),
    }
}

fn recorded_outcome_key(case_id: &str, repetition: usize) -> String {
    format!("{case_id}#{repetition}")
}

async fn run_codex_cli_adapter(
    config: &CodexCliAdapterConfig,
    case: &ExpandedParityCase,
) -> AdapterOutcome {
    let started = Instant::now();
    let fixture_path = case
        .fixture_profile
        .as_ref()
        .map(|profile| config.fixture_root.join(profile))
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| config.fixture_root.clone());
    let reasoning = format!("model_reasoning_effort=\"{}\"", config.reasoning_effort);
    let mut command = tokio::process::Command::new(&config.program);
    command
        .arg("exec")
        .arg("--json")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("--model")
        .arg(&config.model)
        .arg("--config")
        .arg(reasoning)
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("--cd")
        .arg(&fixture_path)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed_outcome(started, format!("start Codex CLI: {error}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(case.prompt.as_bytes()).await {
            return failed_outcome(started, format!("write Codex prompt: {error}"));
        }
    }
    let output = match tokio::time::timeout(config.timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return failed_outcome(started, format!("wait for Codex CLI: {error}")),
        Err(_) => return failed_outcome(started, "Codex CLI timed out".to_string()),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let raw_trace = format!("{stdout}\n--- STDERR ---\n{stderr}");
    if !output.status.success() {
        return AdapterOutcome {
            completed: false,
            answer: String::new(),
            raw_trace,
            elapsed_ms: elapsed_ms(started),
            error: Some(format!("Codex CLI exited with {}", output.status)),
        };
    }
    let answer = extract_answer(&stdout);
    AdapterOutcome {
        completed: !answer.trim().is_empty(),
        error: answer
            .trim()
            .is_empty()
            .then(|| "Codex CLI returned no final answer".to_string()),
        answer,
        raw_trace,
        elapsed_ms: elapsed_ms(started),
    }
}

async fn run_command_adapter(
    config: &CommandAdapterConfig,
    case: &ExpandedParityCase,
    seed: u64,
    repetition: usize,
) -> AdapterOutcome {
    let started = Instant::now();
    let fixture_path = case
        .fixture_profile
        .as_ref()
        .map(|profile| config.fixture_root.join(profile));
    let request = AdapterRequest {
        case_id: &case.case_id,
        category: &case.category,
        prompt: &case.prompt,
        model: &config.model,
        reasoning_effort: &config.reasoning_effort,
        seed,
        repetition,
        fixture_path: fixture_path.as_ref().map(|path| path.display().to_string()),
    };
    let payload = match serde_json::to_vec(&request) {
        Ok(payload) => payload,
        Err(error) => return failed_outcome(started, format!("serialize request: {error}")),
    };
    let mut command = tokio::process::Command::new(&config.program);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(path) = fixture_path.as_ref().filter(|path| path.is_dir()) {
        command.current_dir(path);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed_outcome(started, format!("start adapter: {error}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(&payload).await {
            return failed_outcome(started, format!("write adapter input: {error}"));
        }
    }
    let output = match tokio::time::timeout(config.timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return failed_outcome(started, format!("wait for adapter: {error}")),
        Err(_) => return failed_outcome(started, "adapter timed out".to_string()),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let raw_trace = format!("{stdout}\n--- STDERR ---\n{stderr}");
    if !output.status.success() {
        return AdapterOutcome {
            completed: false,
            answer: String::new(),
            raw_trace,
            elapsed_ms: elapsed_ms(started),
            error: Some(format!("adapter exited with {}", output.status)),
        };
    }
    let answer = extract_answer(&stdout);
    AdapterOutcome {
        completed: !answer.trim().is_empty(),
        error: answer
            .trim()
            .is_empty()
            .then(|| "adapter returned no final answer".to_string()),
        answer,
        raw_trace,
        elapsed_ms: elapsed_ms(started),
    }
}

async fn run_aos_http_adapter(
    config: &AosHttpAdapterConfig,
    case: &ExpandedParityCase,
    _seed: u64,
    _repetition: usize,
) -> AdapterOutcome {
    let started = Instant::now();
    let client = match reqwest::Client::builder().timeout(config.timeout).build() {
        Ok(client) => client,
        Err(error) => return failed_outcome(started, format!("build HTTP client: {error}")),
    };
    let auth = format!("Bearer {}", config.bearer_token);
    let create_url = format!(
        "{}/api/v1/agent/sessions",
        config.base_url.trim_end_matches('/')
    );
    let create = client
        .post(create_url)
        .header(AUTHORIZATION, &auth)
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({"source": "super_assistant", "scenario": "chat", "model": config.model}))
        .send()
        .await;
    let create = match create {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            return failed_outcome(
                started,
                format!("create AOS session returned {}", response.status()),
            )
        }
        Err(error) => return failed_outcome(started, format!("create AOS session: {error}")),
    };
    let create_json: Value = match create.json().await {
        Ok(value) => value,
        Err(error) => return failed_outcome(started, format!("decode AOS session: {error}")),
    };
    let session_id = find_string(&create_json, &["sessionId", "session_id"]).unwrap_or_default();
    if session_id.is_empty() {
        return failed_outcome(
            started,
            "AOS create-session response has no session ID".to_string(),
        );
    }
    let turn_id = format!("eval-{}-{}", case.case_id, elapsed_ms(started));
    let stream_url = format!(
        "{}/api/v1/super-assistant/messages/stream",
        config.base_url.trim_end_matches('/')
    );
    let response = client
        .post(stream_url)
        .header(AUTHORIZATION, auth)
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "sessionId": session_id,
            "turnId": turn_id,
            "text": case.prompt,
            "model": config.model,
            "app": "chat"
        }))
        .send()
        .await;
    let response = match response {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            return failed_outcome(
                started,
                format!("AOS stream returned {}", response.status()),
            )
        }
        Err(error) => return failed_outcome(started, format!("start AOS stream: {error}")),
    };
    let raw_trace = match response.text().await {
        Ok(trace) => trace,
        Err(error) => return failed_outcome(started, format!("read AOS stream: {error}")),
    };
    let answer = extract_sse_final_answer(&raw_trace);
    AdapterOutcome {
        completed: !answer.trim().is_empty() && raw_trace.contains("event: stream_end"),
        error: answer
            .trim()
            .is_empty()
            .then(|| "AOS stream returned no final answer".to_string()),
        answer,
        raw_trace,
        elapsed_ms: elapsed_ms(started),
    }
}

fn extract_answer(stdout: &str) -> String {
    for line in stdout.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(answer) = find_string(
            &value,
            &[
                "finalAnswer",
                "final_answer",
                "answer",
                "text",
                "outputText",
            ],
        ) {
            if !answer.trim().is_empty() {
                return answer;
            }
        }
    }
    stdout.trim().to_string()
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(value) = object.get(*key).and_then(Value::as_str) {
                    return Some(value.to_string());
                }
            }
            object.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn extract_sse_final_answer(trace: &str) -> String {
    let mut final_answer = String::new();
    for block in trace.split("\n\n") {
        let event_type = block
            .lines()
            .find_map(|line| line.strip_prefix("event:").map(str::trim))
            .unwrap_or_default();
        if !matches!(event_type, "final_delta" | "text") {
            continue;
        }
        let data = block
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(value) = serde_json::from_str::<Value>(&data) {
            if let Some(text) = find_string(&value, &["text", "delta"]) {
                final_answer = text;
            }
        }
    }
    final_answer
}

fn blind_swap(seed: u64, case_id: &str, repetition: usize) -> bool {
    let digest = Sha256::digest(format!("{seed}:{case_id}:{repetition}").as_bytes());
    digest[0] & 1 == 1
}

fn persist_raw_outcome(
    output_directory: &Path,
    adapter: &str,
    case: &ExpandedParityCase,
    repetition: usize,
    outcome: &AdapterOutcome,
) -> Result<(), String> {
    let path = output_directory
        .join("raw")
        .join(adapter)
        .join(format!("{}-run-{repetition}.json", case.case_id));
    write_pretty_json(path, outcome)
}

fn write_pretty_json(path: PathBuf, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    std::fs::write(&path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

fn failed_outcome(started: Instant, error: String) -> AdapterOutcome {
    AdapterOutcome {
        completed: false,
        answer: String::new(),
        raw_trace: String::new(),
        elapsed_ms: elapsed_ms(started),
        error: Some(error),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub fn command_config_from_env(prefix: &str) -> Result<CommandAdapterConfig, String> {
    let program_key = format!("{prefix}_PROGRAM");
    let args_key = format!("{prefix}_ARGS_JSON");
    let model_key = format!("{prefix}_MODEL");
    let program = std::env::var(&program_key)
        .map_err(|_| format!("{program_key} must be set for a real parity run"))?;
    let args = std::env::var(&args_key)
        .ok()
        .map(|value| serde_json::from_str::<Vec<String>>(&value))
        .transpose()
        .map_err(|error| format!("{args_key} must be a JSON string array: {error}"))?
        .unwrap_or_default();
    Ok(CommandAdapterConfig {
        program,
        args,
        model: std::env::var(&model_key)
            .map_err(|_| format!("{model_key} must be set for a real parity run"))?,
        reasoning_effort: std::env::var(format!("{prefix}_REASONING_EFFORT"))
            .unwrap_or_else(|_| "high".to_string()),
        fixture_root: std::env::var(format!("{prefix}_FIXTURE_ROOT"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("../eval/fixtures")),
        timeout: Duration::from_secs(
            std::env::var(format!("{prefix}_TIMEOUT_SECS"))
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_800),
        ),
    })
}

pub fn aos_http_config_from_env() -> Result<AosHttpAdapterConfig, String> {
    Ok(AosHttpAdapterConfig {
        base_url: std::env::var("AOS_EVAL_BASE_URL")
            .map_err(|_| "AOS_EVAL_BASE_URL must be set".to_string())?,
        bearer_token: std::env::var("AOS_EVAL_BEARER_TOKEN")
            .map_err(|_| "AOS_EVAL_BEARER_TOKEN must be set".to_string())?,
        model: std::env::var("AOS_EVAL_MODEL")
            .map_err(|_| "AOS_EVAL_MODEL must be set".to_string())?,
        timeout: Duration::from_secs(
            std::env::var("AOS_EVAL_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_800),
        ),
    })
}

pub fn codex_cli_config_from_env() -> Result<CodexCliAdapterConfig, String> {
    Ok(CodexCliAdapterConfig {
        program: std::env::var("AOS_EVAL_CODEX_PROGRAM").unwrap_or_else(|_| "codex".to_string()),
        model: std::env::var("AOS_EVAL_CODEX_MODEL")
            .map_err(|_| "AOS_EVAL_CODEX_MODEL must be set".to_string())?,
        reasoning_effort: std::env::var("AOS_EVAL_CODEX_REASONING_EFFORT")
            .unwrap_or_else(|_| "high".to_string()),
        fixture_root: std::env::var("AOS_EVAL_CODEX_FIXTURE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("../eval/fixtures")),
        timeout: Duration::from_secs(
            std::env::var("AOS_EVAL_CODEX_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_800),
        ),
    })
}

pub fn category_counts(cases: &[ExpandedParityCase]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for case in cases {
        *counts.entry(case.category.clone()).or_default() += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_manifest_expands_to_the_required_180_cases() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../eval/datasets/super-assistant-parity-180.json");
        let dataset = load_parity_dataset(&path).expect("parity dataset should load");
        let cases = expand_parity_cases(&dataset).expect("parity dataset should expand");

        assert_eq!(cases.len(), 180);
        assert_eq!(
            category_counts(&cases),
            BTreeMap::from([
                ("attribution".to_string(), 20),
                ("chat".to_string(), 20),
                ("code".to_string(), 30),
                ("files_sql".to_string(), 30),
                ("long_context".to_string(), 20),
                ("nl2sql".to_string(), 30),
                ("recovery_isolation".to_string(), 10),
                ("web".to_string(), 20),
            ])
        );
    }

    #[tokio::test]
    async fn real_parity_runner_persists_traces_and_pending_blind_review() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../eval/datasets/super-assistant-parity-180.json");
        let dataset = load_parity_dataset(&path).expect("parity dataset should load");
        let cases = expand_parity_cases(&dataset).expect("parity dataset should expand");
        let recorded = |source: &str| {
            cases
                .iter()
                .flat_map(|case| {
                    (1..=dataset.repetitions).map(move |repetition| {
                        (
                            recorded_outcome_key(&case.case_id, repetition),
                            AdapterOutcome {
                                completed: true,
                                answer: format!("{source} answer for {}", case.case_id),
                                raw_trace: format!(
                                    "recorded {source} trace for {} repetition {repetition}",
                                    case.case_id
                                ),
                                elapsed_ms: repetition as u64,
                                error: None,
                            },
                        )
                    })
                })
                .collect::<BTreeMap<_, _>>()
        };
        let output_root = std::env::temp_dir().join(format!(
            "aos-real-parity-runner-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let summary = run_real_parity(
            &dataset,
            &ParityRunConfig {
                aos: AdapterConfig::Recorded(RecordedAdapterConfig {
                    model: "recorded-aos".into(),
                    outcomes: recorded("aos"),
                }),
                codex: AdapterConfig::Recorded(RecordedAdapterConfig {
                    model: "recorded-codex".into(),
                    outcomes: recorded("codex"),
                }),
                output_root: output_root.clone(),
            },
        )
        .await
        .expect("recorded parity run");
        assert_eq!(summary.case_count, 180);
        assert_eq!(summary.attempted_adapter_runs, 1_080);
        assert_eq!(summary.completed_adapter_runs, 1_080);
        assert_eq!(summary.failed_adapter_runs, 0);
        assert_eq!(summary.correctness_status, "pending_blind_review");

        let output = PathBuf::from(&summary.output_directory);
        let blind_review: Value = serde_json::from_slice(
            &std::fs::read(output.join("blind-review.json")).expect("blind review"),
        )
        .unwrap();
        let blind_key: Value = serde_json::from_slice(
            &std::fs::read(output.join("blind-key.json")).expect("blind key"),
        )
        .unwrap();
        assert_eq!(blind_review.as_array().map(Vec::len), Some(540));
        assert_eq!(blind_key.as_array().map(Vec::len), Some(540));
        assert!(output.join("raw/aos").read_dir().unwrap().count() >= 180);
        assert!(output.join("raw/codex").read_dir().unwrap().count() >= 180);
        std::fs::remove_dir_all(output_root).unwrap();
    }

    #[test]
    fn blind_assignment_is_deterministic_and_not_constant() {
        let values = (1..=20)
            .map(|run| blind_swap(42, "case", run))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            (1..=20)
                .map(|run| blind_swap(42, "case", run))
                .collect::<Vec<_>>()
        );
        assert!(values.iter().any(|value| *value));
        assert!(values.iter().any(|value| !*value));
    }

    #[test]
    fn sse_parser_uses_only_the_parent_final_answer() {
        let trace = "event: commentary\ndata: {\"text\":\"draft\"}\n\nevent: final_delta\ndata: {\"text\":\"verified\"}\n\nevent: stream_end\ndata: {}\n\n";
        assert_eq!(extract_sse_final_answer(trace), "verified");
    }
}
