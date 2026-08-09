//! ZeroLossRecall scenario — reuse the existing 0-loss measurement (需求 2.3).
//!
//! Design reference: `.kiro/specs/codex-parity-gaps/design.md` section
//! "2. 评测 Harness" (`eval_zero_loss`) and the `ZeroLossMeasurement`
//! (复用 super-assistant-hub) data model.
//!
//! # Requirement 2.3 — reuse, do not re-implement
//!
//! Requirement 2.3 mandates:
//!
//! > THE Eval_Harness SHALL 复用现有 ZeroLossMeasurement 计算上下文 0 丢失
//! > 召回率,不新建并行的召回度量实现。
//!
//! The authoritative 0-loss recall measurement already lives in the
//! **`web-server`** crate:
//! `rust/crates/web-server/src/routes/super_assistant.rs`
//!   - [`collect_zero_loss_measurement`] — DB-backed probe replay through the
//!     reused `build_injection_bundle`, producing a `ZeroLossMeasurement`.
//!   - `build_zero_loss_probes` → `probe_recalled` → `count_recalled_probes` →
//!     `ZeroLossMeasurement::compute` — the actual recall detection & counting.
//!
//! This module contains **no recall detection or counting logic**. It maps a
//! `ZeroLossMeasurement` (produced by the reused collection path) into a
//! [`ScenarioScore`], taking the already-computed `recall_rate` verbatim as the
//! scenario's AOS score.
//!
//! # Integration boundary (why this is a mapping, not a direct call)
//!
//! The harness cannot call `collect_zero_loss_measurement` directly:
//!
//! 1. **Visibility.** `web-server`'s `routes` module is private
//!    (`mod routes;` in `web-server/src/lib.rs`), so
//!    `collect_zero_loss_measurement` / `ZeroLossMeasurement` are not part of
//!    `web-server`'s public API and cannot be named from another crate.
//! 2. **Runtime coupling.** `collect_zero_loss_measurement` is `async` and
//!    requires a live `&AppState` (DB pool, embedding stores, agent manager).
//!    A reproducible, CI-friendly harness crate must not carry that runtime.
//! 3. **Dependency direction.** `web-server` already depends on `runtime`,
//!    `agent-gateway`, `tools`, `sqlx`, `axum`, … . Making `eval-harness`
//!    depend on `web-server` would invert a sensible direction and risk a
//!    dependency cycle if `web-server` later exposes an eval endpoint that
//!    calls this crate.
//!
//! **Integration contract.** The DB-backed collection stays in `web-server`:
//! the harness runner (or a thin `web-server`-side adapter / eval endpoint)
//! calls `collect_zero_loss_measurement`, serializes the resulting
//! `ZeroLossMeasurement`, and hands it to [`score_zero_loss_recall`] /
//! [`score_from_measurement`] here. [`ZeroLossMeasurementView`] is
//! **wire-compatible** (identical camelCase fields) with `web-server`'s
//! `ZeroLossMeasurement`, so a serialized production measurement deserializes
//! directly into it with no re-computation. When the workspace wiring allows a
//! direct in-process call (e.g. a `web-server` eval adapter), that adapter can
//! construct [`ZeroLossMeasurementView`] field-for-field from the real
//! `ZeroLossMeasurement` instead of going through JSON.

use serde::{Deserialize, Serialize};

use crate::report::{CaseFailure, ScenarioScore};
use crate::scenario::EvalScenario;

/// Metric name reported for the ZeroLossRecall scenario.
///
/// Matches the `recall_rate` metric produced by the reused
/// `ZeroLossMeasurement` (as opposed to the `success_rate` used by
/// success-oriented scenarios).
pub const ZERO_LOSS_METRIC_NAME: &str = "recall_rate";

/// Default 0-loss recall threshold — super-assistant-hub 既定阈值 `0.99`
/// (Requirements 1.9 / 3.7). Used when neither an override nor a
/// measurement-carried threshold is available.
pub const ZERO_LOSS_DEFAULT_THRESHOLD: f64 = 0.99;

/// Wire-compatible mirror of `web-server`'s `ZeroLossMeasurement`.
///
/// Field names and the camelCase wire shape are identical to the backend
/// `ZeroLossMeasurement` (see
/// `web-server/src/routes/super_assistant.rs`), so a production measurement
/// serialized by the reused `collect_zero_loss_measurement` deserializes
/// directly into this view. This is the harness's integration boundary DTO —
/// it carries the already-computed `recall_rate`/`passed` and is **not** a
/// re-implementation of the recall metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZeroLossMeasurementView {
    /// Session the measurement was taken for.
    #[serde(default)]
    pub session_id: String,
    /// Number of probe questions against established key facts.
    pub probe_count: usize,
    /// Probes whose fact resurfaced in the post-compaction injection bundle.
    pub recalled_count: usize,
    /// `recalled_count / probe_count` as computed by the reused measurement
    /// (0 when there were no probes). Taken verbatim as the scenario score.
    pub recall_rate: f64,
    /// Pass threshold the measurement was judged against (default
    /// [`ZERO_LOSS_DEFAULT_THRESHOLD`]).
    pub threshold: f64,
    /// True **iff** `recall_rate >= threshold`, as judged by the reused
    /// measurement.
    pub passed: bool,
    /// Removed-message count reconciled against `session_compacted`.
    #[serde(default)]
    pub removed_messages: usize,
    /// Summary token count reconciled against `session_compacted`.
    #[serde(default)]
    pub summary_tokens: usize,
    /// ISO-8601 timestamp of when the measurement was collected.
    #[serde(default)]
    pub measured_at: String,
}

/// Resolve the pass threshold for the scenario score.
///
/// Preference order: explicit `threshold_override` → the threshold carried by
/// the reused measurements (they are expected to agree; the first is used) →
/// [`ZERO_LOSS_DEFAULT_THRESHOLD`].
fn resolve_threshold(
    measurements: &[ZeroLossMeasurementView],
    threshold_override: Option<f64>,
) -> f64 {
    threshold_override
        .or_else(|| measurements.first().map(|m| m.threshold))
        .unwrap_or(ZERO_LOSS_DEFAULT_THRESHOLD)
}

/// Map a single reused [`ZeroLossMeasurementView`] to a [`ScenarioScore`].
///
/// The AOS score is the measurement's `recall_rate` **verbatim** — no recall
/// re-computation (Requirement 2.3). The threshold defaults to the value the
/// measurement was judged against unless `threshold_override` is supplied, so
/// the derived `ScenarioScore.passed` matches the measurement's own `passed`.
#[must_use]
pub fn score_from_measurement(
    measurement: &ZeroLossMeasurementView,
    codex_baseline: Option<f64>,
    threshold_override: Option<f64>,
) -> ScenarioScore {
    let threshold = threshold_override.unwrap_or(measurement.threshold);
    ScenarioScore::new(
        EvalScenario::ZeroLossRecall,
        ZERO_LOSS_METRIC_NAME,
        measurement.recall_rate,
        codex_baseline,
        Some(threshold),
        Vec::new(),
    )
}

/// Build the ZeroLossRecall [`ScenarioScore`] from reused 0-loss measurements.
///
/// Consumes the `ZeroLossMeasurement`s produced by the reused
/// `collect_zero_loss_measurement` (one per probed session/case in the dataset)
/// and reports the aggregate 0-loss recall rate.
///
/// - Zero measurements → recall rate `0.0` (nothing recalled), judged against
///   the resolved threshold.
/// - One measurement → its `recall_rate` is used verbatim.
/// - Many measurements → **micro-averaged** over the counts the measurements
///   already carry: `Σ recalled_count / Σ probe_count`. This weights each
///   session by how many facts it probed. Per-session recall is **not**
///   re-derived here — each `recalled_count` / `probe_count` comes from the
///   reused measurement's own probe replay (Requirement 2.3); this step is
///   pure reporting-level aggregation over reused counts, not a parallel recall
///   metric.
///
/// `case_failures` carries any failed/timed-out cases the runner collected so
/// the scenario stays best-effort (Requirement 2.8); this function does not
/// generate failures itself.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn score_zero_loss_recall(
    measurements: &[ZeroLossMeasurementView],
    codex_baseline: Option<f64>,
    threshold_override: Option<f64>,
    case_failures: Vec<CaseFailure>,
) -> ScenarioScore {
    let threshold = resolve_threshold(measurements, threshold_override);

    let total_probes: usize = measurements.iter().map(|m| m.probe_count).sum();
    let recall_rate = if measurements.is_empty() {
        0.0
    } else if measurements.len() == 1 {
        // Single measurement: use the reused recall rate verbatim.
        measurements[0].recall_rate
    } else if total_probes == 0 {
        // Sessions existed but nothing was probed — nothing to recall.
        0.0
    } else {
        let total_recalled: usize = measurements.iter().map(|m| m.recalled_count).sum();
        total_recalled as f64 / total_probes as f64
    };

    ScenarioScore::new(
        EvalScenario::ZeroLossRecall,
        ZERO_LOSS_METRIC_NAME,
        recall_rate,
        codex_baseline,
        Some(threshold),
        case_failures,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A production `ZeroLossMeasurement` JSON as emitted by web-server's
    /// camelCase serialization deserializes directly into the view.
    #[test]
    fn view_deserializes_from_web_server_measurement_json() {
        let json = r#"{
            "sessionId": "sess-1",
            "probeCount": 10,
            "recalledCount": 10,
            "recallRate": 1.0,
            "threshold": 0.99,
            "passed": true,
            "removedMessages": 4,
            "summaryTokens": 128,
            "measuredAt": "2024-01-01T00:00:00Z"
        }"#;
        let view: ZeroLossMeasurementView = serde_json::from_str(json).unwrap();
        assert_eq!(view.session_id, "sess-1");
        assert_eq!(view.probe_count, 10);
        assert_eq!(view.recalled_count, 10);
        assert!((view.recall_rate - 1.0).abs() < 1e-9);
        assert!((view.threshold - 0.99).abs() < 1e-9);
        assert!(view.passed);
        assert_eq!(view.removed_messages, 4);
        assert_eq!(view.summary_tokens, 128);
        assert_eq!(view.measured_at, "2024-01-01T00:00:00Z");
    }

    #[test]
    fn view_round_trips_through_json() {
        let view = ZeroLossMeasurementView {
            session_id: "s".into(),
            probe_count: 3,
            recalled_count: 2,
            recall_rate: 2.0 / 3.0,
            threshold: 0.99,
            passed: false,
            removed_messages: 1,
            summary_tokens: 10,
            measured_at: "t".into(),
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"recallRate\""), "json = {json}");
        assert!(json.contains("\"probeCount\":3"), "json = {json}");
        let decoded: ZeroLossMeasurementView = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, view);
    }

    #[test]
    fn score_from_measurement_uses_recall_rate_verbatim() {
        let view = ZeroLossMeasurementView {
            session_id: "s".into(),
            probe_count: 100,
            recalled_count: 99,
            recall_rate: 0.99,
            threshold: 0.99,
            passed: true,
            removed_messages: 0,
            summary_tokens: 0,
            measured_at: String::new(),
        };
        let score = score_from_measurement(&view, None, None);
        assert_eq!(score.scenario, EvalScenario::ZeroLossRecall);
        assert_eq!(score.metric_name, "recall_rate");
        assert!((score.aos_score - 0.99).abs() < 1e-9);
        assert_eq!(score.threshold, Some(0.99));
        // Derived passed matches the reused measurement's own verdict.
        assert_eq!(score.passed, Some(true));
        assert_eq!(score.passed, Some(view.passed));
    }

    #[test]
    fn score_from_measurement_marks_below_threshold_as_failed() {
        let view = ZeroLossMeasurementView {
            session_id: "s".into(),
            probe_count: 10,
            recalled_count: 8,
            recall_rate: 0.8,
            threshold: 0.99,
            passed: false,
            removed_messages: 0,
            summary_tokens: 0,
            measured_at: String::new(),
        };
        let score = score_from_measurement(&view, None, None);
        assert_eq!(score.passed, Some(false));
        assert!(score.is_below_threshold());
    }

    #[test]
    fn score_from_measurement_honours_threshold_override_and_baseline() {
        let view = ZeroLossMeasurementView {
            session_id: "s".into(),
            probe_count: 10,
            recalled_count: 9,
            recall_rate: 0.9,
            threshold: 0.99,
            passed: false,
            removed_messages: 0,
            summary_tokens: 0,
            measured_at: String::new(),
        };
        let score = score_from_measurement(&view, Some(0.85), Some(0.90));
        assert_eq!(score.threshold, Some(0.90));
        // 0.9 >= 0.90 → passes under the override.
        assert_eq!(score.passed, Some(true));
        // delta = aos_score - codex_baseline = 0.9 - 0.85.
        let delta = score.delta.expect("baseline present → delta present");
        assert!((delta - 0.05).abs() < 1e-9, "delta = {delta}");
    }

    #[test]
    fn score_zero_loss_recall_single_uses_recall_rate_verbatim() {
        let m = ZeroLossMeasurementView {
            session_id: "s".into(),
            probe_count: 7,
            recalled_count: 6,
            recall_rate: 6.0 / 7.0,
            threshold: 0.99,
            passed: false,
            removed_messages: 0,
            summary_tokens: 0,
            measured_at: String::new(),
        };
        let score = score_zero_loss_recall(std::slice::from_ref(&m), None, None, Vec::new());
        assert!((score.aos_score - 6.0 / 7.0).abs() < 1e-9);
        assert_eq!(score.threshold, Some(0.99));
    }

    #[test]
    fn score_zero_loss_recall_micro_averages_multiple_sessions() {
        // Session A: 8/10, Session B: 1/2. Micro-average = 9/12 = 0.75.
        let measurements = vec![
            ZeroLossMeasurementView {
                session_id: "a".into(),
                probe_count: 10,
                recalled_count: 8,
                recall_rate: 0.8,
                threshold: 0.99,
                passed: false,
                removed_messages: 0,
                summary_tokens: 0,
                measured_at: String::new(),
            },
            ZeroLossMeasurementView {
                session_id: "b".into(),
                probe_count: 2,
                recalled_count: 1,
                recall_rate: 0.5,
                threshold: 0.99,
                passed: false,
                removed_messages: 0,
                summary_tokens: 0,
                measured_at: String::new(),
            },
        ];
        let score = score_zero_loss_recall(&measurements, None, None, Vec::new());
        assert!(
            (score.aos_score - 0.75).abs() < 1e-9,
            "score = {}",
            score.aos_score
        );
        assert_eq!(score.passed, Some(false));
    }

    #[test]
    fn score_zero_loss_recall_empty_is_zero_and_below_default_threshold() {
        let score = score_zero_loss_recall(&[], None, None, Vec::new());
        assert!((score.aos_score - 0.0).abs() < 1e-9);
        assert_eq!(score.threshold, Some(ZERO_LOSS_DEFAULT_THRESHOLD));
        assert_eq!(score.passed, Some(false));
    }

    #[test]
    fn score_zero_loss_recall_all_sessions_zero_probes_is_zero() {
        let measurements = vec![
            ZeroLossMeasurementView {
                session_id: "a".into(),
                probe_count: 0,
                recalled_count: 0,
                recall_rate: 0.0,
                threshold: 0.99,
                passed: false,
                removed_messages: 0,
                summary_tokens: 0,
                measured_at: String::new(),
            },
            ZeroLossMeasurementView {
                session_id: "b".into(),
                probe_count: 0,
                recalled_count: 0,
                recall_rate: 0.0,
                threshold: 0.99,
                passed: false,
                removed_messages: 0,
                summary_tokens: 0,
                measured_at: String::new(),
            },
        ];
        let score = score_zero_loss_recall(&measurements, None, None, Vec::new());
        assert!((score.aos_score - 0.0).abs() < 1e-9);
    }

    #[test]
    fn score_zero_loss_recall_preserves_case_failures() {
        let m = ZeroLossMeasurementView {
            session_id: "s".into(),
            probe_count: 4,
            recalled_count: 4,
            recall_rate: 1.0,
            threshold: 0.99,
            passed: true,
            removed_messages: 0,
            summary_tokens: 0,
            measured_at: String::new(),
        };
        let failures = vec![CaseFailure::new("case-timeout", "timed out")];
        let score =
            score_zero_loss_recall(std::slice::from_ref(&m), Some(0.97), None, failures.clone());
        assert_eq!(score.case_failures, failures);
        assert_eq!(score.codex_baseline, Some(0.97));
        let delta = score.delta.expect("baseline present");
        assert!((delta - 0.03).abs() < 1e-9, "delta = {delta}");
    }
}
