use super::*;

pub(super) fn wrap_pm_research_prompt(source: &str, message: String) -> String {
    if !source.eq_ignore_ascii_case("pm") {
        return message;
    }
    let language_contract = build_pm_answer_language_contract(&message);
    format!(
        "{PM_POLICY_BEGIN}\n{PM_RESEARCH_POLICY}\n\n{language_contract}\n{PM_POLICY_END}\n\n{message}"
    )
}

fn strip_pm_research_prompt(source: &str, message: &str) -> String {
    if !source.eq_ignore_ascii_case("pm") {
        return message.to_string();
    }
    if !message.contains(PM_POLICY_BEGIN) {
        return message.to_string();
    }
    if let Some(begin) = message.find(PM_POLICY_BEGIN) {
        if let Some(end_rel) = message[begin..].find(PM_POLICY_END) {
            let after = begin + end_rel + PM_POLICY_END.len();
            return message[after..].trim_start().to_string();
        }
    }
    message.to_string()
}

fn is_pm_orch_internal_message(message: &str) -> bool {
    message.contains(PM_ORCH_INTERNAL_BEGIN) && message.contains(PM_ORCH_INTERNAL_END)
}

fn strip_pm_orch_internal_message(message: &str) -> String {
    if !is_pm_orch_internal_message(message) {
        return message.to_string();
    }
    if let Some(begin) = message.find(PM_ORCH_INTERNAL_BEGIN) {
        if let Some(end_rel) = message[begin..].find(PM_ORCH_INTERNAL_END) {
            let after = begin + end_rel + PM_ORCH_INTERNAL_END.len();
            return message[after..].trim_start().to_string();
        }
    }
    String::new()
}

fn strip_pm_retrieval_hint(message: &str) -> String {
    let marker = "[retrieval optimization hint:";
    let lower = message.to_ascii_lowercase();
    if let Some(idx) = lower.find(marker) {
        return message[..idx].trim_end().to_string();
    }
    message.to_string()
}

pub(super) fn sanitize_pm_user_message(source: &str, message: String) -> String {
    if !source.eq_ignore_ascii_case("pm") {
        return message;
    }
    strip_pm_retrieval_hint(&message).trim().to_string()
}

fn load_durable_visible_history(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> Vec<MessageDto> {
    crate::routes::chat::load_session_messages(&state.data_dir, tenant_id, user_id, session_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
        .filter_map(|message| {
            let content = message.content.as_str()?.trim().to_string();
            (!content.is_empty()).then_some(MessageDto {
                role: message.role,
                content,
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            })
        })
        .collect()
}

async fn load_super_assistant_turn_message_metadata(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    answer_texts: &[String],
) -> Result<Vec<SuperAssistantTurnMessageMetadataDto>, AppError> {
    if answer_texts.is_empty() {
        return Ok(Vec::new());
    }

    let mut turn_query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT t.turn_id, COALESCE(t.model, '') AS model, t.final_text, t.route_capability,
                s.external_task_id AS adversarial_run_id,
                r.judge_model, r.winner_model, r.winner_reason,
                (SELECT sa.external_task_id
                   FROM super_assistant_subtasks sa
                  WHERE sa.tenant_id = t.tenant_id
                    AND sa.user_id = t.user_id
                    AND sa.parent_turn_id = t.turn_id
                    AND sa.engine = 'data_attribution'
                  ORDER BY sa.created_at ASC, sa.id ASC
                  LIMIT 1) AS attribution_task_id,
                CAST(t.completed_at AS TEXT) AS completed_at
         FROM super_assistant_turns t
         LEFT JOIN super_assistant_subtasks s
           ON s.id = (SELECT s2.id
                        FROM super_assistant_subtasks s2
                       WHERE s2.tenant_id = t.tenant_id
                         AND s2.user_id = t.user_id
                         AND s2.parent_turn_id = t.turn_id
                         AND s2.engine = 'super_adversarial'
                       ORDER BY s2.created_at ASC, s2.id ASC
                       LIMIT 1)
         LEFT JOIN chat_adversarial_runs r
           ON r.id = s.external_task_id
          AND r.tenant_id = t.tenant_id
          AND r.user_id = t.user_id
         WHERE t.tenant_id = ",
    );
    turn_query
        .push_bind(tenant_id)
        .push(" AND t.user_id = ")
        .push_bind(user_id)
        .push(" AND t.session_id = ")
        .push_bind(session_id)
        .push(
            " AND t.status = 'completed'
              AND t.final_text IS NOT NULL
              AND LENGTH(TRIM(t.final_text)) > 0
              AND t.final_text IN (",
        );
    {
        let mut separated = turn_query.separated(", ");
        for answer in answer_texts {
            separated.push_bind(answer);
        }
    }
    // Repeated compact answers (for example, "OK") can occur in many turns.
    // Keep the query bounded while allowing several matches per visible page
    // answer. Results are matched to messages by exact final text in the UI.
    let result_limit = answer_texts.len().saturating_mul(4).clamp(40, 120);
    turn_query
        .push(") ORDER BY t.started_at DESC, t.id DESC LIMIT ")
        .push_bind(i64::try_from(result_limit).unwrap_or(120));
    let rows = turn_query.build().fetch_all(db).await?;

    let turn_ids = rows
        .iter()
        .map(|row| row.try_get::<String, _>("turn_id"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if turn_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Runtime JSONL can be replaced by the durable visible-text archive during
    // history recovery. NL2SQL audit data therefore comes from the durable
    // parent/subtask ledger instead of depending on runtime ToolUse blocks.
    let mut audit_query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT parent_turn_id, tool_call_id, status, \
         CAST(input_json AS TEXT) AS input_json, \
         CAST(result_json AS TEXT) AS result_json, error_message \
         FROM super_assistant_subtasks \
         WHERE tenant_id = ",
    );
    audit_query
        .push_bind(tenant_id)
        .push(" AND user_id = ")
        .push_bind(user_id)
        .push(" AND engine = 'nl2sql' AND parent_turn_id IN (");
    {
        let mut separated = audit_query.separated(", ");
        for turn_id in &turn_ids {
            separated.push_bind(turn_id);
        }
    }
    audit_query.push(") ORDER BY created_at ASC, id ASC");
    let audit_rows = audit_query.build().fetch_all(db).await?;
    let mut nl2sql_audits_by_turn =
        std::collections::HashMap::<String, Vec<SuperAssistantNl2sqlAuditDto>>::new();
    for row in audit_rows {
        let parse_json = |raw: Option<String>| {
            raw.and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        };
        nl2sql_audits_by_turn
            .entry(row.try_get::<String, _>("parent_turn_id")?)
            .or_default()
            .push(SuperAssistantNl2sqlAuditDto {
                tool_call_id: row.try_get::<String, _>("tool_call_id")?,
                status: row.try_get::<String, _>("status")?,
                input: parse_json(row.try_get::<Option<String>, _>("input_json")?),
                result: parse_json(row.try_get::<Option<String>, _>("result_json")?),
                error_message: row.try_get::<Option<String>, _>("error_message")?,
            });
    }

    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::with_capacity(rows.len());
    for row in rows.into_iter().rev() {
        let turn_id = row.try_get::<String, _>("turn_id")?;
        if !seen.insert(turn_id.clone()) {
            continue;
        }
        let nl2sql_audits = nl2sql_audits_by_turn.remove(&turn_id).unwrap_or_default();
        items.push(SuperAssistantTurnMessageMetadataDto {
            turn_id,
            model: row.try_get::<String, _>("model")?,
            final_text: row.try_get::<String, _>("final_text")?,
            route_capability: row.try_get::<Option<String>, _>("route_capability")?,
            adversarial_run_id: row.try_get::<Option<String>, _>("adversarial_run_id")?,
            judge_model: row.try_get::<Option<String>, _>("judge_model")?,
            winner_model: row.try_get::<Option<String>, _>("winner_model")?,
            winner_reason: row.try_get::<Option<String>, _>("winner_reason")?,
            attribution_task_id: row.try_get::<Option<String>, _>("attribution_task_id")?,
            nl2sql_audits,
            completed_at: row.try_get::<Option<String>, _>("completed_at")?,
        });
    }
    Ok(items)
}

fn history_page_assistant_answer_texts(messages: &[MessageDto]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| durable_history_plain_content(&message.content))
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty() && seen.insert(content.clone()))
        .take(40)
        .collect()
}

fn durable_history_plain_content(content: &str) -> String {
    let Ok(serde_json::Value::Array(blocks)) = serde_json::from_str::<serde_json::Value>(content)
    else {
        return content.trim().to_string();
    };
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        content.trim().to_string()
    } else {
        text
    }
}

fn visible_history_user_content(content: &str) -> String {
    let plain = durable_history_plain_content(content);
    let cutoff = [
        "\n\n[图片理解上下文]",
        "\n\n[附件文档上下文]",
        "\n\n[本轮附件上下文]",
    ]
    .iter()
    .filter_map(|marker| plain.find(marker))
    .min()
    .unwrap_or(plain.len());
    plain[..cutoff].trim().to_string()
}

fn history_user_identity_content(content: &str) -> String {
    let visible = visible_history_user_content(content);
    let Some(command) = visible.strip_prefix('/') else {
        return visible;
    };
    let Some((_, prompt)) = command.split_once(char::is_whitespace) else {
        return visible;
    };
    let prompt = prompt.trim();
    if prompt.is_empty() {
        visible
    } else {
        prompt.to_string()
    }
}

fn is_pm_orch_question_stop_line(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    [
        "subtask:",
        "subtask evidence context:",
        "map-reduce context:",
        "baselinecontext:",
        "query variants:",
        "query variant:",
        "planned query variants:",
        "planned query variant:",
        "planned source routes:",
        "cross-session evidence hints:",
        "return only:",
        "previous answer",
        "return a full research answer",
        "please re-run retrieval",
        "please retrieve evidence",
        "execution rules:",
        "retry strategy:",
        "strategy hint:",
        "repair requirements:",
        "output required:",
        "route focus:",
        "evidence context:",
        "search layer used:",
        "return the final answer now.",
        "general_grounded_answer_mode:",
        "direct_answer_mode:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn extract_pm_orch_visible_user_message(message: &str) -> Option<String> {
    let stripped = strip_pm_orch_internal_message(message);
    let body = stripped.trim();
    if body.is_empty() {
        return None;
    }

    let labels = ["User question:", "User message:", "Original question:"];
    let lines: Vec<&str> = body.lines().collect();

    for (idx, raw) in lines.iter().enumerate() {
        let line = raw.trim_start();
        let Some(label) = labels.iter().find(|label| line.starts_with(**label)) else {
            continue;
        };

        let mut parts: Vec<String> = Vec::new();
        let first = line[label.len()..].trim();
        if !first.is_empty() {
            parts.push(first.to_string());
        }

        for follow in lines.iter().skip(idx + 1) {
            let candidate = follow.trim();
            if candidate.is_empty() {
                continue;
            }
            if is_pm_orch_question_stop_line(candidate) {
                break;
            }
            parts.push(candidate.to_string());
        }

        let question = strip_pm_retrieval_hint(&parts.join("\n"))
            .trim()
            .to_string();
        if !question.is_empty() {
            return Some(question);
        }
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PmAnswerLanguageHint {
    Chinese,
    UserDominant,
}

fn build_pm_answer_language_contract(message: &str) -> String {
    match detect_pm_answer_language(message) {
        PmAnswerLanguageHint::Chinese => {
            "Answer language contract:\n\
             - The latest user-visible question is detected as Chinese/CJK.\n\
             - Answer in Chinese for all user-facing narrative, conclusions, caveats, and next actions.\n\
             - Ignore UI/session locale and internal prompt language when choosing the answer language.\n\
             - If the user explicitly asks for a different answer language, obey that explicit request."
                .to_string()
        }
        PmAnswerLanguageHint::UserDominant => {
            "Answer language contract:\n\
             - Choose the answer language from the latest user-visible question, not from UI/session locale or internal prompt language.\n\
             - Preserve the user's dominant language for all user-facing narrative, conclusions, caveats, and next actions.\n\
             - Do not default to English just because this system prompt is written in English.\n\
             - If the user explicitly asks for a different answer language, obey that explicit request."
                .to_string()
        }
    }
}

fn detect_pm_answer_language(message: &str) -> PmAnswerLanguageHint {
    let visible_message = extract_pm_orch_visible_user_message(message).unwrap_or_else(|| {
        strip_pm_retrieval_hint(&strip_pm_orch_internal_message(message))
            .trim()
            .to_string()
    });
    if visible_message.chars().any(is_cjk_char) {
        PmAnswerLanguageHint::Chinese
    } else {
        PmAnswerLanguageHint::UserDominant
    }
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0x2CEB0..=0x2EBEF
            | 0x30000..=0x3134F
    )
}

#[cfg(test)]
mod pm_history_tests {
    use super::*;

    fn history_message(role: &str, content: impl Into<String>) -> MessageDto {
        MessageDto {
            role: role.to_string(),
            content: content.into(),
            tool_calls: None,
            tool_result: None,
            usage: None,
            thinking: None,
            pm_task_id: None,
            pm_task_status: None,
        }
    }

    #[test]
    fn history_metadata_lookup_uses_only_exact_visible_page_answers() {
        let messages = vec![
            history_message("user", "question one"),
            history_message("assistant", "answer one"),
            history_message("tool", "internal output"),
            history_message("assistant", "answer one"),
            history_message(
                "assistant",
                serde_json::json!([{"type": "text", "text": "answer two"}]).to_string(),
            ),
        ];

        assert_eq!(
            history_page_assistant_answer_texts(&messages),
            vec!["answer one".to_string(), "answer two".to_string()]
        );
    }

    #[test]
    fn durable_attachment_content_deduplicates_by_visible_text() {
        let content = serde_json::json!([
            {"type": "text", "text": "这个实验呢？"},
            {"type": "image", "data": "/api/v1/uploads/user/chart.png"}
        ])
        .to_string();
        assert_eq!(durable_history_plain_content(&content), "这个实验呢？");
    }

    #[test]
    fn runtime_image_summary_deduplicates_against_visible_attachment_message() {
        let runtime =
            "这个实验呢？\n\n[图片理解上下文]\n图片数量: 1\n内部视觉摘要\n[/图片理解上下文]";
        assert_eq!(visible_history_user_content(runtime), "这个实验呢？");
    }

    #[test]
    fn super_assistant_regression_slash_display_deduplicates_runtime_text() {
        let display = "/超级对抗 比较方案 A 和方案 B";
        let execution = "比较方案 A 和方案 B";
        assert_eq!(
            history_user_identity_content(display),
            history_user_identity_content(execution)
        );
        assert_eq!(visible_history_user_content(display), display);
    }

    #[test]
    fn extract_pm_orch_visible_user_message_supports_retrieve_prompt() {
        let message = format!(
            "{PM_ORCH_INTERNAL_BEGIN}\ninternal\n{PM_ORCH_INTERNAL_END}\n\nUser question: 抓 Linux 社区难点\nQuery variants: 难点 | 痛点"
        );
        let got = extract_pm_orch_visible_user_message(&message);
        assert_eq!(got.as_deref(), Some("抓 Linux 社区难点"));
    }

    #[test]
    fn extract_pm_orch_visible_user_message_stops_on_planned_query_variants() {
        let message = format!(
            "{PM_ORCH_INTERNAL_BEGIN}\ninternal\n{PM_ORCH_INTERNAL_END}\n\nUser question: 出海印尼做网赚休闲单机游戏前景如何？\nPlanned query variants: 变体A | 变体B\nPlanned source routes: fallback.web.search"
        );
        let got = extract_pm_orch_visible_user_message(&message);
        assert_eq!(got.as_deref(), Some("出海印尼做网赚休闲单机游戏前景如何？"));
    }

    #[test]
    fn extract_pm_orch_visible_user_message_supports_multiline_questions() {
        let message = format!(
            "{PM_ORCH_INTERNAL_BEGIN}\ninternal\n{PM_ORCH_INTERNAL_END}\n\nUser message: 请帮我研究这个市场\n需要重点看留存和付费\nPrevious answer (insufficient):\nfoo"
        );
        let got = extract_pm_orch_visible_user_message(&message);
        assert_eq!(
            got.as_deref(),
            Some("请帮我研究这个市场\n需要重点看留存和付费")
        );
    }

    #[test]
    fn extract_pm_orch_visible_user_message_stops_before_map_reduce_context() {
        let message = format!(
            "{PM_ORCH_INTERNAL_BEGIN}\ninternal\n{PM_ORCH_INTERNAL_END}\n\nUser question:\n翻译成中文\n\nMap-reduce context:\nSubtask MAP summaries generated for global REDUCE.\nTotalSubtasks=3"
        );
        let got = extract_pm_orch_visible_user_message(&message);
        assert_eq!(got.as_deref(), Some("翻译成中文"));
    }

    #[test]
    fn extract_pm_orch_visible_user_message_stops_before_grounded_evidence_context() {
        let message = format!(
            "{PM_ORCH_INTERNAL_BEGIN}\nGENERAL_GROUNDED_ANSWER_MODE\n{PM_ORCH_INTERNAL_END}\n\nUser question:\n北京下雨吗？广州呢？\n\nEvidence context:\nSearch layer used: native_model_search\nQuery: 北京下雨吗？广州呢\nEvidence:\n- example\n\nReturn the final answer now."
        );
        let got = extract_pm_orch_visible_user_message(&message);
        assert_eq!(got.as_deref(), Some("北京下雨吗？广州呢？"));
    }

    #[test]
    fn extract_pm_orch_visible_user_message_stops_before_subtask_context() {
        let message = format!(
            "{PM_ORCH_INTERNAL_BEGIN}\ninternal\n{PM_ORCH_INTERNAL_END}\n\nUser question:\n印尼市场机会\n\nSubtask:\n广告商业化\n\nSubtask evidence context:\ninternal evidence"
        );
        let got = extract_pm_orch_visible_user_message(&message);
        assert_eq!(got.as_deref(), Some("印尼市场机会"));
    }

    #[test]
    fn strip_pm_research_prompt_requires_pm_source() {
        let message = wrap_pm_research_prompt("pm", "User question: 增长策略".to_string());

        assert_eq!(
            strip_pm_research_prompt("pm", &message),
            "User question: 增长策略"
        );
        assert!(strip_pm_research_prompt("", &message).contains(PM_POLICY_BEGIN));
    }

    #[test]
    fn wrap_pm_research_prompt_injects_chinese_answer_language_contract() {
        let message = wrap_pm_research_prompt(
            "pm",
            "User question: 昨天印尼 ROI 为什么低了 10%？".to_string(),
        );

        assert!(message.contains("detected as Chinese/CJK"));
        assert!(message.contains("Answer in Chinese"));
        assert!(message.contains("Ignore UI/session locale"));
        assert_eq!(
            strip_pm_research_prompt("pm", &message),
            "User question: 昨天印尼 ROI 为什么低了 10%？"
        );
    }

    #[test]
    fn wrap_pm_research_prompt_uses_visible_question_for_language_detection() {
        let message = wrap_pm_research_prompt(
            "pm",
            format!(
                "{PM_ORCH_INTERNAL_BEGIN}\ninternal english routing prompt\n{PM_ORCH_INTERNAL_END}\n\nUser question: 印尼市场还有机会吗？\nQuery variants: Indonesia market"
            ),
        );

        assert!(message.contains("detected as Chinese/CJK"));
        assert!(message.contains("Answer in Chinese"));
    }

    #[test]
    fn wrap_pm_research_prompt_preserves_non_chinese_user_dominant_language() {
        let message = wrap_pm_research_prompt(
            "pm",
            "User question: Why did ROI drop yesterday?".to_string(),
        );

        assert!(message.contains("latest user-visible question"));
        assert!(message.contains("not from UI/session locale"));
        assert!(!message.contains("Answer in Chinese"));
    }

    #[test]
    fn extract_named_json_object_supports_multiline_exec_constraints() {
        let text = "前言\nEXEC_CONSTRAINTS\n{\"routeAllowlist\":[\"web.search.general\"],\"routePriority\":[\"web.search.general\"],\"sourceSlotBudgetSecs\":30,\"toolBudgetPerAttempt\":8,\"pipelineTimeoutSecs\":300,\"stopConditions\":[\"budget_exhausted\"]}";
        let got = extract_named_json_object(text, "EXEC_CONSTRAINTS");
        assert!(got.is_some());
        assert_eq!(
            got.and_then(|v| v.get("sourceSlotBudgetSecs").and_then(|x| x.as_u64())),
            Some(30)
        );
    }

    #[test]
    fn extract_named_json_object_skips_non_json_name_lines() {
        let text = "EXEC_CONSTRAINTS\n说明：下一行才是结构化块\nEXEC_CONSTRAINTS {\"routeAllowlist\":[\"web.search.general\"],\"routePriority\":[\"web.search.general\"],\"sourceSlotBudgetSecs\":28,\"toolBudgetPerAttempt\":7,\"pipelineTimeoutSecs\":260,\"stopConditions\":[\"budget_exhausted\"]}";
        let got = extract_named_json_object(text, "EXEC_CONSTRAINTS");
        assert!(got.is_some());
        assert_eq!(
            got.and_then(|v| v.get("toolBudgetPerAttempt").and_then(|x| x.as_u64())),
            Some(7)
        );
    }

    #[test]
    fn flush_pending_pm_internal_history_keeps_repeated_questions_across_turns() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "你好".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "你好！很高兴见到你。".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let mut pending_question = Some("你好".to_string());
        let mut pending_assistant = Some(MessageDto {
            role: "assistant".to_string(),
            content: "你好！你可以直接告诉我想做什么。".to_string(),
            tool_calls: None,
            tool_result: None,
            usage: None,
            thinking: None,
            pm_task_id: None,
            pm_task_status: None,
        });

        flush_pending_pm_internal_history(
            &mut messages,
            &mut pending_question,
            &mut pending_assistant,
        );

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].role, "user");
        assert_eq!(messages[2].content, "你好");
        assert_eq!(messages[3].role, "assistant");
    }
}

/// GET `/api/v1/agent/sessions/{session_id}/history` — get session message history
pub(super) async fn get_session_history(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
    Query(query): Query<GetSessionHistoryQuery>,
) -> impl IntoResponse {
    let history_started_at = std::time::Instant::now();
    let session_source = {
        let manager = get_agent_manager(&state);
        if let Some(handle) = manager.get_session(&session_id).await {
            if handle.user_id != claims.sub || handle.tenant_id != claims.tenant_id {
                return AppError::Forbidden.into_response();
            }
            handle.source
        } else {
            manager
                .list_user_sessions(&claims.tenant_id, &claims.sub, None)
                .await
                .into_iter()
                .find(|session| session.session_id == session_id)
                .map(|session| session.source)
                .unwrap_or_default()
        }
    };
    let is_pm_session = session_source.eq_ignore_ascii_case("pm");
    let normalized_source = session_source.trim().to_ascii_lowercase();
    let has_durable_visible_history = matches!(
        normalized_source.as_str(),
        "chat" | "pm" | "dataattribution" | "nl2sql" | "super_assistant" | "super-assistant"
    );

    match get_agent_manager(&state)
        .get_session_messages(&session_id, Some(&claims.tenant_id), Some(&claims.sub))
        .await
    {
        Some(session) => {
            // Build a lookup: tool_use_id -> ToolResultBlockDto.
            // This maps tool results to their corresponding tool calls by ID.
            let tool_results: std::collections::HashMap<String, ToolResultBlockDto> = session
                .messages
                .iter()
                .filter(|m| m.role == runtime::MessageRole::Tool)
                .flat_map(|m| {
                    m.blocks.iter().filter_map(|block| {
                        if let runtime::ContentBlock::ToolResult {
                            tool_use_id,
                            tool_name,
                            output,
                            is_error,
                        } = block
                        {
                            Some((
                                tool_use_id.clone(),
                                ToolResultBlockDto {
                                    tool_use_id: tool_use_id.clone(),
                                    tool_name: tool_name.clone(),
                                    output: output.clone(),
                                    is_error: *is_error,
                                },
                            ))
                        } else {
                            None
                        }
                    })
                })
                .collect();

            let mut messages: Vec<MessageDto> = Vec::new();
            let mut hide_next_internal_assistant = false;
            let mut hide_next_parent_protocol_assistant = false;
            let mut pending_pm_question: Option<String> = None;
            let mut pending_pm_assistant: Option<MessageDto> = None;
            for m in session.messages.iter().filter(|m| {
                m.role != runtime::MessageRole::System && m.role != runtime::MessageRole::Tool
            }) {
                let mut content = String::new();
                let mut tool_calls: Option<Vec<ToolCallBlockDto>> = None;

                for block in &m.blocks {
                    match block {
                        runtime::ContentBlock::Text { text } => {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(text);
                        }
                        runtime::ContentBlock::ToolUse { id, name, input } => {
                            if tool_calls.is_none() {
                                tool_calls = Some(Vec::new());
                            }
                            if let Some(ref mut calls) = tool_calls {
                                // Attach the corresponding tool result if present.
                                let result = tool_results.get(id).cloned();
                                calls.push(ToolCallBlockDto {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: input.clone(),
                                    result,
                                });
                            }
                        }
                        runtime::ContentBlock::ToolResult { .. } => {
                            // ToolResult blocks are handled above via the tool_results map.
                        }
                    }
                }

                let role_str = match m.role {
                    runtime::MessageRole::System => "system",
                    runtime::MessageRole::User => "user",
                    runtime::MessageRole::Assistant => "assistant",
                    runtime::MessageRole::Tool => "tool",
                };

                let usage = m.usage.map(|u| UsageBlockDto {
                    input: u.input_tokens,
                    output: u.output_tokens,
                    cache_creation: u.cache_creation_input_tokens,
                    cache_read: u.cache_read_input_tokens,
                });

                let thinking = m.thinking.clone();

                if crate::routes::super_assistant::should_hide_super_assistant_parent_protocol_message(
                    role_str,
                    &content,
                    &mut hide_next_parent_protocol_assistant,
                ) {
                    continue;
                }

                if role_str == "user" {
                    let stripped = strip_pm_research_prompt(&session_source, &content);
                    if is_pm_orch_internal_message(&stripped) {
                        if let Some(question) = extract_pm_orch_visible_user_message(&stripped) {
                            let normalized_question = question.trim().to_string();
                            if pending_pm_question.as_deref() == Some(normalized_question.as_str())
                            {
                                // Retry in the same visible question chain.
                                // Keep the previous assistant snapshot until a newer
                                // assistant message arrives; otherwise upstream
                                // failures can leave history empty.
                            } else {
                                flush_pending_pm_internal_history(
                                    &mut messages,
                                    &mut pending_pm_question,
                                    &mut pending_pm_assistant,
                                );
                                pending_pm_question = Some(normalized_question);
                                pending_pm_assistant = None;
                            }
                            hide_next_internal_assistant = false;
                        } else {
                            // Fallback for unknown internal format: hide the next
                            // assistant line to avoid leaking orchestration prompts.
                            hide_next_internal_assistant = true;
                        }
                        continue;
                    }

                    flush_pending_pm_internal_history(
                        &mut messages,
                        &mut pending_pm_question,
                        &mut pending_pm_assistant,
                    );
                    hide_next_internal_assistant = false;

                    let stripped =
                        strip_pm_retrieval_hint(&strip_pm_orch_internal_message(&stripped));
                    if stripped.trim().is_empty() {
                        continue;
                    }
                    push_history_message_dedup(
                        &mut messages,
                        MessageDto {
                            role: role_str.to_string(),
                            content: stripped,
                            tool_calls,
                            tool_result: None,
                            usage,
                            thinking,
                            pm_task_id: None,
                            pm_task_status: None,
                        },
                    );
                    continue;
                }

                if role_str == "assistant" {
                    if hide_next_internal_assistant {
                        hide_next_internal_assistant = false;
                        continue;
                    }
                    let assistant_message = MessageDto {
                        role: role_str.to_string(),
                        content,
                        tool_calls,
                        tool_result: None,
                        usage,
                        thinking,
                        pm_task_id: None,
                        pm_task_status: None,
                    };
                    if pending_pm_question.is_some() {
                        merge_pending_pm_assistant(&mut pending_pm_assistant, assistant_message);
                    } else {
                        push_history_message_dedup(&mut messages, assistant_message);
                    }
                    continue;
                }

                flush_pending_pm_internal_history(
                    &mut messages,
                    &mut pending_pm_question,
                    &mut pending_pm_assistant,
                );
                hide_next_internal_assistant = false;
                push_history_message_dedup(
                    &mut messages,
                    MessageDto {
                        role: role_str.to_string(),
                        content,
                        tool_calls,
                        tool_result: None,
                        usage,
                        thinking,
                        pm_task_id: None,
                        pm_task_status: None,
                    },
                );
            }

            flush_pending_pm_internal_history(
                &mut messages,
                &mut pending_pm_question,
                &mut pending_pm_assistant,
            );

            if has_durable_visible_history {
                let durable_visible = load_durable_visible_history(
                    &state,
                    &claims.tenant_id,
                    &claims.sub,
                    &session_id,
                );
                let durable_user_count = durable_visible
                    .iter()
                    .filter(|message| message.role == "user")
                    .count();
                let runtime_user_count = messages
                    .iter()
                    .filter(|message| message.role == "user")
                    .count();
                if !durable_visible.is_empty() && durable_user_count >= runtime_user_count {
                    let mut recovered = durable_visible;
                    let mut durable_occurrences = std::collections::HashMap::new();
                    for message in &recovered {
                        if message.role == "user" {
                            *durable_occurrences
                                .entry(history_user_identity_content(&message.content))
                                .or_insert(0usize) += 1;
                        }
                    }
                    let mut runtime_occurrences = std::collections::HashMap::new();
                    for message in messages.iter().filter(|message| message.role == "user") {
                        let content = history_user_identity_content(&message.content);
                        let occurrence =
                            runtime_occurrences.entry(content.clone()).or_insert(0usize);
                        *occurrence += 1;
                        if *occurrence > durable_occurrences.get(&content).copied().unwrap_or(0) {
                            recovered.push(message.clone());
                        }
                    }
                    messages = recovered;
                }
            }

            let pm_research = if is_pm_session {
                match load_pm_session_history_replay_from_db(
                    &state.db,
                    &session_id,
                    &claims.tenant_id,
                    &claims.sub,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        match &error {
                            AppError::Database(db_error) => match db_error {
                                sqlx::Error::Database(db) => {
                                    tracing::warn!(
                                        session_id = %session_id,
                                        tenant_id = %claims.tenant_id,
                                        user_id = %claims.sub,
                                        db_code = ?db.code(),
                                        db_message = %db.message(),
                                        db_error = %db_error,
                                        db_error_debug = ?db_error,
                                        "load_pm_session_history_replay_from_db failed"
                                    );
                                }
                                _ => {
                                    tracing::warn!(
                                        session_id = %session_id,
                                        tenant_id = %claims.tenant_id,
                                        user_id = %claims.sub,
                                        db_error = %db_error,
                                        db_error_debug = ?db_error,
                                        "load_pm_session_history_replay_from_db failed"
                                    );
                                }
                            },
                            _ => {
                                tracing::warn!(
                                    session_id = %session_id,
                                    tenant_id = %claims.tenant_id,
                                    user_id = %claims.sub,
                                    error = %error,
                                    error_debug = ?error,
                                    "load_pm_session_history_replay_from_db failed"
                                );
                            }
                        }
                        None
                    }
                }
            } else {
                None
            };

            if is_pm_session {
                match load_pm_session_task_bindings_from_db(
                    &state.db,
                    &session_id,
                    &claims.tenant_id,
                    &claims.sub,
                )
                .await
                {
                    Ok(bindings) => {
                        reconcile_pm_task_history_turns(&mut messages, &bindings);
                        assign_pm_task_bindings_to_history_messages(&mut messages, &bindings);
                    }
                    Err(error) => {
                        tracing::warn!(
                            session_id = %session_id,
                            tenant_id = %claims.tenant_id,
                            user_id = %claims.sub,
                            error = %error,
                            error_debug = ?error,
                            "load_pm_session_task_bindings_from_db failed"
                        );
                    }
                }
            }

            let (messages, page) = paginate_history_messages(
                &messages,
                query.before_turn_cursor,
                query.limit_turns,
                query.max_bytes,
            );
            let metadata_answers = history_page_assistant_answer_texts(&messages);
            let super_assistant_turns = match load_super_assistant_turn_message_metadata(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                &session_id,
                &metadata_answers,
            )
            .await
            {
                Ok(items) if !items.is_empty() => Some(items),
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(
                        session_id = %session_id,
                        tenant_id = %claims.tenant_id,
                        user_id = %claims.sub,
                        error = %error,
                        "load_super_assistant_turn_message_metadata failed"
                    );
                    None
                }
            };

            let pm_replay_events = pm_research
                .as_ref()
                .map(|replay| replay.events.len())
                .unwrap_or(0);
            let elapsed_ms = history_started_at.elapsed().as_millis() as u64;
            if elapsed_ms >= 500
                || page.approx_payload_bytes >= 128 * 1024
                || pm_replay_events >= 24
            {
                tracing::info!(
                    session_id = %session_id,
                    tenant_id = %claims.tenant_id,
                    user_id = %claims.sub,
                    source = %session_source,
                    elapsed_ms,
                    returned_messages = messages.len(),
                    returned_turns = page.returned_turns,
                    total_turns = page.total_turns,
                    approx_payload_bytes = page.approx_payload_bytes,
                    pm_replay_events,
                    "agent session history loaded"
                );
            }

            let response = SessionHistoryResponse {
                session_id: session_id.clone(),
                messages,
                page: Some(page),
                pm_research,
                super_assistant_turns,
            };
            Json(response).into_response()
        }
        None => {
            let durable_visible =
                load_durable_visible_history(&state, &claims.tenant_id, &claims.sub, &session_id);
            if !durable_visible.is_empty() {
                let (messages, page) = paginate_history_messages(
                    &durable_visible,
                    query.before_turn_cursor,
                    query.limit_turns,
                    query.max_bytes,
                );
                let metadata_answers = history_page_assistant_answer_texts(&messages);
                let super_assistant_turns = load_super_assistant_turn_message_metadata(
                    &state.db,
                    &claims.tenant_id,
                    &claims.sub,
                    &session_id,
                    &metadata_answers,
                )
                .await
                .ok()
                .filter(|items| !items.is_empty());
                return Json(SessionHistoryResponse {
                    session_id,
                    messages,
                    page: Some(page),
                    pm_research: None,
                    super_assistant_turns,
                })
                .into_response();
            }
            AppError::NotFound(format!("session {session_id} history not found")).into_response()
        }
    }
}
