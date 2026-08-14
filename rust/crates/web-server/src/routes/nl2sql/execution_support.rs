//! Shared execution support models for NL2SQL.
//!
//! Query execution, result masking, and the multi-datasource agent all need
//! these DTOs and retry helpers. Keeping them out of `mod.rs` prevents the
//! route root from becoming the implicit home for execution-domain state.

use serde::{Deserialize, Serialize};

use super::AppliedRuleHit;

#[derive(Debug, Deserialize)]
pub(crate) struct ExecuteRequest {
    pub query_id: String,
    pub sql: String,
    pub data_source_id: String,
    /// Optional per-request query execution timeout in seconds.
    /// Defaults to 30 seconds if not specified.
    pub timeout_seconds: Option<u32>,
    /// Legacy UI pagination hint. The execution layer does not append hidden
    /// LIMIT/OFFSET clauses; users must see and edit row limits in the SQL text.
    /// Defaults to 10 for backward-compatible response shaping only.
    #[serde(default = "default_execute_limit")]
    pub limit: i64,
    /// Legacy UI pagination hint. Not applied to the SQL text.
    /// Defaults to 0.
    #[serde(default)]
    pub offset: i64,
}

fn default_execute_limit() -> i64 {
    10
}

/// Error classification for SQL execution failures.
#[derive(Debug)]
pub(crate) enum SqlExecErrorKind {
    /// Table referenced in the SQL does not exist.
    TableNotFound,
    /// Column referenced in the SQL does not exist.
    ColumnNotFound,
    /// SQL syntax is invalid for the target engine/dialect.
    SyntaxError,
    /// SQL is parseable but violates target-engine semantic rules.
    SemanticError,
    /// The query is valid, but one or more referenced partitions/files are
    /// unavailable. The model may choose a narrower diagnostic query, but the
    /// result must not silently replace the user's requested scope.
    DataUnavailable,
    /// A generic execution error (network, auth, timeout, etc.).
    Other(String),
}

impl SqlExecErrorKind {
    pub(crate) fn new(msg: &str) -> Self {
        let msg_lower = msg.to_lowercase();
        if msg_lower.contains("doesn't exist") || msg_lower.contains("doesn't have a column") {
            if msg_lower.contains("table") || msg_lower.contains("doesn't have a column") {
                if msg_lower.contains("column") {
                    return Self::ColumnNotFound;
                }
                return Self::TableNotFound;
            }
        }
        if msg_lower.contains("table")
            && (msg_lower.contains("doesn't exist")
                || msg_lower.contains("not found")
                || msg_lower.contains("doesn't have"))
        {
            return Self::TableNotFound;
        }
        if msg_lower.contains("column")
            && (msg_lower.contains("doesn't exist")
                || msg_lower.contains("not found")
                || msg_lower.contains("unknown column")
                || msg_lower.contains("cannot be resolved")
                || msg_lower.contains("column_not_found"))
        {
            return Self::ColumnNotFound;
        }
        if msg_lower.contains("syntax")
            || msg_lower.contains("parse error")
            || msg_lower.contains("parser")
            || msg_lower.contains("mismatched input")
            || msg_lower.contains("sqlstate 42000")
            || msg_lower.contains("(42000)")
            || msg_lower.contains("error code 1064")
            || msg_lower.contains("near \"with recursive")
            || msg_lower.contains("near 'with recursive")
            || msg_lower.contains("with recursive is not supported")
            || msg_lower.contains("recursive cte")
        {
            return Self::SyntaxError;
        }
        if [
            "nested_window",
            "cannot nest window",
            "expression_not_aggregate",
            "type_mismatch",
            "function_not_found",
            "invalid_function_argument",
            "ambiguous_name",
            "column is ambiguous",
            "column reference is ambiguous",
        ]
        .iter()
        .any(|marker| msg_lower.contains(marker))
        {
            return Self::SemanticError;
        }
        if [
            "hive_file_not_found",
            "partition location does not exist",
            "partition_location_does_not_exist",
            "partition not found",
            "no files found for partition",
            "cannot open split",
        ]
        .iter()
        .any(|marker| msg_lower.contains(marker))
        {
            return Self::DataUnavailable;
        }
        Self::Other(msg.to_string())
    }

    pub(crate) fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::TableNotFound
                | Self::ColumnNotFound
                | Self::SyntaxError
                | Self::SemanticError
                | Self::DataUnavailable
        ) || matches!(
            self,
            Self::Other(msg) if msg.to_lowercase().contains("only_full_group_by")
        )
    }

    /// Operational failures that are safe to retry with the exact same SQL.
    /// These must not enter LLM SQL correction because the query text is not
    /// what caused a gateway, rate-limit, or transport failure.
    pub(crate) fn is_transient_operational(&self) -> bool {
        let Self::Other(message) = self else {
            return false;
        };
        let message = message.to_ascii_lowercase();
        [
            "gateway timeout",
            "gateway time-out",
            "upstream request timeout",
            "bad gateway",
            "service unavailable",
            "temporarily unavailable",
            "too many requests",
            "rate limit",
            "connection reset",
            "connection closed",
            "connection refused",
            "broken pipe",
            "unexpected eof",
            "http status 429",
            "http status 502",
            "http status 503",
            "http status 504",
            "http 504",
            "status code 504",
            "504 gateway",
            "code: 429",
            "code: 502",
            "code: 503",
            "code: 504",
            "status=429",
            "status=502",
            "status=503",
            "status=504",
        ]
        .iter()
        .any(|marker| message.contains(marker))
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::TableNotFound => "table_not_found",
            Self::ColumnNotFound => "column_not_found",
            Self::SyntaxError => "syntax_error",
            Self::SemanticError => "semantic_error",
            Self::DataUnavailable => "data_unavailable",
            Self::Other(_) => "operational_error",
        }
    }

    pub(crate) fn allows_model_recovery_strategy(&self) -> bool {
        matches!(self, Self::DataUnavailable)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SqlRepairDecision {
    pub strategy: Option<String>,
    pub scope_changed: bool,
    pub diagnostic_only: bool,
    pub rationale: Option<String>,
}

/// Tracks a single SQL correction attempt.
#[derive(Debug, Clone)]
struct CorrectAttempt {
    sql: String,
    error: String,
}

/// Maintains the history of SQL correction attempts across retries.
#[derive(Debug, Clone, Default)]
pub(crate) struct SelfCorrectContext {
    attempts: Vec<CorrectAttempt>,
    last_decision: Option<SqlRepairDecision>,
}

impl SelfCorrectContext {
    pub(crate) fn new(initial_sql: &str, initial_error: &str) -> Self {
        Self {
            attempts: vec![CorrectAttempt {
                sql: initial_sql.to_string(),
                error: initial_error.to_string(),
            }],
            last_decision: None,
        }
    }

    pub(crate) fn add(&mut self, sql: String, error: String) {
        self.attempts.push(CorrectAttempt { sql, error });
    }

    pub(crate) fn history_text(&self) -> String {
        self.attempts
            .iter()
            .map(|a| format!("Attempt:\nSQL: {}\nError: {}", a.sql, a.error))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    pub(crate) fn last_sql(&self) -> String {
        self.attempts
            .last()
            .map(|a| a.sql.clone())
            .unwrap_or_default()
    }

    pub(crate) fn last_error(&self) -> String {
        self.attempts
            .last()
            .map(|a| a.error.clone())
            .unwrap_or_default()
    }

    pub(crate) fn set_last_decision(&mut self, decision: Option<SqlRepairDecision>) {
        self.last_decision = decision;
    }

    pub(crate) fn take_last_decision(&mut self) -> Option<SqlRepairDecision> {
        self.last_decision.take()
    }
}

#[derive(Debug, Serialize)]
pub struct ExecuteResponse {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<serde_json::Value>,
    pub rows_count: usize,
    pub total_rows: i64,
    pub has_more: bool,
    pub limit: i64,
    pub offset: i64,
    pub execution_ms: u64,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrected_sql: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_correct_failed: Option<bool>,
    #[serde(default)]
    pub diagnostic_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<crate::nl2sql::result_validator::ValidationWarning>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub applied_rules: Vec<AppliedRuleHit>,
}

#[derive(Debug, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub masked: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::SqlExecErrorKind;

    #[test]
    fn tidb_mysql_syntax_errors_are_retryable_for_self_correction() {
        let err = "mysql query failed: error returned from database: 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your TiDB version near \"WITH RECURSIVE date_spine AS\"";
        assert!(SqlExecErrorKind::new(err).is_retryable());
    }

    #[test]
    fn trino_semantic_errors_are_retryable_but_not_sent_to_sql_repair() {
        assert!(SqlExecErrorKind::new(
            "Query failed with NESTED_WINDOW: Cannot nest window functions"
        )
        .is_retryable());
        assert!(
            SqlExecErrorKind::new("COLUMN_NOT_FOUND: Column 'revenue' cannot be resolved")
                .is_retryable()
        );
        assert!(SqlExecErrorKind::new(
            "Query failed with AMBIGUOUS_NAME: Column 'app_id' is ambiguous"
        )
        .is_retryable());
        assert!(!SqlExecErrorKind::new("Query execution timed out after 60s").is_retryable());
        assert!(!SqlExecErrorKind::new("connector does not support EXPLAIN").is_retryable());
        assert!(!SqlExecErrorKind::new("authentication failed").is_retryable());
    }

    #[test]
    fn transient_gateway_failures_retry_the_same_sql_only() {
        for error in [
            "trino query failed: http not ok, code: 504 Gateway Timeout, reason: upstream request timeout",
            "upstream returned 504 Gateway Time-out",
            "HTTP status 503 Service Unavailable",
            "429 Too Many Requests",
            "connection reset by peer",
        ] {
            let kind = SqlExecErrorKind::new(error);
            assert!(kind.is_transient_operational(), "{error}");
            assert!(!kind.is_retryable(), "{error}");
        }
        assert!(!SqlExecErrorKind::new("authentication failed").is_transient_operational());
        assert!(
            !SqlExecErrorKind::new("Query execution timed out after 60s")
                .is_transient_operational()
        );
    }

    #[test]
    fn unavailable_trino_partitions_allow_model_guided_recovery() {
        for error in [
            "Query failed (#1): HIVE_FILE_NOT_FOUND: Partition location does not exist: obs://bucket/table/dt=20220307",
            "Trino query failed: cannot open split for hive.prod.events partition dt=2026-08-01",
            "no files found for partition ds=20260801",
        ] {
            let kind = SqlExecErrorKind::new(error);
            assert_eq!(kind.label(), "data_unavailable", "{error}");
            assert!(kind.is_retryable(), "{error}");
            assert!(kind.allows_model_recovery_strategy(), "{error}");
            assert!(!kind.is_transient_operational(), "{error}");
        }
    }

    #[test]
    fn permission_and_authentication_errors_never_change_query_scope() {
        for error in [
            "Access Denied: Cannot select from columns [revenue]",
            "HTTP 401 authentication failed",
            "User does not have permission to query table hive.prod.orders",
        ] {
            let kind = SqlExecErrorKind::new(error);
            assert!(!kind.is_retryable(), "{error}");
            assert!(!kind.allows_model_recovery_strategy(), "{error}");
        }
    }
}
