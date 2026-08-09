use super::execution_support::ExecuteRequest;
use super::queries::execute;
use super::{classify_sql, query, QueryRequest, ReferenceUsageDto, SqlSafetyResult};
use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoldenCaseSpec {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub question: String,
    #[serde(default)]
    pub must_not_clarify: bool,
    #[serde(default)]
    pub should_execute: bool,
    #[serde(default)]
    pub expected_metric_terms: Vec<String>,
    #[serde(default)]
    pub required_sql_terms: Vec<String>,
    #[serde(default)]
    pub required_reference_terms: Vec<String>,
    #[serde(default)]
    pub required_reference_files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoldenCaseObservation {
    pub case_id: String,
    #[serde(default)]
    pub clarification_question: Option<String>,
    #[serde(default)]
    pub sql: Option<String>,
    #[serde(default)]
    pub executed: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub used_references: Vec<ReferenceUsageDto>,
    #[serde(default)]
    pub report_text: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvaluateGoldenCasesRequest {
    #[serde(default)]
    pub cases: Vec<GoldenCaseSpec>,
    #[serde(default)]
    pub observations: Vec<GoldenCaseObservation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoldenCaseResult {
    pub case_id: String,
    pub name: Option<String>,
    pub passed: bool,
    pub clarification_ok: bool,
    pub sql_safety_ok: bool,
    pub execution_ok: bool,
    pub metric_terms_ok: bool,
    pub sql_terms_ok: bool,
    pub reference_terms_ok: bool,
    pub reference_files_ok: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoldenCaseSummary {
    pub total: usize,
    pub passed: usize,
    pub pass_rate: f64,
    pub clarification_false_positive_rate: f64,
    pub sql_safety_rate: f64,
    pub execution_rate: f64,
    pub metric_consistency_rate: f64,
    pub reference_recall_rate: f64,
    pub reference_file_hit_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvaluateGoldenCasesResponse {
    pub summary: GoldenCaseSummary,
    pub results: Vec<GoldenCaseResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunGoldenCasesRequest {
    pub data_source_id: String,
    #[serde(default)]
    pub cases: Vec<GoldenCaseSpec>,
    #[serde(default = "default_run_execute")]
    pub execute: bool,
    #[serde(default = "default_run_limit")]
    pub limit: i64,
    #[serde(default = "default_run_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunGoldenCasesResponse {
    pub summary: GoldenCaseSummary,
    pub results: Vec<GoldenCaseResult>,
    pub observations: Vec<GoldenCaseObservation>,
}

fn default_run_execute() -> bool {
    true
}

fn default_run_limit() -> i64 {
    20
}

fn default_run_timeout_seconds() -> u32 {
    30
}

pub(crate) async fn evaluate_golden_cases_route(
    Extension(_claims): Extension<Claims>,
    Json(req): Json<EvaluateGoldenCasesRequest>,
) -> Result<Json<EvaluateGoldenCasesResponse>> {
    Ok(Json(evaluate_golden_cases(req)))
}

pub(crate) async fn run_golden_cases_route(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RunGoldenCasesRequest>,
) -> Result<Json<RunGoldenCasesResponse>> {
    Ok(Json(run_golden_cases(state, claims, req).await))
}

pub(crate) fn evaluate_golden_cases(
    req: EvaluateGoldenCasesRequest,
) -> EvaluateGoldenCasesResponse {
    let mut results = Vec::new();
    for case in &req.cases {
        let observation = req.observations.iter().find(|obs| obs.case_id == case.id);
        results.push(evaluate_one_case(case, observation));
    }
    let summary = summarize_results(&results);
    EvaluateGoldenCasesResponse { summary, results }
}

pub(crate) async fn run_golden_cases(
    state: AppState,
    claims: Claims,
    req: RunGoldenCasesRequest,
) -> RunGoldenCasesResponse {
    let mut observations = Vec::with_capacity(req.cases.len());
    let limit = req.limit.clamp(1, 100);
    let timeout_seconds = req.timeout_seconds.clamp(5, 120);

    for case in &req.cases {
        let conversation_id = req
            .conversation_id
            .as_ref()
            .map(|base| format!("{}:{}", base.trim(), case.id.trim()))
            .filter(|id| !id.trim().is_empty());
        let query_req = QueryRequest {
            data_source_id: req.data_source_id.clone(),
            question: case.question.clone(),
            conversation_id,
            route_confidence: None,
            routing_method: Some("golden_case_run".to_string()),
            semantic_context: None,
            reference_bindings: None,
        };

        let query_response = match query(
            State(state.clone()),
            Extension(claims.clone()),
            Json(query_req),
        )
        .await
        {
            Ok(Json(response)) => response,
            Err(e) => {
                observations.push(GoldenCaseObservation {
                    case_id: case.id.clone(),
                    clarification_question: None,
                    sql: None,
                    executed: if case.should_execute {
                        Some(false)
                    } else {
                        None
                    },
                    error: Some(e.to_string()),
                    used_references: Vec::new(),
                    report_text: None,
                    warnings: Vec::new(),
                });
                continue;
            }
        };

        let mut sql = query_response.sql.clone();
        let mut executed = None;
        let mut error = query_response.error.clone();
        let mut report_text = query_response.explanation.clone();
        let mut warnings = Vec::new();

        if req.execute && case.should_execute {
            if let Some(sql_text) = sql.clone().filter(|s| !s.trim().is_empty()) {
                let exec_req = ExecuteRequest {
                    query_id: query_response.query_id.clone(),
                    sql: sql_text,
                    data_source_id: req.data_source_id.clone(),
                    timeout_seconds: Some(timeout_seconds),
                    limit,
                    offset: 0,
                };
                match execute(
                    State(state.clone()),
                    Extension(claims.clone()),
                    Json(exec_req),
                )
                .await
                {
                    Ok(Json(exec_response)) => {
                        executed = Some(exec_response.error.is_none());
                        if let Some(corrected) = exec_response.corrected_sql.clone() {
                            sql = Some(corrected);
                        }
                        error = exec_response.error.clone();
                        report_text = Some(format!(
                            "executed rows_count={}, total_rows={}, columns=[{}]",
                            exec_response.rows_count,
                            exec_response.total_rows,
                            exec_response
                                .columns
                                .iter()
                                .map(|c| c.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                        if let Some(suggestions) = exec_response.suggestions {
                            warnings.extend(suggestions);
                        }
                    }
                    Err(e) => {
                        executed = Some(false);
                        error = Some(e.to_string());
                    }
                }
            } else {
                executed = Some(false);
            }
        }

        observations.push(GoldenCaseObservation {
            case_id: case.id.clone(),
            clarification_question: query_response.clarification_question,
            sql,
            executed,
            error,
            used_references: query_response.used_references,
            report_text,
            warnings,
        });
    }

    let evaluated = evaluate_golden_cases(EvaluateGoldenCasesRequest {
        cases: req.cases,
        observations: observations.clone(),
    });
    RunGoldenCasesResponse {
        summary: evaluated.summary,
        results: evaluated.results,
        observations,
    }
}

fn evaluate_one_case(
    case: &GoldenCaseSpec,
    observation: Option<&GoldenCaseObservation>,
) -> GoldenCaseResult {
    let mut issues = Vec::new();
    let Some(observation) = observation else {
        return GoldenCaseResult {
            case_id: case.id.clone(),
            name: case.name.clone(),
            passed: false,
            clarification_ok: false,
            sql_safety_ok: false,
            execution_ok: false,
            metric_terms_ok: false,
            sql_terms_ok: false,
            reference_terms_ok: false,
            reference_files_ok: false,
            issues: vec!["missing observation".to_string()],
        };
    };

    let clarified = observation
        .clarification_question
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let clarification_ok = !(case.must_not_clarify && clarified);
    if !clarification_ok {
        issues.push("clarification false positive".to_string());
    }

    let sql = observation.sql.as_deref().unwrap_or_default();
    let sql_safety_ok =
        !sql.trim().is_empty() && matches!(classify_sql(sql), SqlSafetyResult::Safe);
    if !sql_safety_ok {
        issues.push("SQL missing or unsafe".to_string());
    }

    let execution_ok = if case.should_execute {
        observation.executed.unwrap_or(false)
            && observation.error.as_deref().unwrap_or("").is_empty()
    } else {
        true
    };
    if !execution_ok {
        issues.push("expected execution did not succeed".to_string());
    }

    let combined_text = format!(
        "{}\n{}\n{}",
        sql,
        observation.report_text.as_deref().unwrap_or_default(),
        observation.warnings.join("\n")
    );
    let metric_terms_ok = terms_present(&combined_text, &case.expected_metric_terms);
    if !metric_terms_ok {
        issues.push(format!(
            "missing expected metric term(s): {}",
            missing_terms(&combined_text, &case.expected_metric_terms).join(", ")
        ));
    }
    let sql_terms_ok = terms_present(sql, &case.required_sql_terms);
    if !sql_terms_ok {
        issues.push(format!(
            "missing required SQL term(s): {}",
            missing_terms(sql, &case.required_sql_terms).join(", ")
        ));
    }

    let reference_text = observation
        .used_references
        .iter()
        .map(|r| format!("{}\n{}\n{}", r.filename, r.chunk_type, r.reason))
        .collect::<Vec<_>>()
        .join("\n");
    let reference_terms_ok = terms_present(&reference_text, &case.required_reference_terms);
    if !reference_terms_ok {
        issues.push(format!(
            "missing required reference term(s): {}",
            missing_terms(&reference_text, &case.required_reference_terms).join(", ")
        ));
    }
    let reference_files_text = observation
        .used_references
        .iter()
        .map(|r| format!("{}\n{}", r.file_id, r.filename))
        .collect::<Vec<_>>()
        .join("\n");
    let reference_files_ok = terms_present(&reference_files_text, &case.required_reference_files);
    if !reference_files_ok {
        issues.push(format!(
            "missing required reference file(s): {}",
            missing_terms(&reference_files_text, &case.required_reference_files).join(", ")
        ));
    }

    let passed = clarification_ok
        && sql_safety_ok
        && execution_ok
        && metric_terms_ok
        && sql_terms_ok
        && reference_terms_ok
        && reference_files_ok;
    GoldenCaseResult {
        case_id: case.id.clone(),
        name: case.name.clone(),
        passed,
        clarification_ok,
        sql_safety_ok,
        execution_ok,
        metric_terms_ok,
        sql_terms_ok,
        reference_terms_ok,
        reference_files_ok,
        issues,
    }
}

fn summarize_results(results: &[GoldenCaseResult]) -> GoldenCaseSummary {
    fn rate(results: &[GoldenCaseResult], f: impl Fn(&GoldenCaseResult) -> bool) -> f64 {
        if results.is_empty() {
            return 0.0;
        }
        let ok = results.iter().filter(|r| f(r)).count();
        ok as f64 / results.len() as f64
    }

    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    GoldenCaseSummary {
        total,
        passed,
        pass_rate: if total == 0 {
            0.0
        } else {
            passed as f64 / total as f64
        },
        clarification_false_positive_rate: if total == 0 {
            0.0
        } else {
            results.iter().filter(|r| !r.clarification_ok).count() as f64 / total as f64
        },
        sql_safety_rate: rate(results, |r| r.sql_safety_ok),
        execution_rate: rate(results, |r| r.execution_ok),
        metric_consistency_rate: rate(results, |r| r.metric_terms_ok),
        reference_recall_rate: rate(results, |r| r.reference_terms_ok),
        reference_file_hit_rate: rate(results, |r| r.reference_files_ok),
    }
}

fn terms_present(text: &str, terms: &[String]) -> bool {
    missing_terms(text, terms).is_empty()
}

fn missing_terms(text: &str, terms: &[String]) -> Vec<String> {
    let haystack = text.to_lowercase();
    terms
        .iter()
        .filter_map(|term| {
            let term = term.trim();
            if term.is_empty() || haystack.contains(&term.to_lowercase()) {
                None
            } else {
                Some(term.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(filename: &str) -> ReferenceUsageDto {
        ReferenceUsageDto {
            pack_id: "pack".to_string(),
            pack_name: "SQL Knowledge".to_string(),
            file_id: "file".to_string(),
            filename: filename.to_string(),
            chunk_id: "chunk".to_string(),
            language: Some("sql".to_string()),
            chunk_type: "sql_example".to_string(),
            start_line: 1,
            end_line: 10,
            score: 2.0,
            reason: "matched metric".to_string(),
            verified: true,
            stale: false,
            preview: "SELECT AVG(ecpm) AS ecpm FROM ads".to_string(),
        }
    }

    #[test]
    fn golden_case_evaluator_tracks_core_rates() {
        let response = evaluate_golden_cases(EvaluateGoldenCasesRequest {
            cases: vec![GoldenCaseSpec {
                id: "ecpm".to_string(),
                name: Some("eCPM daily".to_string()),
                question: "昨天 ecpm 是多少".to_string(),
                must_not_clarify: true,
                should_execute: true,
                expected_metric_terms: vec!["ecpm".to_string()],
                required_sql_terms: vec!["avg".to_string(), "ecpm".to_string()],
                required_reference_terms: vec!["ecpm".to_string()],
                required_reference_files: vec!["ecpm_metric.sql".to_string()],
            }],
            observations: vec![GoldenCaseObservation {
                case_id: "ecpm".to_string(),
                clarification_question: None,
                sql: Some("SELECT AVG(ecpm) AS ecpm FROM ads".to_string()),
                executed: Some(true),
                error: None,
                used_references: vec![reference("ecpm_metric.sql")],
                report_text: Some("ecpm = 1.2".to_string()),
                warnings: Vec::new(),
            }],
        });

        assert_eq!(response.summary.total, 1);
        assert_eq!(response.summary.passed, 1);
        assert_eq!(response.summary.clarification_false_positive_rate, 0.0);
        assert_eq!(response.summary.reference_file_hit_rate, 1.0);
        assert!(response.results[0].passed);
    }

    #[test]
    fn golden_case_evaluator_flags_clarification_false_positive() {
        let response = evaluate_golden_cases(EvaluateGoldenCasesRequest {
            cases: vec![GoldenCaseSpec {
                id: "order".to_string(),
                name: None,
                question: "查最近订单".to_string(),
                must_not_clarify: true,
                should_execute: false,
                expected_metric_terms: Vec::new(),
                required_sql_terms: Vec::new(),
                required_reference_terms: Vec::new(),
                required_reference_files: Vec::new(),
            }],
            observations: vec![GoldenCaseObservation {
                case_id: "order".to_string(),
                clarification_question: Some("你要查哪个表？".to_string()),
                sql: Some("SELECT * FROM business_order LIMIT 10".to_string()),
                executed: None,
                error: None,
                used_references: Vec::new(),
                report_text: None,
                warnings: Vec::new(),
            }],
        });

        assert_eq!(response.summary.clarification_false_positive_rate, 1.0);
        assert!(!response.results[0].clarification_ok);
    }

    #[test]
    fn golden_case_run_request_defaults_are_bounded() {
        let req: RunGoldenCasesRequest = serde_json::from_value(serde_json::json!({
            "dataSourceId": "ds",
            "cases": []
        }))
        .expect("run request");

        assert!(req.execute);
        assert_eq!(req.limit, 20);
        assert_eq!(req.timeout_seconds, 30);
    }
}
