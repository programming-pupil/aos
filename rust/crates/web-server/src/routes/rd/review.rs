//! Review quality scoring and RD task risk-map generation.

use super::*;

pub(super) use rd_core::review::{analyze_review_quality, RdReviewQuality};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RdTaskRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RdTaskRiskLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }

    fn max(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

#[derive(Debug, Clone)]
struct RdFileRisk {
    path: String,
    risk_level: RdTaskRiskLevel,
    reasons: BTreeSet<String>,
    signals: BTreeSet<String>,
    line_hints: BTreeSet<u64>,
    additions: u64,
    deletions: u64,
}

impl RdFileRisk {
    fn new(path: String) -> Self {
        Self {
            path,
            risk_level: RdTaskRiskLevel::Low,
            reasons: BTreeSet::new(),
            signals: BTreeSet::new(),
            line_hints: BTreeSet::new(),
            additions: 0,
            deletions: 0,
        }
    }

    fn add_signal(&mut self, level: RdTaskRiskLevel, signal: &str, reason: &str) {
        self.risk_level = self.risk_level.max(level);
        self.signals.insert(signal.to_string());
        self.reasons.insert(reason.to_string());
    }

    fn add_line_hint(&mut self, line: Option<u64>) {
        if let Some(line) = line.filter(|value| *value > 0) {
            self.line_hints.insert(line);
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "riskLevel": self.risk_level.as_str(),
            "reasons": self.reasons.iter().take(8).cloned().collect::<Vec<_>>(),
            "signals": self.signals.iter().take(12).cloned().collect::<Vec<_>>(),
            "lineHints": self.line_hints.iter().take(12).copied().collect::<Vec<_>>(),
            "additions": self.additions,
            "deletions": self.deletions,
        })
    }
}

pub(super) async fn record_rd_task_risk_map(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    mode: &str,
    status: &str,
    parsed: &ParsedRdOutput,
    diff: Option<&str>,
    diff_check: Option<&GeneratedDiffCheckOutcome>,
    source_stage: &str,
) -> Result<(), AppError> {
    if !matches!(mode, "modify" | "review") {
        return Ok(());
    }
    let Some(detail) = build_rd_task_risk_map(mode, status, parsed, diff, diff_check, source_stage)
    else {
        return Ok(());
    };
    record_event(
        db,
        tenant_id,
        task_id,
        "risk_map",
        "completed",
        "已生成 Review/Modify 风险地图",
        detail,
    )
    .await
}

fn build_rd_task_risk_map(
    mode: &str,
    status: &str,
    parsed: &ParsedRdOutput,
    diff: Option<&str>,
    diff_check: Option<&GeneratedDiffCheckOutcome>,
    source_stage: &str,
) -> Option<Value> {
    let diff = diff
        .or(parsed.unified_diff.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let review_text = parsed
        .review_md
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| (!parsed.answer_md.trim().is_empty()).then_some(parsed.answer_md.as_str()))
        .unwrap_or_default();

    let mut files = BTreeMap::<String, RdFileRisk>::new();
    for path in infer_files_from_unified_diff(diff)
        .into_iter()
        .chain(parsed.touched_files.iter().cloned())
        .chain(rd_extract_file_mentions_from_text(review_text))
    {
        if let Some(file) = rd_file_risk_mut(&mut files, &path) {
            file.add_signal(
                RdTaskRiskLevel::Low,
                "touched_file",
                "已纳入本次变更/审查范围",
            );
        }
    }

    rd_apply_diff_risk(diff, &mut files);
    for file in files.values_mut() {
        rd_apply_path_risk(file);
    }
    rd_apply_review_text_risk(review_text, &mut files);
    rd_apply_diff_check_risk(diff_check, &mut files);
    rd_apply_missing_test_risk(mode, &mut files);

    let mut risk_level = RdTaskRiskLevel::Low;
    let mut critical_files = 0u64;
    let mut high_files = 0u64;
    let mut medium_files = 0u64;
    let mut low_files = 0u64;
    let mut signal_counts = BTreeMap::<String, u64>::new();
    let mut file_items = Vec::new();

    for file in files.values() {
        risk_level = risk_level.max(file.risk_level);
        match file.risk_level {
            RdTaskRiskLevel::Critical => critical_files += 1,
            RdTaskRiskLevel::High => high_files += 1,
            RdTaskRiskLevel::Medium => medium_files += 1,
            RdTaskRiskLevel::Low => low_files += 1,
        }
        for signal in &file.signals {
            *signal_counts.entry(signal.clone()).or_default() += 1;
        }
        file_items.push(file.to_json());
    }

    if file_items.is_empty() && review_text.trim().is_empty() && diff.is_empty() {
        return None;
    }

    Some(json!({
        "version": 1,
        "mode": mode,
        "taskStatus": status,
        "sourceStage": source_stage,
        "riskLevel": risk_level.as_str(),
        "files": file_items,
        "summary": {
            "fileCount": files.len(),
            "criticalFiles": critical_files,
            "highFiles": high_files,
            "mediumFiles": medium_files,
            "lowFiles": low_files,
            "signals": signal_counts,
            "diffCheckStatus": diff_check.map(|outcome| outcome.status.as_str()),
            "diffCheckError": diff_check
                .and_then(|outcome| outcome.error_message.as_deref())
                .map(|error| truncate_text(error, 1_000)),
            "recommendation": rd_risk_map_recommendation(risk_level),
        }
    }))
}

fn rd_file_risk_mut<'a>(
    files: &'a mut BTreeMap<String, RdFileRisk>,
    path: &str,
) -> Option<&'a mut RdFileRisk> {
    let path = rd_normalize_risk_path(path);
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    Some(
        files
            .entry(path.clone())
            .or_insert_with(|| RdFileRisk::new(path)),
    )
}

fn rd_normalize_risk_path(path: &str) -> String {
    rd_normalize_repo_relative_path(
        path.trim()
            .trim_start_matches("a/")
            .trim_start_matches("b/")
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\''),
    )
}

fn rd_apply_diff_risk(diff: &str, files: &mut BTreeMap<String, RdFileRisk>) {
    let mut current_path: Option<String> = None;
    let mut current_new_line: Option<u64> = None;

    for line in diff.lines() {
        if let Some(path) = rd_diff_git_new_path(line) {
            current_path = Some(path.clone());
            let _ = rd_file_risk_mut(files, &path);
            current_new_line = None;
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ b/") {
            let path = rd_normalize_risk_path(path);
            if !path.is_empty() {
                current_path = Some(path.clone());
                let _ = rd_file_risk_mut(files, &path);
            }
            continue;
        }
        if line.starts_with("@@") {
            current_new_line = rd_parse_unified_diff_new_line(line);
            if let (Some(path), Some(line_hint)) = (current_path.as_deref(), current_new_line) {
                if let Some(file) = rd_file_risk_mut(files, path) {
                    file.add_line_hint(Some(line_hint));
                }
            }
            continue;
        }

        let Some(path) = current_path.clone() else {
            continue;
        };
        let Some(file) = rd_file_risk_mut(files, &path) else {
            continue;
        };
        if line.starts_with('+') && !line.starts_with("+++") {
            file.additions = file.additions.saturating_add(1);
            file.add_line_hint(current_new_line);
            rd_apply_content_risk(file, line.trim_start_matches('+'), true);
            if let Some(value) = current_new_line.as_mut() {
                *value = value.saturating_add(1);
            }
        } else if line.starts_with('-') && !line.starts_with("---") {
            file.deletions = file.deletions.saturating_add(1);
            file.add_signal(
                RdTaskRiskLevel::Low,
                "deletion",
                "Diff 包含删除行，应用前需要确认行为差异",
            );
            rd_apply_content_risk(file, line.trim_start_matches('-'), false);
        } else if !line.starts_with('\\') {
            if let Some(value) = current_new_line.as_mut() {
                *value = value.saturating_add(1);
            }
        }
    }

    for file in files.values_mut() {
        if file.deletions >= 80 {
            file.add_signal(
                RdTaskRiskLevel::High,
                "large_deletion",
                "删除行数较多，建议重点检查回归风险",
            );
        } else if file.deletions >= 25 {
            file.add_signal(
                RdTaskRiskLevel::Medium,
                "large_deletion",
                "删除行数偏多，建议确认是否覆盖测试",
            );
        }
    }
}

fn rd_diff_git_new_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    let parts = rest.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let path = rd_normalize_risk_path(parts[1]);
    (!path.is_empty()).then_some(path)
}

fn rd_parse_unified_diff_new_line(header: &str) -> Option<u64> {
    let plus_index = header.find('+')?;
    let tail = &header[plus_index + 1..];
    let digits = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits.parse::<u64>().ok()
}

fn rd_apply_path_risk(file: &mut RdFileRisk) {
    let lower = file.path.to_ascii_lowercase();
    if rd_is_security_sensitive_path(&lower) {
        file.add_signal(
            RdTaskRiskLevel::High,
            "security_sensitive_path",
            "涉及认证、权限、租户、密钥或安全相关路径",
        );
    }
    if rd_is_database_path(&lower) {
        file.add_signal(
            RdTaskRiskLevel::High,
            "database_or_migration",
            "涉及数据库、SQL 或迁移文件，建议重点确认兼容性与回滚",
        );
    }
    if rd_is_build_or_deploy_path(&lower) {
        file.add_signal(
            RdTaskRiskLevel::Medium,
            "build_or_deploy",
            "涉及构建、部署或 CI 配置，建议确认流水线影响",
        );
    }
}

fn rd_apply_content_risk(file: &mut RdFileRisk, line: &str, is_added: bool) {
    let lower = line.to_ascii_lowercase();
    if is_added
        && contains_any(
            &lower,
            &[
                "password",
                "passwd",
                "secret",
                "api_key",
                "apikey",
                "access_token",
                "client_secret",
                "private_key",
            ],
        )
        && !contains_any(
            &lower,
            &[
                "example",
                "placeholder",
                "todo",
                "redacted",
                "your_",
                "<",
                "${",
            ],
        )
    {
        file.add_signal(
            RdTaskRiskLevel::Critical,
            "possible_secret",
            "新增内容疑似包含密钥、令牌或密码",
        );
    }
    if contains_any(
        &lower,
        &[
            "eval(",
            "dangerouslysetinnerhtml",
            "innerhtml",
            "document.write",
        ],
    ) {
        file.add_signal(
            RdTaskRiskLevel::High,
            "injection_surface",
            "内容涉及动态执行或 HTML 注入面",
        );
    }
    if contains_any(
        &lower,
        &[
            "permission",
            "authorization",
            "authentication",
            "tenant",
            "jwt",
            "oauth",
            "rbac",
        ],
    ) {
        file.add_signal(
            RdTaskRiskLevel::Medium,
            "auth_logic",
            "内容涉及认证、授权或租户隔离逻辑",
        );
    }
    if contains_any(&lower, &["select ", "insert ", "update ", "delete from "])
        && contains_any(&lower, &["format!", "+", "concat", "${"])
    {
        file.add_signal(
            RdTaskRiskLevel::High,
            "dynamic_sql",
            "内容可能涉及动态 SQL 拼接，建议确认注入风险",
        );
    }
    if contains_any(&lower, &["unwrap(", "expect(", "panic!", "unsafe "]) {
        file.add_signal(
            RdTaskRiskLevel::Medium,
            "runtime_failure_surface",
            "内容涉及 panic/unsafe/强制解包等运行时风险点",
        );
    }
}

fn rd_apply_review_text_risk(review_text: &str, files: &mut BTreeMap<String, RdFileRisk>) {
    let lower = review_text.to_ascii_lowercase();
    if lower.trim().is_empty() {
        return;
    }
    let mut matched_paths = files
        .keys()
        .filter(|path| {
            let path_lower = path.to_ascii_lowercase();
            lower.contains(&path_lower)
                || Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| lower.contains(&name.to_ascii_lowercase()))
        })
        .cloned()
        .collect::<Vec<_>>();
    if matched_paths.is_empty() && files.len() == 1 {
        matched_paths.extend(files.keys().cloned());
    }
    if matched_paths.is_empty() {
        if let Some(file) = rd_file_risk_mut(files, "__overall__") {
            matched_paths.push(file.path.clone());
        }
    }

    let mut signals = Vec::<(RdTaskRiskLevel, &'static str, &'static str)>::new();
    if contains_any(
        &lower,
        &[
            "critical", "blocker", "must fix", "严重", "致命", "阻塞", "高危",
        ],
    ) {
        signals.push((
            RdTaskRiskLevel::Critical,
            "review_critical_finding",
            "Review 文本包含严重/阻塞级发现",
        ));
    } else if contains_any(&lower, &["high", "高风险", "高优先级"]) {
        signals.push((
            RdTaskRiskLevel::High,
            "review_high_finding",
            "Review 文本包含高风险发现",
        ));
    }
    if contains_any(
        &lower,
        &["missing test", "缺少测试", "未测试", "没有测试", "no test"],
    ) {
        signals.push((
            RdTaskRiskLevel::Medium,
            "missing_tests",
            "Review 文本提示测试覆盖不足",
        ));
    }
    if contains_any(&lower, &["regression", "回归"]) {
        signals.push((
            RdTaskRiskLevel::High,
            "regression_risk",
            "Review 文本提示潜在回归风险",
        ));
    }
    if contains_any(&lower, &["security", "vulnerability", "安全", "漏洞"]) {
        signals.push((
            RdTaskRiskLevel::High,
            "security_review_signal",
            "Review 文本提示安全相关风险",
        ));
    }

    for path in matched_paths {
        if let Some(file) = files.get_mut(&path) {
            file.add_signal(
                RdTaskRiskLevel::Low,
                "review_referenced",
                "Review 文本引用了该文件或整体任务",
            );
            for (level, signal, reason) in &signals {
                file.add_signal(*level, signal, reason);
            }
        }
    }
}

fn rd_apply_diff_check_risk(
    diff_check: Option<&GeneratedDiffCheckOutcome>,
    files: &mut BTreeMap<String, RdFileRisk>,
) {
    let Some(diff_check) = diff_check else {
        return;
    };
    let (level, signal, reason) = match diff_check.status {
        GeneratedDiffCheckStatus::Passed => (
            RdTaskRiskLevel::Low,
            "diff_check_passed",
            "Diff 可应用性校验已通过",
        ),
        GeneratedDiffCheckStatus::Skipped => (
            RdTaskRiskLevel::Medium,
            "diff_check_skipped",
            "Diff 可应用性校验被跳过，应用前建议人工确认",
        ),
        GeneratedDiffCheckStatus::Failed => (
            RdTaskRiskLevel::High,
            "diff_check_failed",
            "Diff 可应用性校验失败，直接应用可能失败",
        ),
    };
    for file in files.values_mut() {
        file.add_signal(level, signal, reason);
    }
}

fn rd_apply_missing_test_risk(mode: &str, files: &mut BTreeMap<String, RdFileRisk>) {
    if mode != "modify" || files.is_empty() {
        return;
    }
    let has_test_change = files.keys().any(|path| rd_is_test_path(path));
    let has_code_change = files
        .keys()
        .any(|path| rd_is_code_review_relevant_path(path) && !rd_is_test_path(path));
    if has_code_change && !has_test_change {
        for file in files.values_mut() {
            if rd_is_code_review_relevant_path(&file.path) && !rd_is_test_path(&file.path) {
                file.add_signal(
                    RdTaskRiskLevel::Medium,
                    "missing_tests",
                    "代码变更未看到测试文件同步变更，请确认已有测试是否覆盖",
                );
            }
        }
    }
}

fn rd_extract_file_mentions_from_text(text: &str) -> Vec<String> {
    let mut files = BTreeSet::new();
    for token in text.split_whitespace() {
        let value = token.trim_matches(|ch: char| {
            ch.is_ascii_punctuation()
                || matches!(
                    ch,
                    '，' | '。' | '；' | '：' | '（' | '）' | '【' | '】' | '、'
                )
        });
        let lower = value.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            continue;
        }
        if value.contains('/')
            || [
                ".rs", ".ts", ".tsx", ".js", ".jsx", ".java", ".kt", ".go", ".py", ".sql", ".vue",
                ".svelte", ".md", ".yml", ".yaml", ".toml", ".json", ".xml", ".gradle",
            ]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        {
            let path = rd_normalize_risk_path(value);
            if !path.is_empty() && path.len() <= 240 {
                files.insert(path);
            }
        }
        if files.len() >= 80 {
            break;
        }
    }
    files.into_iter().collect()
}

fn rd_is_security_sensitive_path(lower_path: &str) -> bool {
    contains_any(
        lower_path,
        &[
            "auth",
            "login",
            "session",
            "permission",
            "rbac",
            "role",
            "tenant",
            "jwt",
            "oauth",
            "security",
            "secret",
            ".env",
            "token",
            "password",
        ],
    )
}

fn rd_is_database_path(lower_path: &str) -> bool {
    contains_any(
        lower_path,
        &[
            "migration",
            "migrations",
            "schema.sql",
            ".sql",
            "database",
            "datasource",
            "repository",
            "dao",
        ],
    )
}

fn rd_is_build_or_deploy_path(lower_path: &str) -> bool {
    lower_path == "dockerfile"
        || contains_any(
            lower_path,
            &[
                "docker-compose",
                ".github/workflows",
                ".gitlab-ci",
                "jenkinsfile",
                "k8s/",
                "deploy",
                "package.json",
                "cargo.toml",
                "pom.xml",
                "build.gradle",
                "vite.config",
                "webpack.config",
                "tsconfig",
            ],
        )
}

fn rd_is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "/test/",
            "/tests/",
            "__tests__",
            ".test.",
            ".spec.",
            "_test.",
            "test_",
            "tests.rs",
        ],
    )
}

fn rd_is_code_review_relevant_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower == "__overall__" || rd_is_doc_or_asset_path(&lower) {
        return false;
    }
    true
}

fn rd_is_doc_or_asset_path(lower_path: &str) -> bool {
    [
        ".md", ".mdx", ".txt", ".rst", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".ico",
        ".lock",
    ]
    .iter()
    .any(|suffix| lower_path.ends_with(suffix))
}

fn rd_risk_map_recommendation(level: RdTaskRiskLevel) -> &'static str {
    match level {
        RdTaskRiskLevel::Critical => "存在阻塞级风险，建议先修复或重新生成 Diff，再考虑应用。",
        RdTaskRiskLevel::High => "存在高风险信号，建议重点审查相关文件并运行必要测试。",
        RdTaskRiskLevel::Medium => "存在中等风险信号，建议确认测试覆盖和变更范围。",
        RdTaskRiskLevel::Low => "未发现明显高风险信号，仍建议按团队规范完成 Review。",
    }
}

pub(super) async fn record_review_quality_metrics(
    db: &SqlitePool,
    tenant_id: &str,
    repository_id: &str,
    task_id: &str,
    quality: &RdReviewQuality,
) -> Result<(), AppError> {
    let detail = json!({
        "findingsCount": quality.findings_count,
        "fileRefCount": quality.file_ref_count,
        "lineRefCount": quality.line_ref_count,
    });
    record_quality_metric(
        db,
        tenant_id,
        Some(repository_id),
        Some(task_id),
        "review_agent_run",
        1.0,
        detail.clone(),
    )
    .await?;
    record_quality_metric(
        db,
        tenant_id,
        Some(repository_id),
        Some(task_id),
        "review_findings_count",
        quality.findings_count as f64,
        detail.clone(),
    )
    .await?;
    record_quality_metric(
        db,
        tenant_id,
        Some(repository_id),
        Some(task_id),
        "review_file_ref_count",
        quality.file_ref_count as f64,
        detail.clone(),
    )
    .await?;
    record_quality_metric(
        db,
        tenant_id,
        Some(repository_id),
        Some(task_id),
        "review_line_ref_count",
        quality.line_ref_count as f64,
        detail,
    )
    .await
}
