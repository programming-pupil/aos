//! WatchDog natural-language intent parsing.
//!
//! Keep prompt text, rule fallbacks, and normalization here so the AgentOps route
//! module can stay focused on HTTP/actions/query orchestration.

use serde::Deserialize;
#[cfg(feature = "pm")]
use serde_json::Value;

use crate::state::AppState;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchDogIntent {
    pub intent: Option<String>,
    pub scope: Option<String>,
    pub capability: Option<String>,
    pub status: Option<Vec<String>>,
    pub queue_intent: Option<String>,
    pub stale_minutes: Option<i64>,
    pub task_index: Option<usize>,
    pub action: Option<String>,
    pub limit: Option<i64>,
    pub needs_llm_summary: Option<bool>,
}

impl Default for WatchDogIntent {
    fn default() -> Self {
        Self {
            intent: Some("list_tasks".to_string()),
            scope: Some("conversation".to_string()),
            capability: None,
            status: None,
            queue_intent: None,
            stale_minutes: None,
            task_index: None,
            action: None,
            limit: Some(20),
            needs_llm_summary: Some(true),
        }
    }
}

impl WatchDogIntent {
    pub fn action_contract_kind_index(&self) -> Option<(&'static str, usize)> {
        let action = self.action.as_deref()?.trim().to_ascii_lowercase();
        let kind = match action.as_str() {
            "detail" | "open_task" | "detail_task" => "detail",
            "cancel" | "cancel_task" => "cancel",
            "retry" | "retry_task" => "retry",
            _ => return None,
        };
        Some((
            kind,
            self.task_index.filter(|value| *value > 0).unwrap_or(1),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchDogQueueIntent {
    All,
    Dead,
    StaleLease,
}

impl WatchDogQueueIntent {
    pub fn as_contract_value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Dead => "dead",
            Self::StaleLease => "stale_lease",
        }
    }
}

pub async fn parse_watchdog_intent(
    state: &AppState,
    tenant_id: &str,
    question: &str,
    default_scope: &str,
) -> WatchDogIntent {
    let mut fallback = rule_watchdog_intent(question, default_scope);
    let Some(llm_intent) =
        parse_watchdog_intent_with_llm(state, tenant_id, question, default_scope).await
    else {
        return fallback;
    };
    merge_watchdog_intent(&mut fallback, llm_intent);
    sanitize_watchdog_intent(fallback)
}

pub fn rule_watchdog_intent(question: &str, default_scope: &str) -> WatchDogIntent {
    let lower = question.to_ascii_lowercase();
    let mut intent = WatchDogIntent {
        scope: Some(if question.contains("全部") || lower.contains("all") {
            "tenant".to_string()
        } else {
            default_scope.to_string()
        }),
        capability: detect_capability_filter(question).map(ToOwned::to_owned),
        stale_minutes: detect_stale_minutes(question),
        queue_intent: detect_queue_intent(question)
            .map(|value| value.as_contract_value().to_string()),
        ..WatchDogIntent::default()
    };
    if question.contains("运行")
        || question.contains("在跑")
        || question.contains("工作")
        || question.contains("执行")
        || lower.contains("running")
        || lower.contains("active")
    {
        intent.status = Some(
            active_watchdog_statuses()
                .iter()
                .map(|value| value.to_string())
                .collect(),
        );
    }
    if let Some((kind, index)) = parse_watchdog_action_like(question) {
        intent.action = Some(kind.to_string());
        intent.task_index = Some(index);
    }
    intent
}

async fn parse_watchdog_intent_with_llm(
    state: &AppState,
    tenant_id: &str,
    question: &str,
    default_scope: &str,
) -> Option<WatchDogIntent> {
    #[cfg(not(feature = "pm"))]
    {
        let _ = (state, tenant_id, question, default_scope);
        None
    }
    #[cfg(feature = "pm")]
    {
        if std::env::var("AOS_WATCHDOG_INTENT_LLM")
            .ok()
            .is_some_and(|value| matches!(value.trim(), "0" | "false" | "FALSE" | "off" | "OFF"))
        {
            return None;
        }
        let system_prompt = watchdog_intent_system_prompt(default_scope);
        let prompt = format!("用户问题：{question}");
        match crate::routes::pm::run_chat_completion_with_any_chat_key(
            state,
            tenant_id,
            state.default_model.clone(),
            vec![crate::routes::chat::ChatMessage {
                role: "user".to_string(),
                content: Value::String(prompt),
            }],
            &system_prompt,
            800,
        )
        .await
        {
            Ok(result) => parse_json_object_from_text(&result.answer)
                .and_then(|value| serde_json::from_value::<WatchDogIntent>(value).ok()),
            Err(error) => {
                tracing::warn!(
                    tenant_id,
                    error = %error,
                    "watchdog intent llm parse failed; using rule intent"
                );
                None
            }
        }
    }
}

#[cfg(feature = "pm")]
fn watchdog_intent_system_prompt(default_scope: &str) -> String {
    crate::routes::builtin_skills::PromptRegistry::render(
        crate::routes::builtin_skills::PromptId::WatchdogIntent,
        &[("default_scope", default_scope)],
    )
}

fn merge_watchdog_intent(base: &mut WatchDogIntent, incoming: WatchDogIntent) {
    if incoming.intent.is_some() {
        base.intent = incoming.intent;
    }
    if incoming.scope.is_some() {
        base.scope = incoming.scope;
    }
    if incoming.capability.is_some() {
        base.capability = incoming.capability;
    }
    if incoming.status.is_some() {
        base.status = incoming.status;
    }
    if incoming.queue_intent.is_some() {
        base.queue_intent = incoming.queue_intent;
    }
    if incoming.stale_minutes.is_some() {
        base.stale_minutes = incoming.stale_minutes;
    }
    if incoming.task_index.is_some() {
        base.task_index = incoming.task_index;
    }
    if incoming.action.is_some() {
        base.action = incoming.action;
    }
    if incoming.limit.is_some() {
        base.limit = incoming.limit;
    }
    if incoming.needs_llm_summary.is_some() {
        base.needs_llm_summary = incoming.needs_llm_summary;
    }
}

fn sanitize_watchdog_intent(mut intent: WatchDogIntent) -> WatchDogIntent {
    intent.scope = intent
        .scope
        .as_deref()
        .and_then(normalize_watchdog_scope)
        .map(ToOwned::to_owned)
        .or_else(|| Some("conversation".to_string()));
    intent.capability = intent
        .capability
        .as_deref()
        .and_then(normalize_watchdog_capability)
        .map(ToOwned::to_owned);
    intent.queue_intent = intent.queue_intent.as_deref().and_then(|value| {
        normalize_queue_intent(value).map(|intent| intent.as_contract_value().to_string())
    });
    intent.status = normalized_status_filter(intent.status.as_deref())
        .map(|values| values.into_iter().map(ToOwned::to_owned).collect());
    intent.stale_minutes = intent.stale_minutes.map(|value| value.clamp(1, 1440));
    intent.limit = Some(intent.limit.unwrap_or(20).clamp(1, 50));
    intent
}

pub fn normalize_watchdog_scope(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "conversation" | "current" | "当前会话" => Some("conversation"),
        "user" | "me" | "当前用户" => Some("user"),
        "tenant" | "all" | "全部" | "租户" => Some("tenant"),
        _ => None,
    }
}

pub fn normalize_watchdog_capability(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ai_chat" | "chat" | "ai" => Some("ai_chat"),
        "pm_assistant" | "pm" | "operations" => Some("pm_assistant"),
        "rd_agent" | "rd" | "code" | "coding" => Some("rd_agent"),
        "nl2sql" | "sql" | "data" => Some("nl2sql"),
        "super_adversarial" | "adversarial" | "debate" => Some("super_adversarial"),
        "watchdog" | "watch_dog" => Some("watchdog"),
        "generic_ai" | "generic" => Some("generic_ai"),
        _ => None,
    }
}

pub fn normalize_queue_intent(value: &str) -> Option<WatchDogQueueIntent> {
    match value.trim().to_ascii_lowercase().as_str() {
        "all" | "queue" => Some(WatchDogQueueIntent::All),
        "dead" | "dead_letter" => Some(WatchDogQueueIntent::Dead),
        "stale_lease" | "lease" | "stale" => Some(WatchDogQueueIntent::StaleLease),
        _ => None,
    }
}

pub fn active_watchdog_statuses() -> &'static [&'static str] {
    &[
        "queued",
        "claimed",
        "running",
        "waiting_input",
        "retrying",
        "cancelling",
    ]
}

pub fn normalized_status_filter(values: Option<&[String]>) -> Option<Vec<&'static str>> {
    let values = values?;
    let mut out = Vec::new();
    for value in values {
        let normalized = match value.trim().to_ascii_lowercase().as_str() {
            "created" => "created",
            "queued" => "queued",
            "claimed" => "claimed",
            "running" => "running",
            "waiting_input" => "waiting_input",
            "retrying" => "retrying",
            "cancelling" => "cancelling",
            "completed" | "succeeded" => "completed",
            "failed" => "failed",
            "cancelled" | "canceled" => "cancelled",
            "timed_out" | "timeout" => "timed_out",
            "stale" => "stale",
            "dead" => "dead",
            "active" => {
                out.extend_from_slice(active_watchdog_statuses());
                continue;
            }
            _ => continue,
        };
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(feature = "pm")]
fn parse_json_object_from_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if value.is_object() {
            return Some(value);
        }
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end])
        .ok()
        .filter(Value::is_object)
}

pub fn detect_queue_intent(question: &str) -> Option<WatchDogQueueIntent> {
    let lower = question.to_ascii_lowercase();
    if question.contains("死信") || lower.contains("dead") {
        Some(WatchDogQueueIntent::Dead)
    } else if question.contains("租约")
        || question.contains("lease")
        || question.contains("队列超时")
        || lower.contains("lease")
    {
        Some(WatchDogQueueIntent::StaleLease)
    } else if question.contains("队列") || lower.contains("queue") || lower.contains("worker") {
        Some(WatchDogQueueIntent::All)
    } else {
        None
    }
}

pub fn detect_capability_filter(question: &str) -> Option<&'static str> {
    let lower = question.to_ascii_lowercase();
    if question.contains("产运") || lower.contains("pm") {
        Some("pm_assistant")
    } else if question.contains("RD") || question.contains("代码") || lower.contains("rd") {
        Some("rd_agent")
    } else if question.contains("对抗") || lower.contains("adversarial") || lower.contains("debate")
    {
        Some("super_adversarial")
    } else if question.contains("数据") || lower.contains("sql") {
        Some("nl2sql")
    } else if question.contains("AI") || question.contains("对话") || lower.contains("chat") {
        Some("ai_chat")
    } else {
        None
    }
}

pub fn detect_stale_minutes(question: &str) -> Option<i64> {
    if question.contains("没心跳")
        || question.contains("卡")
        || question.contains("超时")
        || question.contains("stale")
    {
        Some(10)
    } else {
        None
    }
}

fn parse_watchdog_action_like(question: &str) -> Option<(&'static str, usize)> {
    let compact = question.trim();
    let kind = if compact.starts_with("详情") || compact.to_ascii_lowercase().starts_with("detail")
    {
        "detail"
    } else if compact.starts_with("取消") || compact.to_ascii_lowercase().starts_with("cancel") {
        "cancel"
    } else if compact.starts_with("重试") || compact.to_ascii_lowercase().starts_with("retry") {
        "retry"
    } else {
        return None;
    };
    let index = compact
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(1);
    Some((kind, index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_running_agents_maps_to_active_statuses() {
        let intent = rule_watchdog_intent("当前有哪些 agent 在运行？", "conversation");
        assert_eq!(intent.scope.as_deref(), Some("conversation"));
        assert_eq!(
            normalized_status_filter(intent.status.as_deref()),
            Some(active_watchdog_statuses().to_vec())
        );
    }

    #[test]
    fn golden_actions_are_structured_before_general_chat() {
        let intent = rule_watchdog_intent("取消 1", "conversation");
        assert_eq!(intent.action.as_deref(), Some("cancel"));
        assert_eq!(intent.task_index, Some(1));
        assert_eq!(intent.action_contract_kind_index(), Some(("cancel", 1)));
    }

    #[test]
    fn golden_queue_and_capability_filters_are_stable() {
        let queue = rule_watchdog_intent("死信任务", "conversation");
        assert_eq!(queue.queue_intent.as_deref(), Some("dead"));

        let pm = rule_watchdog_intent("产运助手有几个在工作？", "conversation");
        assert_eq!(pm.capability.as_deref(), Some("pm_assistant"));
        assert!(pm.status.is_some());
    }

    #[cfg(feature = "pm")]
    #[test]
    fn watchdog_skill_prompt_preserves_structured_contract() {
        let prompt = watchdog_intent_system_prompt("conversation");
        assert!(prompt.contains("intent: list_tasks"));
        assert!(prompt.contains("action: detail | cancel | retry"));
        assert!(prompt.contains("[\"queued\",\"claimed\",\"running\""));
        assert!(prompt.contains("当前有哪些 agent 在运行？"));
    }
}
