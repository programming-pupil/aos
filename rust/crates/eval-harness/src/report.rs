//! Structured evaluation report data models (需求 2.4 / 2.5 / 2.7).
//!
//! Design reference: `.kiro/specs/codex-parity-gaps/design.md` section
//! "2. 评测 Harness" and the `EvalReport` / `ScenarioScore` data model.
//!
//! This module defines the serializable report shapes produced by an
//! evaluation run and the two pure derivations they carry:
//! - `delta = aos_score - codex_baseline`, computed only when a Codex baseline
//!   is present (Requirement 2.5).
//! - `passed = Some(aos_score >= threshold)`, computed only when a per-scenario
//!   pass threshold is configured (Requirement 2.7).
//!
//! Serialization uses camelCase field names to match the design TS interfaces
//! (`runId` / `aosScore` / `codexBaseline` / `caseFailures` / ...). The runner
//! (`run_eval`) and CI exit-code plumbing are implemented separately in task
//! 3.3; this module contains only the data models and their pure judgments.

use serde::{Deserialize, Serialize};

use crate::scenario::EvalScenario;

/// A single evaluation case that failed or timed out during a run.
///
/// Failed / timed-out cases are recorded here (rather than aborting the run)
/// so the overall evaluation is best-effort with no silent loss
/// (Requirement 2.8). Task 3.2 defines the shape; the runner populates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseFailure {
    /// Identifier of the failing case within its dataset.
    pub case_id: String,
    /// Human-readable reason the case failed or timed out.
    pub reason: String,
}

impl CaseFailure {
    /// Creates a new case-failure record.
    #[must_use]
    pub fn new(case_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            case_id: case_id.into(),
            reason: reason.into(),
        }
    }
}

/// Quantified result for one [`EvalScenario`] (Requirement 2.4 / 2.5 / 2.7).
///
/// Carries the AOS measured score, the optional Codex baseline (or `None`
/// placeholder), and the two pure derivations:
/// - `delta` — `Some(aos_score - codex_baseline)` when a baseline exists,
///   otherwise `None` (Requirement 2.5).
/// - `passed` — `Some(aos_score >= threshold)` when a threshold is configured,
///   otherwise `None` (Requirement 2.7).
///
/// Prefer [`ScenarioScore::new`] so `delta` and `passed` stay consistent with
/// the inputs; the fields are public to support deserialization and testing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioScore {
    /// Which scenario this score belongs to.
    pub scenario: EvalScenario,
    /// Metric name, e.g. `"success_rate"` or `"recall_rate"`.
    pub metric_name: String,
    /// AOS measured score for this scenario.
    pub aos_score: f64,
    /// Codex baseline for side-by-side comparison, or `None` as a placeholder.
    pub codex_baseline: Option<f64>,
    /// `aos_score - codex_baseline` when the baseline is present, else `None`.
    pub delta: Option<f64>,
    /// Configured pass threshold for this scenario, if any.
    pub threshold: Option<f64>,
    /// `aos_score >= threshold` when a threshold is configured, else `None`.
    pub passed: Option<bool>,
    /// Cases that failed or timed out (best-effort, Requirement 2.8).
    pub case_failures: Vec<CaseFailure>,
}

impl ScenarioScore {
    /// Builds a scenario score, deriving `delta` and `passed` from the inputs.
    ///
    /// - `delta` is `Some(aos_score - codex_baseline)` only when `codex_baseline`
    ///   is `Some` (Requirement 2.5); otherwise `None`.
    /// - `passed` is `Some(aos_score >= threshold)` only when `threshold` is
    ///   `Some` (Requirement 2.7); otherwise `None`.
    #[must_use]
    pub fn new(
        scenario: EvalScenario,
        metric_name: impl Into<String>,
        aos_score: f64,
        codex_baseline: Option<f64>,
        threshold: Option<f64>,
        case_failures: Vec<CaseFailure>,
    ) -> Self {
        let delta = codex_baseline.map(|baseline| aos_score - baseline);
        let passed = threshold.map(|t| aos_score >= t);
        Self {
            scenario,
            metric_name: metric_name.into(),
            aos_score,
            codex_baseline,
            delta,
            threshold,
            passed,
            case_failures,
        }
    }

    /// Whether this scenario is a threshold-bearing scenario that failed to meet
    /// its bar (`passed == Some(false)`). Scenarios without a threshold never
    /// count as failing (Requirement 2.7).
    #[must_use]
    pub fn is_below_threshold(&self) -> bool {
        self.passed == Some(false)
    }
}

/// Structured evaluation report produced by a run (Requirement 2.4 / 2.5 / 2.9).
///
/// Fields mirror the design TS `EvalReport` interface (camelCase on the wire).
/// `overall_passed` is the CI exit-code source: it is `false` when any
/// threshold-bearing scenario is below its bar. The runner (task 3.3) fills the
/// `seed` / `run_id` / timing plumbing; task 3.2 provides the model plus the
/// pure `compute_overall_passed` derivation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalReport {
    /// Unique identifier for this evaluation run.
    pub run_id: String,
    /// Fixed random seed anchoring reproducibility (Requirement 2.2).
    pub seed: u64,
    /// Per-scenario scores.
    pub scenarios: Vec<ScenarioScore>,
    /// `false` when any threshold-bearing scenario is below its bar.
    pub overall_passed: bool,
    /// Disclaimer that bare coding accuracy is out of this spec's scope
    /// (Requirement 2.9).
    pub disclaimer: String,
    /// AOS positioning statement: model-agnostic + enterprise + deep analysis.
    pub positioning: String,
    /// ISO-8601 timestamp of when the report was generated.
    pub generated_at: String,
}

impl EvalReport {
    /// Assembles a report, deriving `overall_passed` from the scenarios.
    ///
    /// `overall_passed` is `false` if any scenario is below its configured
    /// threshold (`passed == Some(false)`), and `true` otherwise
    /// (Requirement 2.7). Scenarios without a threshold do not affect the
    /// outcome.
    #[must_use]
    pub fn new(
        run_id: impl Into<String>,
        seed: u64,
        scenarios: Vec<ScenarioScore>,
        disclaimer: impl Into<String>,
        positioning: impl Into<String>,
        generated_at: impl Into<String>,
    ) -> Self {
        let overall_passed = Self::compute_overall_passed(&scenarios);
        Self {
            run_id: run_id.into(),
            seed,
            scenarios,
            overall_passed,
            disclaimer: disclaimer.into(),
            positioning: positioning.into(),
            generated_at: generated_at.into(),
        }
    }

    /// Returns `false` when any threshold-bearing scenario failed to meet its
    /// bar; `true` otherwise (Requirement 2.7).
    #[must_use]
    pub fn compute_overall_passed(scenarios: &[ScenarioScore]) -> bool {
        !scenarios.iter().any(ScenarioScore::is_below_threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_is_computed_when_baseline_present() {
        let score = ScenarioScore::new(
            EvalScenario::CodingSuccess,
            "success_rate",
            0.82,
            Some(0.75),
            None,
            Vec::new(),
        );
        assert_eq!(score.codex_baseline, Some(0.75));
        let delta = score.delta.expect("delta should be present");
        assert!((delta - 0.07).abs() < 1e-9, "delta = {delta}");
    }

    #[test]
    fn delta_is_none_when_baseline_absent() {
        let score = ScenarioScore::new(
            EvalScenario::Chat,
            "success_rate",
            0.9,
            None,
            None,
            Vec::new(),
        );
        assert_eq!(score.delta, None);
    }

    #[test]
    fn delta_can_be_negative() {
        let score = ScenarioScore::new(
            EvalScenario::CodingSuccess,
            "success_rate",
            0.60,
            Some(0.75),
            None,
            Vec::new(),
        );
        let delta = score.delta.expect("delta should be present");
        assert!((delta - (-0.15)).abs() < 1e-9, "delta = {delta}");
    }

    #[test]
    fn passed_is_true_when_score_meets_threshold() {
        let at = ScenarioScore::new(
            EvalScenario::ZeroLossRecall,
            "recall_rate",
            0.99,
            None,
            Some(0.99),
            Vec::new(),
        );
        assert_eq!(at.passed, Some(true), "score equal to threshold passes");

        let above = ScenarioScore::new(
            EvalScenario::ZeroLossRecall,
            "recall_rate",
            0.995,
            None,
            Some(0.99),
            Vec::new(),
        );
        assert_eq!(above.passed, Some(true));
    }

    #[test]
    fn passed_is_false_when_score_below_threshold() {
        let score = ScenarioScore::new(
            EvalScenario::ZeroLossRecall,
            "recall_rate",
            0.80,
            None,
            Some(0.99),
            Vec::new(),
        );
        assert_eq!(score.passed, Some(false));
        assert!(score.is_below_threshold());
    }

    #[test]
    fn passed_is_none_when_threshold_absent() {
        let score = ScenarioScore::new(
            EvalScenario::Chat,
            "success_rate",
            0.5,
            None,
            None,
            Vec::new(),
        );
        assert_eq!(score.passed, None);
        assert!(!score.is_below_threshold());
    }

    #[test]
    fn overall_passed_true_when_no_scenario_below_threshold() {
        let scenarios = vec![
            ScenarioScore::new(
                EvalScenario::Chat,
                "success_rate",
                0.9,
                None,
                None,
                Vec::new(),
            ),
            ScenarioScore::new(
                EvalScenario::ZeroLossRecall,
                "recall_rate",
                0.99,
                None,
                Some(0.99),
                Vec::new(),
            ),
        ];
        assert!(EvalReport::compute_overall_passed(&scenarios));
    }

    #[test]
    fn overall_passed_false_when_any_scenario_below_threshold() {
        let scenarios = vec![
            ScenarioScore::new(
                EvalScenario::Chat,
                "success_rate",
                0.9,
                None,
                None,
                Vec::new(),
            ),
            ScenarioScore::new(
                EvalScenario::ZeroLossRecall,
                "recall_rate",
                0.5,
                None,
                Some(0.99),
                Vec::new(),
            ),
        ];
        assert!(!EvalReport::compute_overall_passed(&scenarios));
    }

    #[test]
    fn report_new_derives_overall_passed() {
        let report = EvalReport::new(
            "run-123",
            42,
            vec![ScenarioScore::new(
                EvalScenario::CodingSuccess,
                "success_rate",
                0.60,
                Some(0.75),
                Some(0.70),
                Vec::new(),
            )],
            "disclaimer text",
            "positioning text",
            "2024-01-01T00:00:00Z",
        );
        assert!(!report.overall_passed);
        assert_eq!(report.seed, 42);
        assert_eq!(report.run_id, "run-123");
    }

    #[test]
    fn scenario_score_serializes_to_camel_case() {
        let score = ScenarioScore::new(
            EvalScenario::CodingSuccess,
            "success_rate",
            0.82,
            Some(0.75),
            Some(0.70),
            vec![CaseFailure::new("case-1", "timeout")],
        );
        let json = serde_json::to_string(&score).unwrap();
        assert!(json.contains("\"aosScore\":0.82"), "json = {json}");
        assert!(json.contains("\"codexBaseline\":0.75"), "json = {json}");
        assert!(
            json.contains("\"metricName\":\"success_rate\""),
            "json = {json}"
        );
        assert!(json.contains("\"caseFailures\""), "json = {json}");
        assert!(json.contains("\"caseId\":\"case-1\""), "json = {json}");
        assert!(
            json.contains("\"scenario\":\"coding_success\""),
            "json = {json}"
        );
    }

    #[test]
    fn report_serializes_to_camel_case() {
        let report = EvalReport::new("run-1", 7, Vec::new(), "d", "p", "2024-01-01T00:00:00Z");
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"runId\":\"run-1\""), "json = {json}");
        assert!(json.contains("\"overallPassed\":true"), "json = {json}");
        assert!(json.contains("\"generatedAt\""), "json = {json}");
    }

    #[test]
    fn scenario_score_round_trips_through_json() {
        let score = ScenarioScore::new(
            EvalScenario::WebSearch,
            "success_rate",
            0.88,
            Some(0.90),
            Some(0.85),
            vec![CaseFailure::new("c1", "boom")],
        );
        let json = serde_json::to_string(&score).unwrap();
        let decoded: ScenarioScore = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.scenario, score.scenario);
        assert_eq!(decoded.metric_name, score.metric_name);
        assert!((decoded.aos_score - score.aos_score).abs() < 1e-12);
        assert_eq!(decoded.codex_baseline, score.codex_baseline);
        assert!(
            (decoded.delta.expect("decoded delta") - score.delta.expect("score delta")).abs()
                < 1e-12
        );
        assert_eq!(decoded.threshold, score.threshold);
        assert_eq!(decoded.passed, score.passed);
        assert_eq!(decoded.case_failures, score.case_failures);
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    fn scenario_strategy() -> impl Strategy<Value = EvalScenario> {
        prop_oneof![
            Just(EvalScenario::Chat),
            Just(EvalScenario::WebSearch),
            Just(EvalScenario::SqlAttribution),
            Just(EvalScenario::DeepReport),
            Just(EvalScenario::CodingSuccess),
            Just(EvalScenario::ZeroLossRecall),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn property_8_report_has_complete_fields_and_correct_delta(
            inputs in proptest::collection::vec(
                (
                    scenario_strategy(),
                    "[a-z_]{1,24}",
                    0.0f64..1.0,
                    prop::option::of(0.0f64..1.0),
                ),
                1..24,
            ),
        ) {
            // Feature: codex-parity-gaps, Property 8: 评测报表结构完整且差值计算正确
            let scores: Vec<ScenarioScore> = inputs
                .iter()
                .map(|(scenario, metric_name, aos_score, codex_baseline)| {
                    ScenarioScore::new(
                        *scenario,
                        metric_name.clone(),
                        *aos_score,
                        *codex_baseline,
                        None,
                        Vec::new(),
                    )
                })
                .collect();
            let report = EvalReport::new(
                "run-property-8",
                20260710,
                scores,
                "disclaimer",
                "positioning",
                "2026-07-10T00:00:00Z",
            );

            prop_assert_eq!(report.scenarios.len(), inputs.len());
            for (score, (_scenario, metric_name, aos_score, codex_baseline)) in
                report.scenarios.iter().zip(inputs.iter())
            {
                prop_assert_eq!(&score.metric_name, metric_name);
                prop_assert!((score.aos_score - *aos_score).abs() < f64::EPSILON);
                prop_assert_eq!(score.codex_baseline, *codex_baseline);
                match codex_baseline {
                    Some(baseline) => {
                        let delta = score.delta.expect("baseline present -> delta present");
                        prop_assert!((delta - (*aos_score - *baseline)).abs() < 1e-12);
                    }
                    None => prop_assert_eq!(score.delta, None),
                }
            }
        }

        #[test]
        fn property_10_passed_iff_score_meets_threshold(
            scenario in scenario_strategy(),
            aos_score in 0.0f64..1.0,
            threshold in 0.0f64..1.0,
        ) {
            // Feature: codex-parity-gaps, Property 10: 达标判定当且仅当分数达阈
            let score = ScenarioScore::new(
                scenario,
                "success_rate",
                aos_score,
                None,
                Some(threshold),
                Vec::new(),
            );
            prop_assert_eq!(score.passed, Some(aos_score >= threshold));
            prop_assert_eq!(score.is_below_threshold(), aos_score < threshold);
        }
    }
}
