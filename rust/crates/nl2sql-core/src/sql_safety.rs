//! SQL safety classifier — verifies that user-bound SQL is a single read-only statement
//! AND that it does not invoke MySQL/TiDB primitives that escape the data plane
//! (filesystem reads, sleeps, connection metadata, XML/XPath injection sinks, …).
//!
//! ## Defense in depth
//!
//! 1. **Lexical guard**: a tight regex pass rejects `INTO OUTFILE` / `INTO DUMPFILE`
//!    clauses (these are pure side-effects — they write to the database server's
//!    filesystem and have no place in a read-only NL2SQL surface).
//! 2. **Parser-level**: `sqlparser` rejects anything that isn't `Statement::Query`,
//!    blocking `INSERT` / `UPDATE` / `DELETE` / `DROP` / `ALTER` / `TRUNCATE` /
//!    `GRANT` / stacked statements (`SELECT 1; DROP …`).
//! 3. **AST visitor**: walks the parsed `Query` AST via `sqlparser`'s
//!    `Visitor` trait and rejects any call to a dangerous built-in
//!    (`LOAD_FILE`, `SLEEP`, `BENCHMARK`, `GET_LOCK`, `EXTRACTVALUE`,
//!    `UPDATEXML`, …). Using the official visitor guarantees every nested
//!    expression — including CTEs, subqueries, set operations, window
//!    functions and UDF args — is visited.

use std::ops::ControlFlow;
use std::sync::OnceLock;

use sqlparser::ast::{Expr, Statement, Visit, Visitor};

/// Classified result of a SQL safety check.
#[allow(dead_code)]
#[derive(Debug)]
pub enum SqlSafetyResult {
    /// The SQL is a safe read-only statement.
    Safe,
    /// The SQL is syntactically invalid.
    SyntaxError { message: String },
    /// The SQL contains disallowed operations (non-SELECT).
    ForbiddenOperation { statement_type: String },
    /// Multiple statements detected; only single-statement SQL is allowed.
    MultipleStatements,
    /// SQL invokes a built-in function that escapes the read-only contract
    /// (e.g. `LOAD_FILE`, `SLEEP`, `BENCHMARK`).
    ForbiddenFunction { function_name: String },
    /// SQL contains `INTO OUTFILE` / `INTO DUMPFILE` — these write to the database
    /// server's filesystem and are never allowed via NL2SQL.
    ForbiddenIntoClause,
}

/// MySQL / TiDB built-in functions blocked from NL2SQL surfaces.
///
/// Categories:
/// * **Filesystem**: `LOAD_FILE` reads server-local files; the matching
///   `INTO OUTFILE` write path is handled by [`into_clause_regex`].
/// * **Side-effect / amplification**: `SLEEP`, `BENCHMARK`, `GET_LOCK`,
///   `RELEASE_LOCK`, `RELEASE_ALL_LOCKS`, `IS_FREE_LOCK`, `IS_USED_LOCK`.
/// * **Server metadata leak**: `CURRENT_USER`, `SESSION_USER`, `SYSTEM_USER`,
///   `CONNECTION_ID`.
/// * **XPath / XML injection sinks** (classic out-of-band exfil channel):
///   `EXTRACTVALUE`, `UPDATEXML`.
/// * **TiDB-specific server-bridging**: `TIDB_DECODE_KEY`, `TIDB_DECODE_PLAN`,
///   `TIDB_PARSE_TSO`, `TIDB_IS_DDL_OWNER`, `TIDB_CONFIG`.
///
/// Functions are matched case-insensitively against the *last* identifier in a
/// dotted name (so `mysql.sleep` is matched the same as `sleep`).
const BLOCKED_FUNCTIONS: &[&str] = &[
    "LOAD_FILE",
    "SLEEP",
    "BENCHMARK",
    "GET_LOCK",
    "RELEASE_LOCK",
    "RELEASE_ALL_LOCKS",
    "IS_FREE_LOCK",
    "IS_USED_LOCK",
    "CONNECTION_ID",
    "CURRENT_USER",
    "SESSION_USER",
    "SYSTEM_USER",
    "EXTRACTVALUE",
    "UPDATEXML",
    "TIDB_DECODE_KEY",
    "TIDB_DECODE_PLAN",
    "TIDB_PARSE_TSO",
    "TIDB_IS_DDL_OWNER",
    "TIDB_CONFIG",
];

/// Returns a compiled regex matching `INTO OUTFILE` / `INTO DUMPFILE`.
///
/// The regex requires the next non-space char after `OUTFILE` / `DUMPFILE` to be
/// a quote or path character (`'`, `"`, `/`, `.`, `~`). That keeps it from
/// matching unrelated identifier collisions while still catching every real
/// MySQL `INTO OUTFILE` clause.
fn into_clause_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)\bINTO\s+(?:OUT|DUMP)FILE\s*['"/.~]"#).expect("static regex")
    })
}

/// Classify the safety of `sql`. Returns a structured [`SqlSafetyResult`] so callers
/// can surface actionable error messages to users.
#[allow(dead_code)]
pub fn classify_sql(sql: &str) -> SqlSafetyResult {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    // Layer 1: cheap regex guard for INTO OUTFILE / INTO DUMPFILE. This runs
    // before parsing so even a clever dialect mismatch cannot route around it.
    if into_clause_regex().is_match(sql) {
        return SqlSafetyResult::ForbiddenIntoClause;
    }

    let dialect = GenericDialect {};
    let statements = match Parser::parse_sql(&dialect, sql) {
        Ok(s) => s,
        Err(e) => {
            return SqlSafetyResult::SyntaxError {
                message: e.to_string(),
            }
        }
    };
    if statements.len() != 1 {
        return SqlSafetyResult::MultipleStatements;
    }
    let stmt = &statements[0];

    // Layer 2: only `Statement::Query` is permitted.
    match stmt {
        Statement::Query(_) => { /* fall through to layer 3 */ }
        other => {
            return SqlSafetyResult::ForbiddenOperation {
                statement_type: short_statement_kind(other),
            };
        }
    }

    // Layer 3: AST visitor — reject any dangerous function call anywhere in the tree.
    let mut visitor = ForbiddenFunctionVisitor { found: None };
    let _ = stmt.visit(&mut visitor);
    if let Some(name) = visitor.found {
        return SqlSafetyResult::ForbiddenFunction {
            function_name: name,
        };
    }

    SqlSafetyResult::Safe
}

/// Convenience bool wrapper around [`classify_sql`].
#[allow(dead_code)]
pub fn is_safe_sql(sql: &str) -> bool {
    matches!(classify_sql(sql), SqlSafetyResult::Safe)
}

/// Short, stable identifier for the variant kind of a forbidden statement,
/// used in user-facing error messages. Falls back to a `Debug` short prefix.
fn short_statement_kind(stmt: &Statement) -> String {
    let dbg = format!("{:?}", stmt);
    dbg.split(|c: char| c.is_whitespace() || c == '(' || c == '{')
        .next()
        .unwrap_or("Unknown")
        .to_string()
}

/// Visitor that breaks on the first dangerous function-call it encounters.
struct ForbiddenFunctionVisitor {
    found: Option<String>,
}

impl Visitor for ForbiddenFunctionVisitor {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::Function(f) = expr {
            let name_str = f.name.to_string();
            let last_segment = name_str
                .rsplit('.')
                .next()
                .unwrap_or(name_str.as_str())
                .to_ascii_uppercase();
            // Strip backticks / quotes that sqlparser preserves on quoted identifiers.
            let cleaned = last_segment
                .trim_matches(|c: char| c == '`' || c == '"' || c == '\'' || c == '[' || c == ']');
            if BLOCKED_FUNCTIONS.iter().any(|b| *b == cleaned) {
                self.found = Some(cleaned.to_string());
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_sql, is_safe_sql, SqlSafetyResult};

    #[test]
    fn accepts_simple_select() {
        assert!(is_safe_sql("SELECT 1"));
        assert!(is_safe_sql("SELECT * FROM users"));
    }

    #[test]
    fn accepts_cte() {
        assert!(is_safe_sql("WITH t AS (SELECT 1) SELECT * FROM t"));
    }

    #[test]
    fn rejects_show_and_explain() {
        // B-07: SHOW* and EXPLAIN are forbidden to prevent schema/plan information leakage.
        assert!(!is_safe_sql("SHOW TABLES"));
        assert!(!is_safe_sql("SHOW COLUMNS FROM t"));
        assert!(!is_safe_sql("SHOW CREATE TABLE t"));
        assert!(!is_safe_sql("EXPLAIN SELECT * FROM t"));
    }

    #[test]
    fn accepts_literal_keywords_inside_select() {
        // The old blacklist would wrongly reject this.
        assert!(is_safe_sql("SELECT * FROM t WHERE name = 'drop me'"));
    }

    #[test]
    fn rejects_mutations() {
        assert!(!is_safe_sql("INSERT INTO t VALUES (1)"));
        assert!(!is_safe_sql("UPDATE t SET x=1"));
        assert!(!is_safe_sql("DELETE FROM t"));
        assert!(!is_safe_sql("DROP TABLE t"));
        assert!(!is_safe_sql("TRUNCATE TABLE t"));
        assert!(!is_safe_sql("ALTER TABLE t ADD COLUMN x INT"));
        assert!(!is_safe_sql("CREATE TABLE t (x INT)"));
        assert!(!is_safe_sql("GRANT SELECT ON t TO u"));
    }

    #[test]
    fn rejects_stacked_statements() {
        assert!(!is_safe_sql("SELECT 1; DROP TABLE users"));
        assert!(!is_safe_sql("SELECT 1; SELECT 2"));
    }

    #[test]
    fn rejects_unparseable() {
        assert!(!is_safe_sql(""));
        assert!(!is_safe_sql("this is not sql"));
    }

    #[test]
    fn rejects_dangerous_functions() {
        // Filesystem read
        assert!(!is_safe_sql("SELECT LOAD_FILE('/etc/passwd')"));
        // Amplification / sleeps
        assert!(!is_safe_sql("SELECT SLEEP(10)"));
        assert!(!is_safe_sql("SELECT BENCHMARK(1000000, MD5('a'))"));
        // Server metadata
        assert!(!is_safe_sql("SELECT CURRENT_USER()"));
        assert!(!is_safe_sql("SELECT CONNECTION_ID()"));
        // XPath / XML sinks classically used for out-of-band exfil
        assert!(!is_safe_sql("SELECT EXTRACTVALUE(1, CONCAT(0x7e, USER()))"));
        assert!(!is_safe_sql("SELECT UPDATEXML(1, CONCAT(0x7e, USER()), 1)"));
        // Locks (DoS / coordination side effects)
        assert!(!is_safe_sql("SELECT GET_LOCK('x', 10)"));
        assert!(!is_safe_sql("SELECT RELEASE_LOCK('x')"));
    }

    #[test]
    fn rejects_dangerous_function_nested_anywhere() {
        // Nested in WHERE
        assert!(!is_safe_sql("SELECT id FROM t WHERE SLEEP(1) > 0"));
        // Nested in subquery
        assert!(!is_safe_sql(
            "SELECT * FROM t WHERE id IN (SELECT SLEEP(1))"
        ));
        // Nested in CTE
        assert!(!is_safe_sql(
            "WITH x AS (SELECT BENCHMARK(1, MD5('a'))) SELECT * FROM x"
        ));
        // UNION arm
        assert!(!is_safe_sql(
            "SELECT 1 UNION ALL SELECT LOAD_FILE('/etc/passwd')"
        ));
    }

    #[test]
    fn rejects_into_outfile_dumpfile() {
        // Both are server-side filesystem writes; never allowed.
        assert!(matches!(
            classify_sql("SELECT * FROM users INTO OUTFILE '/tmp/x.csv'"),
            SqlSafetyResult::ForbiddenIntoClause
        ));
        assert!(matches!(
            classify_sql("SELECT * FROM users INTO DUMPFILE '/tmp/x'"),
            SqlSafetyResult::ForbiddenIntoClause
        ));
        // Case-insensitive
        assert!(matches!(
            classify_sql("select * from users into outfile '/tmp/x'"),
            SqlSafetyResult::ForbiddenIntoClause
        ));
    }

    #[test]
    fn allows_literal_text_containing_dangerous_words() {
        // Function names inside string literals are fine.
        assert!(is_safe_sql(
            "SELECT name FROM users WHERE name = 'load_file is unsafe'"
        ));
    }
}
