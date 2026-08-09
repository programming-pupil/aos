//! CI single-shot entry for the evaluation harness (Requirement 2.6).
//!
//! Runs [`eval_harness::run_eval`] and terminates the process with an exit code
//! that reflects `overall_passed`: `0` on pass, non-zero on failure. This is the
//! binary CI systems invoke (`cargo run -p eval-harness`).
//!
//! The default run loads the checked-in `eval/datasets` fixture so CI never
//! reports a green "empty eval" run.

use std::path::PathBuf;

use eval_harness::parity::{
    aos_http_config_from_env, codex_cli_config_from_env, command_config_from_env,
    load_parity_dataset, run_real_parity, AdapterConfig, ParityRunConfig,
};
use eval_harness::{default_eval_config, exit_code_for, run_eval, EXIT_FAIL};

#[tokio::main]
async fn main() {
    if std::env::args().any(|arg| arg == "--parity") {
        let result = run_parity_command().await;
        match result {
            Ok(summary) => match serde_json::to_string_pretty(&summary) {
                Ok(value) => println!("{value}"),
                Err(error) => {
                    eprintln!("failed to serialize parity summary: {error}");
                    std::process::exit(EXIT_FAIL);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(EXIT_FAIL);
            }
        }
        return;
    }
    let config = match default_eval_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(EXIT_FAIL);
        }
    };
    let report = run_eval(&config);
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("failed to serialize eval report: {error}"),
    }
    std::process::exit(exit_code_for(&report));
}

async fn run_parity_command() -> Result<eval_harness::parity::ParityRunSummary, String> {
    let dataset_path = std::env::var("AOS_EVAL_DATASET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../eval/datasets/super-assistant-parity-180.json"));
    let dataset = load_parity_dataset(&dataset_path)?;
    let aos = if std::env::var_os("AOS_EVAL_BASE_URL").is_some() {
        AdapterConfig::AosHttp(aos_http_config_from_env()?)
    } else {
        AdapterConfig::Command(command_config_from_env("AOS_EVAL_AOS")?)
    };
    let codex = AdapterConfig::CodexCli(codex_cli_config_from_env()?);
    let output_root = std::env::var("AOS_EVAL_OUTPUT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../eval/results"));
    run_real_parity(
        &dataset,
        &ParityRunConfig {
            aos,
            codex,
            output_root,
        },
    )
    .await
}
