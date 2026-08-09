use super::*;

pub(super) fn turn_to_run_turn_response(
    result: TurnResult,
    pm_quality: Option<PmAnswerQualityDto>,
    pm_question: Option<&str>,
) -> RunTurnResponse {
    turn_to_run_turn_response_with_report(result, pm_quality, pm_question, true)
}

pub(super) fn turn_to_run_turn_response_with_report(
    result: TurnResult,
    pm_quality: Option<PmAnswerQualityDto>,
    pm_question: Option<&str>,
    include_pm_report: bool,
) -> RunTurnResponse {
    let is_shared_chat_tool_loop = pm_quality
        .as_ref()
        .map(|quality| quality.conflict_reason.to_ascii_lowercase())
        .is_some_and(|reason| {
            reason.contains("shared chat")
                || reason.contains("chatturnengine")
                || reason.contains("chat tool loop")
        });
    let pm_report = if include_pm_report && pm_quality.is_some() && !is_shared_chat_tool_loop {
        Some(build_pm_report_artifact(
            pm_question,
            &result.text,
            pm_quality.as_ref(),
        ))
    } else {
        None
    };
    let compacted = result.compacted.clone();
    let metadata = result.metadata.map(|m| SessionActivatedDto {
        mcp_servers: m.mcp_servers,
        skills: m.skills,
        permission_mode: m.permission_mode,
        model: m.model,
    });
    RunTurnResponse {
        session_id: result.session_id,
        text: result.text,
        tool_calls: result
            .tool_calls
            .into_iter()
            .map(ToolCallDto::from)
            .collect(),
        usage: UsageDto::from(result.usage),
        iterations: result.iterations,
        compacted,
        metadata,
        pm_quality,
        pm_report,
    }
}

pub(crate) async fn maybe_dispatch_skill_command(
    message: &str,
    tenant_id: &str,
    db: &sqlx::SqlitePool,
) -> Option<String> {
    let message = message.trim();

    // Slash commands remain the highest-signal form. Natural-language skill
    // requests are handled below after loading the tenant's enabled registry.
    let is_slash_command = message.starts_with('/') && !message.starts_with("//");

    let registered_skills: Vec<(String,)> = if is_slash_command
        || explicit_natural_skill_request(message)
        || message_mentions_registered_skill(message)
    {
        sqlx::query_as(
            "SELECT name FROM skills_registry WHERE tenant_id = ? AND enabled = 1 ORDER BY name ASC",
        )
        .bind(tenant_id)
        .fetch_all(db)
        .await
        .unwrap_or_default()
    } else {
        return None;
    };

    if !is_slash_command {
        let Some(skill_name) = select_natural_language_skill(message, &registered_skills) else {
            return None;
        };
        let args = remove_skill_request_prefix(message);
        let prompt = if args.is_empty() {
            format!("${skill_name}")
        } else {
            format!("${skill_name} {args}")
        };
        tracing::debug!(
            skill_name = %skill_name,
            original_chars = message.chars().count(),
            prompt_chars = prompt.chars().count(),
            "natural-language skill request intercepted and transformed"
        );
        return Some(prompt);
    }

    // Must start with a single slash followed by a name character.
    let slash_idx = message.find(char::is_whitespace).unwrap_or(message.len());
    let raw_name = &message[1..slash_idx];

    // Skip known built-in slash commands.
    if SKIP_INTERCEPT.contains(&raw_name) {
        return None;
    }

    // Skip numeric/strictly-allocation patterns to avoid interpreting `/1`, `/42` etc.
    if raw_name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    // Validate skill name: lowercase alphanumeric + hyphens/underscores, 1..=64 chars.
    if raw_name.len() > 64
        || raw_name
            .bytes()
            .any(|b| !b.is_ascii_lowercase() && !b.is_ascii_digit() && b != b'-' && b != b'_')
    {
        return None;
    }

    // Check the DB for an enabled skill matching tenant + name.
    registered_skills
        .iter()
        .any(|(name,)| normalize_skill_identifier(name) == raw_name)
        .then_some(())?;

    // Skill found — build the $skill-name prompt exactly as the CLI does.
    let args = if slash_idx < message.len() {
        message[slash_idx..].trim()
    } else {
        ""
    };
    let prompt = if args.is_empty() {
        format!("${raw_name}")
    } else {
        format!("${raw_name} {args}")
    };

    tracing::debug!(
        skill_name = %raw_name,
        original_chars = message.chars().count(),
        prompt_chars = prompt.chars().count(),
        "skill slash command intercepted and transformed"
    );

    Some(prompt)
}

fn normalize_skill_identifier(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn explicit_natural_skill_request(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "用skill",
        "用 skill",
        "使用skill",
        "使用 skill",
        "通过skill",
        "通过 skill",
        "调用skill",
        "调用 skill",
        "用技能",
        "使用技能",
        "调用技能",
        "use skill",
        "using skill",
        "with skill",
        "via skill",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
}

fn message_mentions_registered_skill(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("skill") || lower.contains("技能")
}

fn registered_skill_mentioned(message: &str, skill_name: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let normalized = normalize_skill_identifier(skill_name);
    if normalized.is_empty() {
        return false;
    }
    if lower.contains(&normalized) {
        return true;
    }
    // Users frequently copy a hyphenated command with spaces/underscores.
    let folded = normalized.replace(['-', '_'], " ");
    folded.len() > 1 && lower.contains(&folded)
}

fn select_natural_language_skill(message: &str, registered_skills: &[(String,)]) -> Option<String> {
    let mentioned = registered_skills
        .iter()
        .map(|(name,)| name)
        .filter(|name| registered_skill_mentioned(message, name))
        .filter_map(|name| {
            let normalized = normalize_skill_identifier(name);
            (normalized.len() <= 64
                && normalized.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                }))
            .then_some(normalized)
        })
        .collect::<Vec<_>>();
    if mentioned.len() == 1 {
        return mentioned.into_iter().next();
    }

    // A generic "use skill" request is unambiguous only when the tenant has
    // exactly one enabled skill. With multiple skills, do not guess and route
    // the request through the normal assistant so it can ask which one.
    if explicit_natural_skill_request(message) && registered_skills.len() == 1 {
        return registered_skills
            .first()
            .map(|(name,)| normalize_skill_identifier(name));
    }
    None
}

fn remove_skill_request_prefix(message: &str) -> String {
    let mut result = message.trim().to_string();
    for cue in [
        "用skill",
        "用 skill",
        "使用skill",
        "使用 skill",
        "通过skill",
        "通过 skill",
        "调用skill",
        "调用 skill",
        "用技能",
        "使用技能",
        "调用技能",
        "use skill",
        "using skill",
        "with skill",
        "via skill",
    ] {
        if result.to_ascii_lowercase().starts_with(cue) {
            result = result[cue.len()..].trim().to_string();
            break;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

pub(super) async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    match crate::auth::verify_token(&state, token).await {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_turn(text: &str) -> TurnResult {
        TurnResult {
            session_id: "session-1".to_string(),
            text: text.to_string(),
            thinking: None,
            tool_calls: Vec::new(),
            usage: TokenUsageRecord {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                model: "test-model".to_string(),
            },
            compacted: None,
            iterations: 1,
            metadata: None,
            hot_reloaded: false,
        }
    }

    #[test]
    fn shared_chat_tool_loop_response_omits_pm_report_artifact() {
        let mut quality = build_pm_direct_answer_quality();
        quality.conflict_reason =
            "PmTurnRouter used shared ChatTurnEngine Codex-like tool loop".to_string();

        let response = turn_to_run_turn_response(
            minimal_turn("北京今天有雨。"),
            Some(quality),
            Some("北京下雨吗"),
        );

        assert!(response.pm_quality.is_some());
        assert!(response.pm_report.is_none());
    }

    #[test]
    fn deep_pm_response_keeps_pm_report_artifact() {
        let mut quality = build_pm_direct_answer_quality();
        quality.conflict_reason = "deep research quality gate passed".to_string();

        let response = turn_to_run_turn_response(
            minimal_turn("## 核心结论\n可以优先验证高价值用户策略。"),
            Some(quality),
            Some("制定增长策略"),
        );

        assert!(response.pm_quality.is_some());
        assert!(response.pm_report.is_some());
    }

    #[test]
    fn natural_language_skill_request_selects_the_only_enabled_skill() {
        let skills = vec![("ab-experiment-analyzer".to_string(),)];
        assert_eq!(
            select_natural_language_skill("用skill查一下昨天哪个产品的ROI最好", &skills),
            Some("ab-experiment-analyzer".to_string())
        );
        assert_eq!(
            remove_skill_request_prefix("用skill查一下昨天哪个产品的ROI最好"),
            "查一下昨天哪个产品的ROI最好"
        );
    }

    #[test]
    fn natural_language_skill_request_matches_a_named_skill_without_guessing() {
        let skills = vec![
            ("frontend-design".to_string(),),
            ("ab-experiment-analyzer".to_string(),),
        ];
        assert_eq!(
            select_natural_language_skill("请用 frontend-design skill 设计一个登录页", &skills),
            Some("frontend-design".to_string())
        );
        assert_eq!(
            select_natural_language_skill("用skill查ROI", &skills),
            None,
            "ambiguous generic requests must not silently pick a skill"
        );
    }

    #[test]
    fn ordinary_skill_word_does_not_intercept_without_a_registered_match() {
        let skills = vec![("frontend-design".to_string(),)];
        assert_eq!(
            select_natural_language_skill("这个技能的设计原则是什么？", &skills),
            None
        );
        assert!(!explicit_natural_skill_request("昨天ROI最好的是哪个产品"));
    }
}
