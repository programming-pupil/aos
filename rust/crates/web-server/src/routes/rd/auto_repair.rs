//! Failed-test auto-repair loop for RD tasks.

use super::*;

async fn build_rd_auto_repair_context(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: &str,
    current_test_run_id: &str,
) -> Result<String, AppError> {
    let mut sections = Vec::new();
    let mut touched_files = BTreeSet::new();

    let change_rows = sqlx::query(
        "SELECT id, file_path, change_type, diff_patch, applied, CAST(created_at AS TEXT) created_at \
         FROM rd_file_changes \
         WHERE task_id = ? AND tenant_id = ? \
         ORDER BY created_at DESC LIMIT 6",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await?;

    if change_rows.is_empty() {
        sections.push("## 最近 Diff\n暂无已记录的候选/修复 Diff。".to_string());
    } else {
        let mut lines = vec!["## 最近 Diff（倒序，最多 6 条）".to_string()];
        for row in &change_rows {
            let id: String = row.get("id");
            let file_path: String = row.get("file_path");
            let change_type: String = row.get("change_type");
            let diff_patch: String = row.get("diff_patch");
            let applied: bool = row.get("applied");
            let created_at: String = row.get("created_at");
            touched_files.insert(file_path.clone());
            for path in infer_files_from_unified_diff(&diff_patch) {
                touched_files.insert(path);
            }
            lines.push(format!(
                "### change_id={id} · type={change_type} · applied={applied} · created_at={created_at}\n文件：`{file_path}`\n```diff\n{}\n```",
                truncate_text(&diff_patch, 8_000)
            ));
        }
        sections.push(truncate_text(&lines.join("\n\n"), 28_000));
    }

    let check_rows = sqlx::query(
        "SELECT status, message, detail_json, CAST(created_at AS TEXT) created_at \
         FROM rd_task_events \
         WHERE task_id = ? AND tenant_id = ? \
           AND stage IN ('diff_check','diff_repair','candidate_worktree') \
         ORDER BY id DESC LIMIT 8",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await?;
    if !check_rows.is_empty() {
        let mut lines = vec!["## 最近 Diff / Candidate 事件（倒序）".to_string()];
        for row in check_rows {
            let status: String = row.get("status");
            let message: Option<String> = row.get("message");
            let created_at: String = row.get("created_at");
            let detail: Option<Value> = row.get("detail_json");
            lines.push(format!(
                "- {created_at} · status={status} · {}\n  detail: {}",
                message.unwrap_or_default(),
                detail
                    .as_ref()
                    .map(compact_json_for_prompt)
                    .unwrap_or_else(|| "{}".to_string())
            ));
        }
        sections.push(truncate_text(&lines.join("\n"), 12_000));
    }

    let test_rows = sqlx::query(
        "SELECT id, command, status, exit_code, stdout_text, stderr_text, duration_ms, CAST(created_at AS TEXT) created_at \
         FROM rd_test_runs \
         WHERE task_id = ? AND tenant_id = ? \
         ORDER BY created_at DESC LIMIT 3",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await?;
    if !test_rows.is_empty() {
        let mut lines = vec![format!(
            "## 最近测试记录（倒序，当前失败 run_id={current_test_run_id}）"
        )];
        for row in test_rows {
            let id: String = row.get("id");
            let command: String = row.get("command");
            let status: String = row.get("status");
            let exit_code: Option<i32> = row.get("exit_code");
            let duration_ms: Option<i64> = row.get("duration_ms");
            let created_at: String = row.get("created_at");
            let stdout_text: Option<String> = row.get("stdout_text");
            let stderr_text: Option<String> = row.get("stderr_text");
            let output_digest = summarize_rd_test_output_for_prompt(
                stdout_text.as_deref().unwrap_or_default(),
                stderr_text.as_deref().unwrap_or_default(),
                7_000,
            );
            lines.push(format!(
                "### run_id={id} · status={status} · exit={exit_code:?} · duration_ms={duration_ms:?} · created_at={created_at}\n命令：`{command}`\n关键输出摘要：\n```text\n{output_digest}\n```",
            ));
        }
        sections.push(truncate_text(&lines.join("\n\n"), 14_000));
    }

    if let Some(snapshot) =
        build_rd_auto_repair_file_snapshots(state, claims, repository_id, &touched_files).await?
    {
        sections.push(snapshot);
    }

    Ok(sections.join("\n\n"))
}

async fn build_rd_auto_repair_file_snapshots(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    touched_files: &BTreeSet<String>,
) -> Result<Option<String>, AppError> {
    if touched_files.is_empty() {
        return Ok(None);
    }
    let root = repository_root(state, claims, repository_id).await?;
    let mut lines = vec!["## 当前真实文件快照（最多 5 个相关文件）".to_string()];
    let mut included = 0usize;
    for file in touched_files {
        if included >= 5 {
            break;
        }
        let trimmed = file
            .trim()
            .trim_start_matches("a/")
            .trim_start_matches("b/");
        if trimmed.is_empty() || trimmed == "/dev/null" || trimmed.ends_with(".diff") {
            continue;
        }
        match safe_join(&root, trimmed) {
            Ok(path) => {
                let meta = match std::fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) => {
                        lines.push(format!("### `{trimmed}`\n无法读取 metadata：{error}"));
                        included += 1;
                        continue;
                    }
                };
                if !meta.is_file() {
                    lines.push(format!("### `{trimmed}`\n当前路径不是普通文件。"));
                    included += 1;
                    continue;
                }
                if meta.len() > MAX_FILE_BYTES {
                    lines.push(format!(
                        "### `{trimmed}`\n文件过大（{} bytes），未内联快照；请 runtime 使用 read_file 分段读取相关位置。",
                        meta.len()
                    ));
                    included += 1;
                    continue;
                }
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        lines.push(format!(
                            "### `{trimmed}`\n```{}\n{}\n```",
                            language_for_path(&path).unwrap_or_else(|| "text".to_string()),
                            truncate_text(&content, 8_000)
                        ));
                    }
                    Err(error) => {
                        lines.push(format!("### `{trimmed}`\n无法读取文本内容：{error}"));
                    }
                }
            }
            Err(error) => {
                lines.push(format!("### `{trimmed}`\n当前文件不可用：{error}"));
            }
        }
        included += 1;
    }
    if included == 0 {
        Ok(None)
    } else {
        Ok(Some(truncate_text(&lines.join("\n\n"), 36_000)))
    }
}

fn compact_json_for_prompt(value: &Value) -> String {
    truncate_text(
        &serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        4_000,
    )
}

pub(super) async fn attempt_rd_auto_repair(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    test_run_id: &str,
) -> Result<(), AppError> {
    let max_attempts = rd_auto_repair_max_attempts();
    let prior_attempts: i64 = sqlx::query(
        "SELECT COUNT(*) c FROM rd_task_events WHERE task_id = ? AND tenant_id = ? AND stage = 'auto_repair' AND status = 'running'",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .fetch_one(&state.db)
    .await?
    .get("c");
    if prior_attempts >= max_attempts {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "auto_repair",
            "completed",
            "已达到自动修复轮次上限，本次跳过",
            json!({"testRunId": test_run_id, "priorAttempts": prior_attempts, "maxAttempts": max_attempts}),
        )
        .await?;
        return Ok(());
    }

    let task = get_task_row(&state.db, &claims.tenant_id, &claims.sub, task_id).await?;
    let Some(repo_id) = task.repository_id.clone() else {
        return Ok(());
    };
    let test_run = get_test_run(&state.db, &claims.tenant_id, test_run_id).await?;
    let repair_context =
        build_rd_auto_repair_context(state, claims, task_id, &repo_id, test_run_id).await?;

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "auto_repair",
        "running",
        "测试失败，正在生成一轮自动修复 Diff",
        json!({"testRunId": test_run_id, "command": test_run.command.clone(), "attemptNo": prior_attempts + 1, "maxAttempts": max_attempts}),
    )
    .await?;

    let use_runtime = rd_runtime_executor_enabled();
    let repo_context = if use_runtime {
        build_repository_runtime_context_hint(
            state,
            claims,
            &repo_id,
            &repair_context,
            RdContextProfile::Modify,
            Some(task_id),
        )
        .await?
    } else {
        build_repository_context_for_prompt(
            state,
            claims,
            &repo_id,
            &repair_context,
            RD_INLINE_CONTEXT_BUDGET_BYTES,
            RdContextProfile::Modify,
            Some(task_id),
        )
        .await?
    };
    let test_output_digest = summarize_rd_test_output_for_prompt(
        test_run.stdout_text.as_deref().unwrap_or_default(),
        test_run.stderr_text.as_deref().unwrap_or_default(),
        10_000,
    );
    let repair_prompt = format!(
        "{}\n\n原始任务：\n{}\n\n最近测试失败：\n命令：{}\n退出码：{:?}\n关键输出摘要：\n{}\n\n当前任务计划：\n{}\n\n已有结果：\n{}\n\n自动修复上下文包：\n{}\n\n仓库上下文：\n{}\n\n修复要求：\n- 必须先基于真实仓库文件、最近候选/修复 Diff、diff_check 错误和测试错误定位问题。\n- 如果 runtime 工具可用，优先读取相关真实文件再生成修复，不要只根据摘要猜测。\n- 完整 stdout/stderr 已保存在任务测试记录中；如摘要不足，先读取相关真实文件和失败位置，不要凭空猜测。\n- 生成的 unifiedDiff 必须相对当前仓库状态可应用；不要重复已经应用成功的变更。\n- 请只输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"unifiedDiff\":string,\"touchedFiles\":array}}。只生成可 git apply 的修复 Diff，不要声称已经应用。",
        rd_system_prompt("modify"),
        task.prompt,
        test_run.command,
        test_run.exit_code,
        test_output_digest,
        task.plan_md.unwrap_or_default(),
        task.answer_md.unwrap_or_default(),
        repair_context,
        repo_context,
    );
    let repair_prompt_for_followup = repair_prompt.clone();

    run_rd_hook(
        state,
        claims,
        HookEventType::BeforeModelCall,
        "rd.auto_repair.model",
        json!({
            "taskId": task_id,
            "testRunId": test_run_id,
            "repositoryId": repo_id.clone(),
        }),
        None,
        false,
        true,
    )
    .await?;

    let completion = if use_runtime {
        match run_rd_runtime_completion(
            state,
            claims,
            task_id,
            "modify",
            Some(&repo_id),
            task.model.as_deref(),
            None,
            RdRuntimeSessionPolicy::ThreadPersistent,
            None,
            repair_prompt.clone(),
        )
        .await
        {
            Ok(completion) => completion,
            Err(error) if rd_runtime_direct_fallback_enabled() => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "auto_repair",
                    "failed",
                    "自动修复 runtime 失败，已按显式配置回退到直连模型",
                    json!({"testRunId": test_run_id, "error": error.to_string()}),
                )
                .await?;
                run_rd_completion(
                    state,
                    &claims.tenant_id,
                    &claims.sub,
                    task.model.as_deref(),
                    repair_prompt,
                )
                .await?
            }
            Err(error) => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "auto_repair",
                    "failed",
                    "自动修复 runtime 失败；默认不回退到直连模型，以避免弱修复污染 Diff",
                    json!({"testRunId": test_run_id, "error": error.to_string()}),
                )
                .await?;
                return Err(error);
            }
        }
    } else {
        run_rd_completion(
            state,
            &claims.tenant_id,
            &claims.sub,
            task.model.as_deref(),
            repair_prompt,
        )
        .await?
    };
    let mut parsed = parse_rd_output(&completion.text, "modify");
    sanitize_rd_parsed_diff_output(&mut parsed);
    sanitize_rd_parsed_diff_output(&mut parsed);
    let Some(diff) = parsed.unified_diff.filter(|v| !v.trim().is_empty()) else {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "auto_repair",
            "completed",
            "自动修复未生成可应用 Diff",
            json!({"testRunId": test_run_id, "model": completion.model}),
        )
        .await?;
        return Ok(());
    };

    let file_path = parsed.touched_files.first().cloned().unwrap_or_else(|| {
        infer_first_file_from_diff(&diff).unwrap_or_else(|| "auto-repair.diff".to_string())
    });
    let change_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO rd_file_changes (id, task_id, tenant_id, repository_id, file_path, change_type, diff_patch) VALUES (?, ?, ?, ?, ?, 'auto_repair', ?)")
        .bind(&change_id)
        .bind(task_id)
        .bind(&claims.tenant_id)
        .bind(&repo_id)
        .bind(file_path)
        .bind(&diff)
        .execute(&state.db)
        .await?;
    let diff_check = validate_generated_diff(
        state,
        claims,
        task_id,
        Some(&repo_id),
        &change_id,
        &diff,
        "auto_repair",
    )
    .await?;
    if diff_check.status == GeneratedDiffCheckStatus::Failed {
        maybe_repair_invalid_generated_diff(
            state,
            claims,
            task_id,
            "modify",
            &repair_prompt_for_followup,
            Some(&repo_id),
            Some(completion.model.as_str()),
            &change_id,
            &diff,
            diff_check.error_message.as_deref(),
            &parsed.plan_md,
            &parsed.answer_md,
            "auto_repair",
        )
        .await?;
    }
    let repair_summary = format!(
        "\n\n## 自动修复建议\n\n{}\n\n{}",
        parsed.plan_md, parsed.answer_md
    );
    sqlx::query("UPDATE rd_tasks SET status = 'waiting_approval', answer_md = (COALESCE(answer_md, '') || ?), model = ? WHERE id = ? AND tenant_id = ?")
        .bind(repair_summary)
        .bind(&completion.model)
        .bind(task_id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "auto_repair",
        "completed",
        "已生成一轮自动修复 Diff，等待人工确认",
        json!({"testRunId": test_run_id, "model": completion.model, "provider": completion.provider}),
    )
    .await?;
    run_rd_hook(
        state,
        claims,
        HookEventType::AfterModelCall,
        "rd.auto_repair.model",
        json!({
            "taskId": task_id,
            "testRunId": test_run_id,
            "repositoryId": repo_id.clone(),
        }),
        Some(json!({"status": "waiting_approval"})),
        false,
        false,
    )
    .await?;
    Ok(())
}
