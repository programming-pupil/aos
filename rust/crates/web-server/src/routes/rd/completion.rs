//! Direct RD chat completion adapter and model/tool option parsing.

use super::*;

#[derive(Debug)]
pub(super) struct RdCompletionResult {
    pub(super) text: String,
    pub(super) model: String,
    pub(super) provider: String,
    pub(super) api_key_id: Option<String>,
    pub(super) usage: Option<RdTokenUsageSnapshot>,
}

pub(super) async fn run_rd_completion(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    selected_model: Option<&str>,
    prompt: String,
) -> Result<RdCompletionResult, AppError> {
    run_rd_completion_with_options(
        state,
        tenant_id,
        user_id,
        selected_model,
        prompt,
        "You are AOS Code Studio, a careful coding agent. Prefer safe patches, precise plans, and verifiable tests.".to_string(),
        8192,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_rd_completion_with_options(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    selected_model: Option<&str>,
    prompt: String,
    system_prompt: String,
    max_tokens: u32,
    temperature: Option<f64>,
) -> Result<RdCompletionResult, AppError> {
    let entries = state
        .config_registry()
        .resolve_api_keys_by_model_type(tenant_id, Some(RD_SCENARIO), "chat")
        .await
        .map_err(|e| AppError::Internal(format!("failed to load RD API keys: {e}")))?;
    if entries.is_empty() {
        return Err(AppError::ValidationError(
            "no RD-scoped chat API key found; please add a key with scenario 'rd' and model_type 'chat'".to_string(),
        ));
    }
    let mut candidates = entries;
    if let Some(model) = selected_model.filter(|v| !v.trim().is_empty()) {
        candidates.retain(|entry| {
            entry
                .model
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(model))
        });
        if candidates.is_empty() {
            return Err(AppError::ValidationError(format!(
                "selected RD model '{model}' is unavailable"
            )));
        }
    }
    let mut last_error = None;
    for entry in candidates {
        let model = entry
            .model
            .clone()
            .unwrap_or_else(|| selected_model.unwrap_or("gpt-4.1").to_string());
        let provider = match api::build_provider(
            &entry.provider,
            &model,
            &entry.key,
            entry.base_url.as_deref(),
        ) {
            Ok(provider) => provider,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let req = api::MessageRequest {
            model: model.clone(),
            max_tokens,
            messages: vec![api::InputMessage {
                role: "user".to_string(),
                content: vec![api::InputContentBlock::Text {
                    text: prompt.clone(),
                }],
            }],
            system: Some(system_prompt.clone()),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };
        for attempt in 1..=2 {
            match provider.send_message(&req).await {
                Ok(response) => {
                    let text = response
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            api::OutputContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    let usage = response.usage.clone();
                    let _ = state
                        .config_registry()
                        .record_token_usage(agent_gateway::TokenUsageParams {
                            tenant_id: tenant_id.to_string(),
                            user_id: user_id.to_string(),
                            session_id: None,
                            model: model.clone(),
                            input_tokens: i64::from(usage.input_tokens),
                            output_tokens: i64::from(usage.output_tokens),
                            cache_creation_tokens: i64::from(usage.cache_creation_input_tokens),
                            cache_read_tokens: i64::from(usage.cache_read_input_tokens),
                            api_key_id: Some(entry.id.clone()),
                            provider: entry.provider.clone(),
                            custom_input_price: None,
                            custom_output_price: None,
                        })
                        .await;
                    return Ok(RdCompletionResult {
                        text,
                        model: model.clone(),
                        provider: entry.provider,
                        api_key_id: Some(entry.id),
                        usage: Some(RdTokenUsageSnapshot::from_api(&usage, &model)),
                    });
                }
                Err(error) => {
                    let retryable = rd_completion_error_is_retryable(&error);
                    last_error = Some(error.to_string());
                    if retryable && attempt < 2 {
                        tracing::warn!(
                            model = %model,
                            provider = %entry.provider,
                            attempt,
                            error = %error,
                            "transient RD model response failure; retrying bounded completion"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                        continue;
                    }
                    break;
                }
            }
        }
    }
    Err(AppError::Internal(format!(
        "all RD model candidates failed: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )))
}

fn rd_completion_error_is_retryable(error: &api::ApiError) -> bool {
    if error.is_retryable() {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    [
        "error decoding response body",
        "body read",
        "connection reset",
        "connection closed",
        "broken pipe",
        "unexpected eof",
        "temporarily unavailable",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

pub(super) fn extract_rd_allowed_tools(
    value: Option<&Value>,
) -> Result<Option<Vec<String>>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut tools = BTreeSet::new();
    collect_rd_allowed_tools(value, &mut tools)?;
    if tools.is_empty() {
        Ok(None)
    } else {
        Ok(Some(tools.into_iter().collect()))
    }
}

fn collect_rd_allowed_tools(value: &Value, tools: &mut BTreeSet<String>) -> Result<(), AppError> {
    match value {
        Value::Null => Ok(()),
        Value::String(raw) => {
            for token in raw
                .split(|ch: char| ch == ',' || ch == '\n' || ch == '\t' || ch.is_whitespace())
                .map(str::trim)
                .filter(|token| !token.is_empty())
            {
                tools.insert(token.to_string());
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                let Some(tool) = item.as_str().map(str::trim).filter(|tool| !tool.is_empty())
                else {
                    return Err(AppError::ValidationError(
                        "allowedTools array must contain only non-empty strings".to_string(),
                    ));
                };
                tools.insert(tool.to_string());
            }
            Ok(())
        }
        Value::Object(map) => {
            let nested = map
                .get("tools")
                .or_else(|| map.get("allowedTools"))
                .or_else(|| map.get("allowed_tools"));
            match nested {
                Some(nested) => collect_rd_allowed_tools(nested, tools),
                None => Err(AppError::ValidationError(
                    "allowedTools object must contain `tools`, `allowedTools`, or `allowed_tools`"
                        .to_string(),
                )),
            }
        }
        _ => Err(AppError::ValidationError(
            "allowedTools must be a string, string array, or object with a tools array".to_string(),
        )),
    }
}
