//! Canonical disclaimer and positioning statements injected into every
//! [`EvalReport`](crate::report::EvalReport).
//!
//! Deterministic regression measurements and broader product-effect claims are
//! intentionally separated. Keeping these statements as compile-time constants
//! ensures reports and CI assertions use the same wording.

/// Disclaimer injected into every report. Deterministic fixtures verify the
/// evaluation wiring and regression contracts, not overall product quality.
pub const DISCLAIMER: &str = "确定性 fixture 仅验证评测接线、回归契约与结果可复现性;\
单次或合成评测结果不代表全面效果领先，发布结论仍需固定环境实测与人工盲评。";

/// Product positioning injected into every report.
pub const POSITIONING: &str =
    "AOS 的差异化定位为模型无关 + 企业级 + 深度分析:可插拔运行时(模型无关)、\
多租户与审计(企业级)、以及证据可追溯的深度分析能力。";

/// Required [`DISCLAIMER`] marker shared by report and CI assertions.
pub const DISCLAIMER_MARKER: &str = "不代表全面效果领先";

/// Required [`POSITIONING`] marker shared by report and CI assertions.
pub const POSITIONING_MARKER: &str = "模型无关 + 企业级 + 深度分析";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::EvalConfig;

    #[test]
    fn disclaimer_contains_required_marker() {
        assert!(
            DISCLAIMER.contains(DISCLAIMER_MARKER),
            "disclaimer must contain {DISCLAIMER_MARKER:?}: {DISCLAIMER}"
        );
        assert!(DISCLAIMER.contains("固定环境实测与人工盲评"));
    }

    #[test]
    fn positioning_contains_required_marker() {
        assert!(
            POSITIONING.contains(POSITIONING_MARKER),
            "positioning must contain {POSITIONING_MARKER:?}: {POSITIONING}"
        );
    }

    #[test]
    fn eval_config_injects_canonical_disclaimer_and_positioning() {
        let config = EvalConfig::new("run-x", 7, "2024-01-01T00:00:00Z");
        assert_eq!(config.disclaimer, DISCLAIMER);
        assert_eq!(config.positioning, POSITIONING);
    }

    #[test]
    fn report_produced_by_runner_carries_canonical_text() {
        let config = EvalConfig::new("run-y", 1, "2024-01-01T00:00:00Z");
        let report = crate::runner::run_eval(&config);
        assert!(report.disclaimer.contains(DISCLAIMER_MARKER));
        assert!(report.positioning.contains(POSITIONING_MARKER));
    }

    #[test]
    fn ci_exit_code_and_report_positioning_contract() {
        let failing = crate::runner::ScenarioPlan::new(
            crate::scenario::EvalScenario::ZeroLossRecall,
            "recall_rate",
            None,
            Some(0.99),
            vec![crate::runner::EvalCase::new(
                "low-recall",
                crate::runner::CaseSpec::Fixed(0.5),
            )],
        );
        let config =
            EvalConfig::new("run-ci", 7, "2024-01-01T00:00:00Z").with_scenarios(vec![failing]);
        let report = crate::runner::run_eval(&config);

        assert!(!report.overall_passed);
        assert_ne!(
            crate::runner::exit_code_for(&report),
            crate::runner::EXIT_PASS
        );
        assert!(report.disclaimer.contains(DISCLAIMER_MARKER));
        assert!(report.positioning.contains(POSITIONING_MARKER));

        let passing = EvalConfig::new("run-ci-pass", 7, "2024-01-01T00:00:00Z");
        assert_eq!(
            crate::runner::run_eval_ci(&passing),
            crate::runner::EXIT_PASS
        );
    }
}
