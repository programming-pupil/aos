//! Deterministic dataset loading for the Codex parity harness.
//!
//! The binary entry point must not run an empty plan: that would make CI look
//! green while measuring nothing. This module converts the checked-in
//! `eval/datasets/codex-parity-gaps.seed.json` fixture into the existing
//! [`EvalConfig`] / [`ScenarioPlan`] / [`EvalCase`] types, keeping the runner
//! itself pure and unchanged.

use serde::Deserialize;

use crate::runner::{CaseSpec, EvalCase, EvalConfig, ScenarioPlan};
use crate::scenario::EvalScenario;

pub const DEFAULT_DATASET_JSON: &str =
    include_str!("../../../../eval/datasets/codex-parity-gaps.seed.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dataset {
    name: String,
    seed: u64,
    #[serde(default)]
    generated_at: Option<String>,
    scenarios: Vec<DatasetScenario>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetScenario {
    scenario: EvalScenario,
    #[serde(default)]
    metric_name: Option<String>,
    #[serde(default)]
    codex_baseline: Option<f64>,
    #[serde(default)]
    threshold: Option<f64>,
    cases: Vec<DatasetCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetCase {
    case_id: String,
    spec: CaseSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatasetError {
    Parse(String),
    MissingScenario(&'static str),
    EmptyScenario(&'static str),
}

impl std::fmt::Display for DatasetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "failed to parse eval dataset: {message}"),
            Self::MissingScenario(scenario) => {
                write!(f, "eval dataset is missing scenario `{scenario}`")
            }
            Self::EmptyScenario(scenario) => {
                write!(f, "eval dataset scenario `{scenario}` has no cases")
            }
        }
    }
}

impl std::error::Error for DatasetError {}

fn default_metric_name(scenario: EvalScenario) -> &'static str {
    match scenario {
        EvalScenario::ZeroLossRecall => "recall_rate",
        EvalScenario::Chat
        | EvalScenario::WebSearch
        | EvalScenario::SqlAttribution
        | EvalScenario::DeepReport
        | EvalScenario::CodingSuccess => "success_rate",
    }
}

pub fn eval_config_from_dataset_str(input: &str) -> Result<EvalConfig, DatasetError> {
    let dataset: Dataset =
        serde_json::from_str(input).map_err(|e| DatasetError::Parse(e.to_string()))?;
    validate_dataset(&dataset)?;

    let scenarios = dataset
        .scenarios
        .into_iter()
        .map(|scenario| {
            let cases = scenario
                .cases
                .into_iter()
                .map(|case| EvalCase::new(case.case_id, case.spec))
                .collect();
            ScenarioPlan::new(
                scenario.scenario,
                scenario
                    .metric_name
                    .unwrap_or_else(|| default_metric_name(scenario.scenario).to_string()),
                scenario.codex_baseline,
                scenario.threshold,
                cases,
            )
        })
        .collect();

    Ok(EvalConfig::new(
        dataset.name,
        dataset.seed,
        dataset
            .generated_at
            .unwrap_or_else(|| "2026-07-10T00:00:00Z".to_string()),
    )
    .with_scenarios(scenarios))
}

pub fn default_eval_config() -> Result<EvalConfig, DatasetError> {
    eval_config_from_dataset_str(DEFAULT_DATASET_JSON)
}

fn validate_dataset(dataset: &Dataset) -> Result<(), DatasetError> {
    for expected in EvalScenario::all() {
        let scenario = dataset
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == *expected)
            .ok_or_else(|| DatasetError::MissingScenario(expected.as_str()))?;
        if scenario.cases.is_empty() {
            return Err(DatasetError::EmptyScenario(expected.as_str()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dataset_loads_all_registered_scenarios() {
        let config = default_eval_config().expect("default dataset should parse");

        assert_eq!(config.seed, 20260710);
        assert_eq!(config.scenarios.len(), EvalScenario::all().len());
        for expected in EvalScenario::all() {
            assert!(config
                .scenarios
                .iter()
                .any(|plan| plan.scenario == *expected));
        }
    }

    #[test]
    fn default_dataset_is_not_an_empty_green_run() {
        let config = default_eval_config().expect("default dataset should parse");

        assert!(config.scenarios.iter().all(|plan| !plan.cases.is_empty()));
        assert!(
            config.scenarios.iter().any(|plan| plan.threshold.is_some()),
            "at least one threshold must gate CI"
        );
    }

    #[test]
    fn missing_required_scenario_is_rejected() {
        let json = r#"{
            "name": "bad",
            "seed": 1,
            "scenarios": [
                {"scenario": "chat", "cases": [{"caseId": "c1", "spec": {"kind": "fixed", "value": 1.0}}]}
            ]
        }"#;

        let err = eval_config_from_dataset_str(json).expect_err("dataset must be rejected");

        assert_eq!(err, DatasetError::MissingScenario("web_search"));
    }
}
