//! # eval-harness
//!
//! 自动化准确度评测 harness(需求 2)。可复现、可纳入 CI 的评测框架,覆盖
//! 六个 [`EvalScenario`]:普通聊天、联网检索、SQL 归因、深度报告、编码任务
//! 成功率、上下文 0 丢失召回率。产出可与 Codex 基准对比的结构化报表。
//!
//! 本 crate 遵循 spec 总原则「复用优先、不推倒重做」:0 丢失召回场景复用
//! 现有 `ZeroLossMeasurement`,不新建并行的召回度量实现。
//!
//! ## 当前进度
//!
//! - 3.1 场景枚举骨架([`EvalScenario`])。
//! - 3.2 [`EvalReport`] / [`ScenarioScore`] / [`CaseFailure`] 数据模型与
//!   delta / 达标阈值判定。
//! - 3.3 [`run_eval`] runner(固定种子可复现 + best-effort 用例失败收集 +
//!   CI 退出码,经 [`run_eval_ci`] / [`exit_code_for`])。
//! - 3.4 `ZeroLossRecall` 场景复用现有 0 丢失度量
//!   ([`scenarios::zero_loss`]):映射 web-server 的 `ZeroLossMeasurement`
//!   到 [`ScenarioScore`],不新建并行召回度量。
//!
//! - 3.5 免责 / 定位固化([`positioning`])并注入报表;评测数据集目录
//!   (`eval/datasets/`,固定种子、可复现)与对比文档
//!   (`docs/AOS_VS_CODEX_EVAL.md`)。
//!
//! Design reference: `.kiro/specs/codex-parity-gaps/design.md` 第 2 节。

pub mod conformance;
pub mod dataset;
pub mod parity;
pub mod positioning;
pub mod replay;
pub mod report;
pub mod runner;
pub mod scenario;
pub mod scenarios;

pub use dataset::{default_eval_config, eval_config_from_dataset_str, DatasetError};
pub use positioning::{DISCLAIMER, DISCLAIMER_MARKER, POSITIONING, POSITIONING_MARKER};
pub use report::{CaseFailure, EvalReport, ScenarioScore};
pub use runner::{
    exit_code_for, run_eval, run_eval_ci, CaseSpec, EvalCase, EvalConfig, ScenarioPlan, EXIT_FAIL,
    EXIT_PASS,
};
pub use scenario::EvalScenario;
pub use scenarios::zero_loss::{
    score_from_measurement, score_zero_loss_recall, ZeroLossMeasurementView,
    ZERO_LOSS_DEFAULT_THRESHOLD, ZERO_LOSS_METRIC_NAME,
};
