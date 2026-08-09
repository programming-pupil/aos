//! Optional LLM refinement for deterministic RD repository summaries.

use super::*;

pub(in crate::routes::rd) fn schedule_rd_repository_llm_summary_refinement(
    state: AppState,
    tenant_id: String,
    user_id: String,
    repository_id: String,
    reason: &'static str,
) {
    if !rd_llm_context_summary_enabled() || state.config_registry.is_none() {
        return;
    }
    tokio::spawn(async move {
        match run_rd_repository_llm_summary_refinement(
            &state,
            &tenant_id,
            &user_id,
            &repository_id,
            reason,
        )
        .await
        {
            Ok(updated) => {
                if updated > 0 {
                    schedule_rd_repository_embedding_index(
                        state,
                        tenant_id,
                        user_id,
                        repository_id,
                        "llm_context_summary",
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    repository_id = %repository_id,
                    reason = %reason,
                    error = %error,
                    "RD LLM context summary refinement failed; deterministic summaries remain available"
                );
            }
        }
    });
}

async fn run_rd_repository_llm_summary_refinement(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    repository_id: &str,
    reason: &'static str,
) -> Result<usize, AppError> {
    let rows = sqlx::query(
        "SELECT CAST(id AS TEXT) AS id, scope_type, scope_key, source_hash, summary_text
         FROM rd_repository_context_summaries
         WHERE tenant_id = ? AND repository_id = ?
           AND (llm_summary_text IS NULL OR llm_summary_text = '')
         ORDER BY
           CASE scope_type
             WHEN 'repository' THEN 0
             WHEN 'entrypoints' THEN 1
             WHEN 'directory' THEN 2
             ELSE 9
           END,
           updated_at DESC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(repository_id)
    .bind(i64::try_from(RD_LLM_CONTEXT_SUMMARY_MAX_SCOPES).unwrap_or(i64::MAX))
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(0);
    }

    let mut updated = 0usize;
    for row in rows {
        let id: String = row.get("id");
        let scope_type: String = row.get("scope_type");
        let scope_key: String = row.get("scope_key");
        let source_hash: String = row.get("source_hash");
        let summary_text: String = row.get("summary_text");
        let prompt = build_rd_llm_context_summary_prompt(
            repository_id,
            &scope_type,
            &scope_key,
            &summary_text,
        );
        let completion = match timeout(
            Duration::from_secs(RD_LLM_CONTEXT_SUMMARY_TIMEOUT_SECS),
            run_rd_completion_with_options(
                state,
                tenant_id,
                user_id,
                None,
                prompt,
                "你是 AOS Code Studio 的代码库上下文摘要专家。只基于输入的确定性索引事实做精炼，不编造不存在的文件、接口或依赖。输出 Markdown。".to_string(),
                1800,
                Some(0.1),
            ),
        )
        .await
        {
            Ok(Ok(completion)) => completion,
            Ok(Err(error)) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    repository_id = %repository_id,
                    scope_type = %scope_type,
                    scope_key = %scope_key,
                    reason = %reason,
                    error = %error,
                    "RD LLM context summary candidate failed; trying next scope"
                );
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    repository_id = %repository_id,
                    scope_type = %scope_type,
                    scope_key = %scope_key,
                    timeout_secs = RD_LLM_CONTEXT_SUMMARY_TIMEOUT_SECS,
                    "RD LLM context summary timed out; trying next scope"
                );
                continue;
            }
        };
        let refined = normalize_rd_llm_context_summary(&completion.text);
        if refined.trim().is_empty() {
            continue;
        }
        let result = sqlx::query(
            "UPDATE rd_repository_context_summaries
             SET llm_summary_text = ?, llm_model = ?, llm_updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND tenant_id = ? AND repository_id = ? AND source_hash = ?",
        )
        .bind(&refined)
        .bind(&completion.model)
        .bind(&id)
        .bind(tenant_id)
        .bind(repository_id)
        .bind(&source_hash)
        .execute(&state.db)
        .await?;
        if result.rows_affected() > 0 {
            updated = updated.saturating_add(1);
        }
    }

    if updated > 0 {
        tracing::info!(
            tenant_id = %tenant_id,
            repository_id = %repository_id,
            updated = updated,
            reason = %reason,
            "RD LLM context summary refinement completed"
        );
    }
    Ok(updated)
}

fn build_rd_llm_context_summary_prompt(
    repository_id: &str,
    scope_type: &str,
    scope_key: &str,
    deterministic_summary: &str,
) -> String {
    format!(
        "请把下面的确定性代码库索引摘要精炼为更适合 Coding Agent 使用的上下文摘要。\n\n\
         约束：\n\
         - 只能基于输入内容，不要编造未出现的文件、接口、依赖、业务能力。\n\
         - 目标是帮助后续问答/改代码少走弯路，不是替代读取真实文件。\n\
         - 用中文输出 Markdown，结构清晰，尽量短但信息密度高。\n\
         - 必须包含：项目/模块职责、关键入口文件、可能的数据流/调用链、后续应优先读取的文件、置信度/缺口。\n\
         - 如果输入证据不足，明确写“证据不足”，不要猜。\n\n\
         RepositoryId: {repository_id}\n\
         Scope: {scope_type}/{scope_key}\n\n\
         确定性摘要：\n<<<\n{}\n>>>",
        truncate_text(deterministic_summary, 9_000)
    )
}

fn normalize_rd_llm_context_summary(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    truncate_text(trimmed, 6_000)
}
