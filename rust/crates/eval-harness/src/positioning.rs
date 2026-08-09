//! Canonical disclaimer / positioning statements injected into every
//! [`EvalReport`](crate::report::EvalReport) (Requirement 2.5 / 2.9).
//!
//! Design reference: `.kiro/specs/codex-parity-gaps/design.md` section
//! "2. 评测 Harness"（免责与定位）与 Data Models 的 `EvalReport` 字段
//! `disclaimer` / `positioning`。
//!
//! ## 为什么固化在此
//!
//! 需求 2.9 要求评测报表**显式记录**两件事:
//!
//! 1. **免责([`DISCLAIMER`])** —— 裸编码准确度受「模型 × harness 协同训练」
//!    影响,**非本 spec 承诺范围**。本 harness 只用于**客观量化**该差距,
//!    而不承诺在该维度上追平 / 超越 Codex。
//! 2. **定位([`POSITIONING`])** —— AOS 的差异化赢点是
//!    **模型无关 + 企业级 + 深度分析**,使定位可被评测数据支撑。
//!
//! 把这两段文本固化为编译期常量(而非散落在各处的字符串字面量),保证:
//! - 报表、文档([`docs/AOS_VS_CODEX_EVAL.md`])、CI 断言引用同一份权威文本;
//! - [`crate::runner::EvalConfig::new`] 默认注入的即是此处的权威文本
//!   (`runner` 的 `DEFAULT_DISCLAIMER` / `DEFAULT_POSITIONING` 是本模块的别名)。

/// 免责声明:裸编码准确度受模型 × harness 协同影响、**非本 spec 承诺范围**
/// (Requirement 2.9)。
///
/// 该文本被注入到每一份 [`EvalReport`](crate::report::EvalReport) 的
/// `disclaimer` 字段,并被 `docs/AOS_VS_CODEX_EVAL.md` 引用。
///
/// 不变量(CI 示例测试断言,见任务 3.10):本串**必须**包含子串
/// `"非本 spec 承诺"`。
pub const DISCLAIMER: &str = "裸编码准确度受模型 × harness 协同影响、非本 spec 承诺范围;\
本 harness 仅用于客观量化 AOS 与 Codex 的差距,不承诺在裸编码准确度维度上追平或超越 Codex。";

/// 定位声明:AOS 的差异化赢点为 **模型无关 + 企业级 + 深度分析**
/// (Requirement 2.9)。
///
/// 该文本被注入到每一份 [`EvalReport`](crate::report::EvalReport) 的
/// `positioning` 字段,并被 `docs/AOS_VS_CODEX_EVAL.md` 引用。
///
/// 不变量(CI 示例测试断言,见任务 3.10):本串**必须**包含子串
/// `"模型无关 + 企业级 + 深度分析"`。
pub const POSITIONING: &str =
    "AOS 的差异化定位为模型无关 + 企业级 + 深度分析:可插拔运行时(模型无关)、\
多租户与审计(企业级)、以及证据可追溯的深度分析能力。";

/// 必须出现在 [`DISCLAIMER`] 中的核心子串,供报表 / 文档 / CI 断言复用。
pub const DISCLAIMER_MARKER: &str = "非本 spec 承诺";

/// 必须出现在 [`POSITIONING`] 中的核心子串,供报表 / 文档 / CI 断言复用。
pub const POSITIONING_MARKER: &str = "模型无关 + 企业级 + 深度分析";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::EvalConfig;

    #[test]
    fn disclaimer_contains_required_marker() {
        // Req 2.9: 报表须显式记录「裸编码准确度非本 spec 承诺」。
        assert!(
            DISCLAIMER.contains(DISCLAIMER_MARKER),
            "disclaimer must contain {DISCLAIMER_MARKER:?}: {DISCLAIMER}"
        );
        assert!(DISCLAIMER.contains("非本 spec 承诺"));
    }

    #[test]
    fn positioning_contains_required_marker() {
        // Req 2.9: 报表须将 AOS 定位标注为模型无关 + 企业级 + 深度分析。
        assert!(
            POSITIONING.contains(POSITIONING_MARKER),
            "positioning must contain {POSITIONING_MARKER:?}: {POSITIONING}"
        );
        assert!(POSITIONING.contains("模型无关 + 企业级 + 深度分析"));
    }

    #[test]
    fn eval_config_injects_canonical_disclaimer_and_positioning() {
        // EvalConfig::new must default-inject the canonical text, so every
        // report carries the Req 2.9 disclaimer / positioning.
        let config = EvalConfig::new("run-x", 7, "2024-01-01T00:00:00Z");
        assert_eq!(config.disclaimer, DISCLAIMER);
        assert_eq!(config.positioning, POSITIONING);
    }

    #[test]
    fn report_produced_by_runner_carries_canonical_text() {
        // End-to-end: text flows config -> report (Req 2.9).
        let config = EvalConfig::new("run-y", 1, "2024-01-01T00:00:00Z");
        let report = crate::runner::run_eval(&config);
        assert!(report.disclaimer.contains(DISCLAIMER_MARKER));
        assert!(report.positioning.contains(POSITIONING_MARKER));
    }

    #[test]
    fn ci_exit_code_and_report_positioning_contract() {
        // Task 3.10 example: overallPassed=false maps to a non-zero CI exit
        // code; every report keeps the canonical disclaimer / positioning text.
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
        assert!(report.disclaimer.contains("非本 spec 承诺"));
        assert!(report.positioning.contains("模型无关 + 企业级 + 深度分析"));

        let passing = EvalConfig::new("run-ci-pass", 7, "2024-01-01T00:00:00Z");
        assert_eq!(
            crate::runner::run_eval_ci(&passing),
            crate::runner::EXIT_PASS
        );
    }
}
