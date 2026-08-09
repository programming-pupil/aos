//! Per-scenario scoring for the evaluation harness.
//!
//! Each [`EvalScenario`](crate::scenario::EvalScenario) maps its domain-specific
//! measurement into a [`ScenarioScore`](crate::report::ScenarioScore). This
//! module hosts one submodule per scenario as they are implemented.
//!
//! ## ZeroLossRecall (Requirement 2.3)
//!
//! The [`zero_loss`] submodule implements the ZeroLossRecall scenario by
//! **reusing** the existing 0-loss measurement — `collect_zero_loss_measurement`
//! / `ZeroLossMeasurement` in
//! `rust/crates/web-server/src/routes/super_assistant.rs`. It deliberately does
//! **not** build a parallel recall-metric implementation: the harness consumes
//! the recall rate that the reused measurement already computed and only maps it
//! into a [`ScenarioScore`]. See [`zero_loss`] for the integration boundary.

pub mod zero_loss;

pub use zero_loss::{
    score_from_measurement, score_zero_loss_recall, ZeroLossMeasurementView,
    ZERO_LOSS_DEFAULT_THRESHOLD, ZERO_LOSS_METRIC_NAME,
};
