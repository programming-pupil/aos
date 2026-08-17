//! Evaluation runner: deterministic execution + best-effort case collection +
//! CI exit-code plumbing (Requirements 2.2 / 2.6 / 2.8).
//!
//! Design reference: `.kiro/specs/codex-parity-gaps/design.md` section
//! "2. 评测 Harness", specifically the `run_eval(&EvalConfig) -> EvalReport`
//! entry point and the CI single-shot invocation that maps `overall_passed`
//! to a process exit code.
//!
//! This module is the orchestration layer over the report data models in
//! [`crate::report`]. It does not itself perform real model calls — per the
//! spec's "复用优先、不推倒重做" principle the concrete scenario execution
//! (e.g. the `ZeroLossRecall` scenario) is layered on top in later tasks and
//! plugs its per-case outcomes into the [`EvalCase`] / [`CaseSpec`] abstraction
//! consumed here. The runner's responsibilities are:
//!
//! 1. **Deterministic execution (Req 2.2).** Given a fixed [`EvalConfig::seed`]
//!    and identical scenario plans, two runs produce byte-for-byte identical
//!    scores. Randomised case outcomes are drawn from a seeded [`SplitMix64`]
//!    PRNG consumed in a fixed scenario/case order, so no external RNG or
//!    wall-clock state leaks into the result.
//! 2. **Best-effort collection (Req 2.8).** A case that fails or times out is
//!    recorded in the scenario's [`CaseFailure`] list and the run continues;
//!    no failure is silently dropped and the overall run always completes.
//! 3. **CI single-shot entry (Req 2.6).** [`run_eval_ci`] runs the evaluation
//!    and returns a process exit code — `0` when [`EvalReport::overall_passed`]
//!    is `true`, non-zero otherwise.

use serde::{Deserialize, Serialize};

use crate::report::{CaseFailure, EvalReport, ScenarioScore};
use crate::scenario::EvalScenario;

/// Default disclaimer injected when a config does not supply its own.
///
/// This is an alias of the canonical
/// [`crate::positioning::DISCLAIMER`], so [`EvalConfig::new`] injects the
/// same text and every [`EvalReport`] carries it.
pub const DEFAULT_DISCLAIMER: &str = crate::positioning::DISCLAIMER;

/// Default positioning statement injected when a config does not supply one.
///
/// This is an alias of the canonical
/// [`crate::positioning::POSITIONING`] (containing
/// `"模型无关 + 企业级 + 深度分析"`).
pub const DEFAULT_POSITIONING: &str = crate::positioning::POSITIONING;

/// Process exit code returned when an evaluation run passes (Req 2.6).
pub const EXIT_PASS: i32 = 0;

/// Process exit code returned when an evaluation run fails its thresholds
/// (Req 2.6).
pub const EXIT_FAIL: i32 = 1;

/// Deterministic outcome plan for a single evaluation case.
///
/// The runner turns each spec into a [`CaseOutcome`] using only the run seed,
/// so the mapping from a config to an [`EvalReport`] is a pure function
/// (Req 2.2). Concrete scenarios (later tasks) construct these from their real
/// per-case results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum CaseSpec {
    /// The case succeeds with a fixed per-case score contribution.
    Fixed(f64),
    /// The case succeeds with a score drawn deterministically from the seeded
    /// PRNG in `[0.0, 1.0)`. Demonstrates seed-driven reproducibility.
    Sampled,
    /// The case failed; recorded in `case_failures` with the given reason and
    /// the run continues (Req 2.8).
    Failed(String),
    /// The case timed out; recorded in `case_failures` and the run continues
    /// (Req 2.8).
    TimedOut,
}

/// A single evaluation case within a scenario plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalCase {
    /// Stable identifier of this case within its dataset.
    pub case_id: String,
    /// Deterministic outcome plan for this case.
    pub spec: CaseSpec,
}

impl EvalCase {
    /// Creates a case with the given id and outcome spec.
    #[must_use]
    pub fn new(case_id: impl Into<String>, spec: CaseSpec) -> Self {
        Self {
            case_id: case_id.into(),
            spec,
        }
    }
}

/// Plan for evaluating one [`EvalScenario`]: its metric metadata plus the cases
/// to run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioPlan {
    /// Which scenario this plan evaluates.
    pub scenario: EvalScenario,
    /// Metric name recorded on the resulting [`ScenarioScore`].
    pub metric_name: String,
    /// Codex baseline for side-by-side comparison, or `None` placeholder.
    pub codex_baseline: Option<f64>,
    /// Pass threshold for this scenario, if any (Req 2.7).
    pub threshold: Option<f64>,
    /// Cases to run for this scenario.
    pub cases: Vec<EvalCase>,
}

impl ScenarioPlan {
    /// Creates a scenario plan.
    #[must_use]
    pub fn new(
        scenario: EvalScenario,
        metric_name: impl Into<String>,
        codex_baseline: Option<f64>,
        threshold: Option<f64>,
        cases: Vec<EvalCase>,
    ) -> Self {
        Self {
            scenario,
            metric_name: metric_name.into(),
            codex_baseline,
            threshold,
            cases,
        }
    }
}

/// Configuration for a single evaluation run.
///
/// Holds the reproducibility anchor ([`EvalConfig::seed`], Req 2.2), report
/// metadata, and the per-scenario plans. Two runs sharing the same config
/// produce identical scores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalConfig {
    /// Unique identifier for the run, echoed into the report.
    pub run_id: String,
    /// Fixed random seed anchoring reproducibility (Req 2.2).
    pub seed: u64,
    /// ISO-8601 timestamp recorded on the report.
    pub generated_at: String,
    /// Disclaimer text written into the report (Req 2.9); defaults to
    /// [`DEFAULT_DISCLAIMER`] via [`EvalConfig::new`].
    pub disclaimer: String,
    /// Positioning statement written into the report (Req 2.9); defaults to
    /// [`DEFAULT_POSITIONING`] via [`EvalConfig::new`].
    pub positioning: String,
    /// Per-scenario plans to evaluate.
    pub scenarios: Vec<ScenarioPlan>,
}

impl EvalConfig {
    /// Creates a config with the default disclaimer / positioning text and no
    /// scenarios. Add scenarios via [`EvalConfig::with_scenarios`].
    #[must_use]
    pub fn new(run_id: impl Into<String>, seed: u64, generated_at: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            seed,
            generated_at: generated_at.into(),
            disclaimer: DEFAULT_DISCLAIMER.to_string(),
            positioning: DEFAULT_POSITIONING.to_string(),
            scenarios: Vec::new(),
        }
    }

    /// Sets the scenario plans, returning the updated config (builder style).
    #[must_use]
    pub fn with_scenarios(mut self, scenarios: Vec<ScenarioPlan>) -> Self {
        self.scenarios = scenarios;
        self
    }
}

/// Outcome of running a single case.
enum CaseOutcome {
    /// Case succeeded with the given score contribution.
    Scored(f64),
    /// Case failed / timed out (best-effort, recorded and skipped).
    Failed(CaseFailure),
}

/// Deterministic SplitMix64 PRNG.
///
/// A tiny, self-contained generator so reproducibility (Req 2.2) does not
/// depend on any external RNG crate or global/thread state. Seeded once per run
/// from [`EvalConfig::seed`] and consumed in a fixed scenario/case order.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Draws a value in `[0.0, 1.0)` using the top 53 bits (f64 mantissa).
    #[allow(clippy::cast_precision_loss)]
    fn next_unit_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

/// Evaluates a single case deterministically, routing failures / timeouts into
/// a [`CaseFailure`] (Req 2.8) and successes into a score contribution.
fn run_case(case: &EvalCase, rng: &mut SplitMix64) -> CaseOutcome {
    match &case.spec {
        CaseSpec::Fixed(score) => CaseOutcome::Scored(*score),
        CaseSpec::Sampled => CaseOutcome::Scored(rng.next_unit_f64()),
        CaseSpec::Failed(reason) => {
            CaseOutcome::Failed(CaseFailure::new(case.case_id.clone(), reason.clone()))
        }
        CaseSpec::TimedOut => {
            CaseOutcome::Failed(CaseFailure::new(case.case_id.clone(), "timeout"))
        }
    }
}

/// Runs every case in a scenario plan best-effort and aggregates a
/// [`ScenarioScore`] (Req 2.8).
///
/// The scenario's `aos_score` is the mean of the successful cases' score
/// contributions, or `0.0` when no case succeeds. Failed / timed-out cases are
/// collected into `case_failures` and excluded from the mean; the run never
/// aborts on a case failure.
fn run_scenario(plan: &ScenarioPlan, rng: &mut SplitMix64) -> ScenarioScore {
    let mut scored: Vec<f64> = Vec::new();
    let mut failures: Vec<CaseFailure> = Vec::new();

    for case in &plan.cases {
        match run_case(case, rng) {
            CaseOutcome::Scored(score) => scored.push(score),
            CaseOutcome::Failed(failure) => failures.push(failure),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let aos_score = if scored.is_empty() {
        0.0
    } else {
        scored.iter().sum::<f64>() / scored.len() as f64
    };

    ScenarioScore::new(
        plan.scenario,
        plan.metric_name.clone(),
        aos_score,
        plan.codex_baseline,
        plan.threshold,
        failures,
    )
}

/// Runs the full evaluation described by `config` and produces an
/// [`EvalReport`] (Req 2.2 / 2.4 / 2.8).
///
/// Deterministic: a single [`SplitMix64`] seeded from `config.seed` is consumed
/// in scenario-then-case order, so repeating the run with the same config
/// yields identical scores. Best-effort: individual case failures / timeouts
/// are collected per scenario and never abort the run. The returned report's
/// `overall_passed` (derived by [`EvalReport::new`]) is the source of truth for
/// the CI exit code (Req 2.6).
#[must_use]
pub fn run_eval(config: &EvalConfig) -> EvalReport {
    let mut rng = SplitMix64::new(config.seed);
    let scenarios: Vec<ScenarioScore> = config
        .scenarios
        .iter()
        .map(|plan| run_scenario(plan, &mut rng))
        .collect();

    EvalReport::new(
        config.run_id.clone(),
        config.seed,
        scenarios,
        config.disclaimer.clone(),
        config.positioning.clone(),
        config.generated_at.clone(),
    )
}

/// Maps an [`EvalReport`] to a process exit code (Req 2.6).
///
/// Returns [`EXIT_PASS`] (`0`) when `report.overall_passed` is `true`, and
/// [`EXIT_FAIL`] (non-zero) otherwise.
#[must_use]
pub fn exit_code_for(report: &EvalReport) -> i32 {
    if report.overall_passed {
        EXIT_PASS
    } else {
        EXIT_FAIL
    }
}

/// CI single-shot entry (Req 2.6): runs the evaluation and returns the process
/// exit code without terminating the process.
///
/// Callers (e.g. a thin `main`) can pass the result to [`std::process::exit`].
/// Returning the code rather than exiting keeps the function testable.
#[must_use]
pub fn run_eval_ci(config: &EvalConfig) -> i32 {
    exit_code_for(&run_eval(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored_case(id: &str, score: f64) -> EvalCase {
        EvalCase::new(id, CaseSpec::Fixed(score))
    }

    fn config_with(scenarios: Vec<ScenarioPlan>) -> EvalConfig {
        EvalConfig::new("run-test", 1234, "2024-01-01T00:00:00Z").with_scenarios(scenarios)
    }

    #[test]
    fn run_eval_is_reproducible_for_fixed_seed() {
        // Req 2.2: same config (incl. seed) -> identical scores across runs.
        // Use Sampled cases so the seeded PRNG actually drives the outcome.
        let plan = ScenarioPlan::new(
            EvalScenario::Chat,
            "success_rate",
            None,
            None,
            (0..25)
                .map(|i| EvalCase::new(format!("c{i}"), CaseSpec::Sampled))
                .collect(),
        );
        let config = config_with(vec![plan]);

        let first = run_eval(&config);
        let second = run_eval(&config);

        assert_eq!(
            first.scenarios, second.scenarios,
            "scores must be identical"
        );
        assert_eq!(first.seed, config.seed);
    }

    #[test]
    fn different_seeds_change_sampled_scores() {
        // With many sampled cases, changing the seed changes the aggregate.
        let plan_for = |seed: u64| {
            let cases = (0..50)
                .map(|i| EvalCase::new(format!("c{i}"), CaseSpec::Sampled))
                .collect();
            EvalConfig::new("run", seed, "2024-01-01T00:00:00Z").with_scenarios(vec![
                ScenarioPlan::new(EvalScenario::Chat, "success_rate", None, None, cases),
            ])
        };

        let a = run_eval(&plan_for(1));
        let b = run_eval(&plan_for(2));
        assert_ne!(
            a.scenarios[0].aos_score, b.scenarios[0].aos_score,
            "different seeds should yield different aggregate scores"
        );
    }

    #[test]
    fn best_effort_records_every_failure_with_no_silent_loss() {
        // Req 2.8: failures/timeouts are recorded and the run continues; the
        // count of successes + failures equals the total number of cases.
        let cases = vec![
            scored_case("ok-1", 1.0),
            EvalCase::new("fail-1", CaseSpec::Failed("boom".into())),
            scored_case("ok-2", 1.0),
            EvalCase::new("timeout-1", CaseSpec::TimedOut),
            EvalCase::new("fail-2", CaseSpec::Failed("nope".into())),
        ];
        let total = cases.len();
        let plan = ScenarioPlan::new(
            EvalScenario::CodingSuccess,
            "success_rate",
            None,
            None,
            cases,
        );
        let report = run_eval(&config_with(vec![plan]));

        let score = &report.scenarios[0];
        let failed = score.case_failures.len();
        assert_eq!(failed, 3, "three cases failed/timed out");

        // No silent loss: succeeded + failed == total.
        let succeeded = total - failed;
        assert_eq!(succeeded + failed, total);

        // Every failing/timed-out case id is present.
        let failed_ids: Vec<&str> = score
            .case_failures
            .iter()
            .map(|f| f.case_id.as_str())
            .collect();
        assert!(failed_ids.contains(&"fail-1"));
        assert!(failed_ids.contains(&"fail-2"));
        assert!(failed_ids.contains(&"timeout-1"));

        // Timeout is recorded with a timeout reason.
        let timeout = score
            .case_failures
            .iter()
            .find(|f| f.case_id == "timeout-1")
            .unwrap();
        assert_eq!(timeout.reason, "timeout");
    }

    #[test]
    fn case_failure_does_not_abort_other_scenarios() {
        // A failing case in one scenario must not prevent later scenarios from
        // being scored (Req 2.8).
        let failing = ScenarioPlan::new(
            EvalScenario::Chat,
            "success_rate",
            None,
            None,
            vec![EvalCase::new("f", CaseSpec::Failed("x".into()))],
        );
        let healthy = ScenarioPlan::new(
            EvalScenario::WebSearch,
            "success_rate",
            None,
            None,
            vec![scored_case("ok", 0.75)],
        );
        let report = run_eval(&config_with(vec![failing, healthy]));

        assert_eq!(report.scenarios.len(), 2);
        assert!((report.scenarios[1].aos_score - 0.75).abs() < 1e-9);
    }

    #[test]
    fn aos_score_is_mean_of_successful_cases() {
        let plan = ScenarioPlan::new(
            EvalScenario::CodingSuccess,
            "success_rate",
            None,
            None,
            vec![
                scored_case("a", 1.0),
                scored_case("b", 0.0),
                EvalCase::new("c", CaseSpec::Failed("skip".into())),
            ],
        );
        let report = run_eval(&config_with(vec![plan]));
        // Mean over the two successful cases (1.0, 0.0) == 0.5; the failed case
        // is excluded from the mean.
        assert!((report.scenarios[0].aos_score - 0.5).abs() < 1e-9);
    }

    #[test]
    fn empty_scenario_scores_zero() {
        let plan = ScenarioPlan::new(EvalScenario::DeepReport, "success_rate", None, None, vec![]);
        let report = run_eval(&config_with(vec![plan]));
        assert!((report.scenarios[0].aos_score - 0.0).abs() < 1e-9);
    }

    #[test]
    fn exit_code_is_zero_when_overall_passed() {
        // No thresholds -> overall_passed == true -> exit 0.
        let plan = ScenarioPlan::new(
            EvalScenario::Chat,
            "success_rate",
            None,
            None,
            vec![scored_case("ok", 1.0)],
        );
        let report = run_eval(&config_with(vec![plan.clone()]));
        assert!(report.overall_passed);
        assert_eq!(exit_code_for(&report), EXIT_PASS);
        assert_eq!(run_eval_ci(&config_with(vec![plan])), EXIT_PASS);
    }

    #[test]
    fn exit_code_is_nonzero_when_below_threshold() {
        // A scenario scoring below its threshold fails the run (Req 2.6/2.7).
        let plan = ScenarioPlan::new(
            EvalScenario::ZeroLossRecall,
            "recall_rate",
            None,
            Some(0.99),
            vec![scored_case("low", 0.5)],
        );
        let config = config_with(vec![plan]);
        let report = run_eval(&config);
        assert!(!report.overall_passed);
        assert_eq!(exit_code_for(&report), EXIT_FAIL);
        assert_ne!(run_eval_ci(&config), EXIT_PASS);
    }

    #[test]
    fn report_carries_seed_disclaimer_and_positioning() {
        let report = run_eval(&config_with(vec![]));
        assert_eq!(report.seed, 1234);
        assert_eq!(report.disclaimer, DEFAULT_DISCLAIMER);
        assert_eq!(report.positioning, DEFAULT_POSITIONING);
        assert!(report.overall_passed, "no scenarios -> passes");
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    fn sampled_config(seed: u64, case_count: usize) -> EvalConfig {
        let cases = (0..case_count)
            .map(|index| EvalCase::new(format!("case-{index}"), CaseSpec::Sampled))
            .collect();
        EvalConfig::new("run-property-9", seed, "2026-07-10T00:00:00Z").with_scenarios(vec![
            ScenarioPlan::new(EvalScenario::Chat, "success_rate", None, None, cases),
        ])
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn property_9_fixed_seed_is_reproducible(seed in any::<u64>(), case_count in 1usize..80) {
            // Feature: codex-parity-gaps, Property 9: 评测可复现（固定种子确定性）
            let config = sampled_config(seed, case_count);
            let first = run_eval(&config);
            let second = run_eval(&config);
            prop_assert_eq!(first.scenarios, second.scenarios);
            prop_assert_eq!(first.seed, second.seed);
        }

        #[test]
        fn property_11_best_effort_records_every_failed_case(
            failures in proptest::collection::vec(any::<bool>(), 1..100),
        ) {
            // Feature: codex-parity-gaps, Property 11: 评测 best-effort——整体完成且失败用例无静默丢失
            let total = failures.len();
            let expected_failed_ids: Vec<String> = failures
                .iter()
                .enumerate()
                .filter_map(|(index, failed)| failed.then(|| format!("case-{index}")))
                .collect();
            let cases: Vec<EvalCase> = failures
                .into_iter()
                .enumerate()
                .map(|(index, failed)| {
                    let id = format!("case-{index}");
                    if failed {
                        EvalCase::new(id, CaseSpec::Failed("generated failure".to_string()))
                    } else {
                        EvalCase::new(id, CaseSpec::Fixed(1.0))
                    }
                })
                .collect();
            let config = EvalConfig::new("run-property-11", 20260710, "2026-07-10T00:00:00Z")
                .with_scenarios(vec![ScenarioPlan::new(
                    EvalScenario::CodingSuccess,
                    "success_rate",
                    None,
                    None,
                    cases,
                )]);
            let report = run_eval(&config);
            prop_assert_eq!(report.scenarios.len(), 1);

            let score = &report.scenarios[0];
            let actual_failed_ids: std::collections::HashSet<String> = score
                .case_failures
                .iter()
                .map(|failure| failure.case_id.clone())
                .collect();
            let expected_failed: std::collections::HashSet<String> =
                expected_failed_ids.into_iter().collect();
            prop_assert_eq!(actual_failed_ids, expected_failed);

            let failed = score.case_failures.len();
            let succeeded = total - failed;
            prop_assert_eq!(succeeded + failed, total);
        }
    }
}
