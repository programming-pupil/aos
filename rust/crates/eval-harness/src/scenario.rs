//! Eval scenarios covered by the automated accuracy / recall harness.
//!
//! Design reference: `.kiro/specs/codex-parity-gaps/design.md` section
//! "2. 评测 Harness". Requirement 2.1 mandates the harness cover the six
//! scenarios enumerated below, and that all six be registered.

use serde::{Deserialize, Serialize};

/// A single class of task the [`Eval_Harness`](crate) evaluates.
///
/// Covers the six scenarios required by Requirement 2.1:
/// - [`EvalScenario::Chat`] — 普通聊天
/// - [`EvalScenario::WebSearch`] — 联网检索
/// - [`EvalScenario::SqlAttribution`] — SQL 归因
/// - [`EvalScenario::DeepReport`] — 深度报告
/// - [`EvalScenario::CodingSuccess`] — 编码任务成功率
/// - [`EvalScenario::ZeroLossRecall`] — 上下文 0 丢失召回率
///
/// Serialization uses the stable snake_case identifiers defined in the design
/// data model (`chat` / `web_search` / `sql_attribution` / `deep_report` /
/// `coding_success` / `zero_loss_recall`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalScenario {
    /// 普通聊天。
    Chat,
    /// 联网检索。
    WebSearch,
    /// SQL 归因。
    SqlAttribution,
    /// 深度报告。
    DeepReport,
    /// 编码任务成功率。
    CodingSuccess,
    /// 上下文 0 丢失召回率(复用现有 `ZeroLossMeasurement`)。
    ZeroLossRecall,
}

impl EvalScenario {
    /// All registered scenarios, in canonical order (Requirement 2.1).
    ///
    /// This is the single registration point the harness runner iterates over;
    /// keeping it exhaustive guarantees every scenario is covered by a run.
    pub const ALL: [EvalScenario; 6] = [
        EvalScenario::Chat,
        EvalScenario::WebSearch,
        EvalScenario::SqlAttribution,
        EvalScenario::DeepReport,
        EvalScenario::CodingSuccess,
        EvalScenario::ZeroLossRecall,
    ];

    /// Returns every registered scenario.
    #[must_use]
    pub fn all() -> &'static [EvalScenario] {
        &Self::ALL
    }

    /// Stable, serialization-aligned identifier for this scenario.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EvalScenario::Chat => "chat",
            EvalScenario::WebSearch => "web_search",
            EvalScenario::SqlAttribution => "sql_attribution",
            EvalScenario::DeepReport => "deep_report",
            EvalScenario::CodingSuccess => "coding_success",
            EvalScenario::ZeroLossRecall => "zero_loss_recall",
        }
    }
}

impl std::fmt::Display for EvalScenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_all_six_scenarios() {
        assert_eq!(EvalScenario::all().len(), 6);
    }

    #[test]
    fn all_scenarios_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for scenario in EvalScenario::all() {
            assert!(seen.insert(*scenario), "duplicate scenario: {scenario}");
        }
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn covers_each_required_scenario() {
        let all = EvalScenario::all();
        for expected in [
            EvalScenario::Chat,
            EvalScenario::WebSearch,
            EvalScenario::SqlAttribution,
            EvalScenario::DeepReport,
            EvalScenario::CodingSuccess,
            EvalScenario::ZeroLossRecall,
        ] {
            assert!(all.contains(&expected), "missing scenario: {expected}");
        }
    }

    #[test]
    fn serializes_to_snake_case_identifier() {
        assert_eq!(
            serde_json::to_string(&EvalScenario::WebSearch).unwrap(),
            "\"web_search\""
        );
        assert_eq!(EvalScenario::ZeroLossRecall.as_str(), "zero_loss_recall");
        assert_eq!(EvalScenario::ZeroLossRecall.to_string(), "zero_loss_recall");
    }

    #[test]
    fn as_str_matches_serde_representation() {
        for scenario in EvalScenario::all() {
            let json = serde_json::to_string(scenario).unwrap();
            assert_eq!(json, format!("\"{}\"", scenario.as_str()));
        }
    }
}
