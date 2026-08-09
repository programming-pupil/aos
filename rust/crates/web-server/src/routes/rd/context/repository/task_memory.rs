//! Retrieval augmentation from similar historical RD tasks.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn augment_repository_retrieval_with_task_memory(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    prompt: &str,
    terms: &[String],
    context_budget: RdContextBudget,
    task_notes: &mut Vec<String>,
    file_notes: &mut HashMap<String, Vec<String>>,
    file_scores: &mut HashMap<String, f32>,
    observability_sources: &mut BTreeSet<String>,
) -> Result<u64, AppError> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Ok(0);
    }

    let rows = sqlx::query(
        "SELECT id, mode, status, title,
                LEFT(prompt, 3000) AS prompt_text,
                LEFT(COALESCE(plan_md, ''), 2200) AS plan_text,
                LEFT(COALESCE(answer_md, ''), 2600) AS answer_text,
                LEFT(COALESCE(review_md, ''), 1600) AS review_text,
                CAST(created_at AS TEXT) AS created_at
         FROM rd_tasks
         WHERE tenant_id = ? AND user_id = ? AND repository_id = ?
           AND status IN ('completed', 'waiting_approval')
         ORDER BY created_at DESC
         LIMIT 80",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(repository_id)
    .fetch_all(&state.db)
    .await?;

    let prompt_lower = prompt.to_ascii_lowercase();
    let mut search_terms = terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| term.chars().count() >= 2)
        .take(context_budget.retrieval_term_limit.max(6))
        .map(|term| term.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if prompt.chars().count() <= 120 {
        search_terms.insert(prompt_lower.clone());
    }
    if search_terms.is_empty() {
        return Ok(0);
    }

    let mut scored = Vec::new();
    for row in rows {
        let id: String = row.get("id");
        let mode: String = row.get("mode");
        let status: String = row.get("status");
        let title: String = row.get("title");
        let prompt_text: String = row.get("prompt_text");
        let plan_text: String = row.get("plan_text");
        let answer_text: String = row.get("answer_text");
        let review_text: String = row.get("review_text");
        let created_at: String = row.get("created_at");
        let haystack = format!("{title}\n{prompt_text}\n{plan_text}\n{answer_text}\n{review_text}")
            .to_ascii_lowercase();
        let mut score = 0.0_f32;
        if prompt_text.trim().eq_ignore_ascii_case(prompt) {
            score += 14.0;
        } else if prompt_lower.chars().count() >= 6
            && (haystack.contains(&prompt_lower)
                || prompt_lower.contains(&prompt_text.to_ascii_lowercase()))
        {
            score += 8.0;
        }
        for term in &search_terms {
            if term.chars().count() < 2 {
                continue;
            }
            if title.to_ascii_lowercase().contains(term) {
                score += 2.4;
            }
            if prompt_text.to_ascii_lowercase().contains(term) {
                score += 2.2;
            }
            if plan_text.to_ascii_lowercase().contains(term) {
                score += 1.2;
            }
            if answer_text.to_ascii_lowercase().contains(term) {
                score += 1.2;
            }
            if review_text.to_ascii_lowercase().contains(term) {
                score += 0.8;
            }
        }
        if score >= 3.0 {
            scored.push((
                score,
                id,
                mode,
                status,
                title,
                prompt_text,
                plan_text,
                answer_text,
                created_at,
            ));
        }
    }

    if scored.is_empty() {
        return Ok(0);
    }
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.8.cmp(&a.8))
    });

    let mut hits = 0_u64;
    for (score, task_id, mode, status, title, prompt_text, plan_text, answer_text, created_at) in
        scored.into_iter().take(5)
    {
        let touched_files =
            load_rd_task_memory_touched_files(&state.db, &claims.tenant_id, &task_id).await?;
        task_notes.push(format!(
            "- 相似历史任务 taskId={} score={:.1} mode={} status={} createdAt={} touchedFiles={}: {}\n  需求: {}\n  计划/结论: {}",
            task_id,
            score,
            mode,
            status,
            created_at,
            if touched_files.is_empty() { "-".to_string() } else { touched_files.join(", ") },
            truncate_text(&title, 180),
            truncate_text(&prompt_text, 360),
            truncate_text(
                if answer_text.trim().is_empty() { &plan_text } else { &answer_text },
                520,
            )
        ));
        observability_sources.insert("task_memory".to_string());
        hits = hits.saturating_add(1);

        for file_path in touched_files.into_iter().take(8) {
            file_notes
                .entry(file_path.clone())
                .or_default()
                .push(format!(
                    "history-task(taskId={}, score={:.1}): 相似已完成任务曾修改/触达该文件，优先读取真实文件确认当前状态。",
                    task_id, score
                ));
            *file_scores.entry(file_path).or_default() += 3.0 + (score / 10.0);
        }
    }

    Ok(hits)
}

async fn load_rd_task_memory_touched_files(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query(
        "SELECT DISTINCT file_path
         FROM rd_file_changes
         WHERE tenant_id = ? AND task_id = ?
         ORDER BY file_path ASC
         LIMIT 12",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|row| row.get("file_path")).collect())
}
