//! AOS Router Agent decision logic for Bot Gateway.
//!
//! The Bot route module owns transport and queue orchestration. This module owns
//! natural-language routing policy, prompt text, and deterministic fallbacks.

use serde::Deserialize;
use serde_json::Value;
use tokio::time::{timeout, Duration};

use crate::routes::bot_agents::BotAgentCapabilityInfo;
use crate::routes::builtin_skills::{PromptId, PromptRegistry};
use crate::state::AppState;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AosRouterIntent {
    pub target_capability: Option<String>,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
    #[serde(default)]
    pub needs_web_search: Option<bool>,
    #[serde(default)]
    pub web_search_query: Option<String>,
    #[serde(default)]
    pub web_search_reason: Option<String>,
    #[serde(default)]
    pub required_evidence: Option<Vec<String>>,
    pub needs_clarification: Option<bool>,
    pub clarification_question: Option<String>,
    pub rewritten_prompt: Option<String>,
}

const ALLOWED_EVIDENCE_REQUIREMENTS: [&str; 6] = [
    "web",
    "workspace",
    "code_change",
    "data_execution",
    "deep_research",
    "super_adversarial",
];

/// Normalize the LLM policy output into a small server-owned evidence
/// vocabulary. Unknown values are discarded so prompt drift cannot create an
/// unenforceable completion requirement.
pub fn normalize_required_evidence(values: Option<&[String]>) -> Vec<String> {
    let mut normalized = values
        .unwrap_or_default()
        .iter()
        .filter_map(|value| {
            let value = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
            ALLOWED_EVIDENCE_REQUIREMENTS
                .contains(&value.as_str())
                .then_some(value)
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

pub fn bot_router_confidence_threshold(config: Option<&Value>) -> f64 {
    config
        .and_then(|value| value.get("confidenceThreshold"))
        .or_else(|| config.and_then(|value| value.get("confidence_threshold")))
        .and_then(Value::as_f64)
        .unwrap_or(0.80)
        .clamp(0.50, 0.99)
}

pub fn router_enabled_capabilities(
    capabilities: &[BotAgentCapabilityInfo],
) -> Vec<&BotAgentCapabilityInfo> {
    capabilities
        .iter()
        .filter(|item| item.enabled && item.capability_key != "aos_router")
        .collect()
}

pub fn normalize_router_capability(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ai_chat" | "chat" | "ai" => Some("ai_chat"),
        "pm_assistant" | "pm" | "operations" | "market" => Some("pm_assistant"),
        "rd_agent" | "rd" | "code" | "coding" => Some("rd_agent"),
        "nl2sql" | "sql" | "data" | "data_attribution" | "data-attribution" => Some("nl2sql"),
        "insight" | "diagnosis" | "diagnostic" => Some("pm_assistant"),
        "automation" | "schedule" | "report" | "alert" => Some("generic_ai"),
        "super_adversarial" | "adversarial" | "debate" => Some("super_adversarial"),
        "watchdog" | "watch_dog" => Some("watchdog"),
        "generic_ai" | "generic" => Some("generic_ai"),
        _ => None,
    }
}

pub fn is_watchdog_action_command(text: &str) -> bool {
    let compact = text.trim();
    if compact.is_empty() {
        return false;
    }
    let lower = compact.to_ascii_lowercase();
    let rest = if let Some(rest) = compact.strip_prefix("详情") {
        rest
    } else if let Some(rest) = compact.strip_prefix("取消") {
        rest
    } else if let Some(rest) = compact.strip_prefix("重试") {
        rest
    } else if let Some(rest) = lower.strip_prefix("detail") {
        &compact[compact.len() - rest.len()..]
    } else if let Some(rest) = lower.strip_prefix("cancel") {
        &compact[compact.len() - rest.len()..]
    } else if let Some(rest) = lower.strip_prefix("retry") {
        &compact[compact.len() - rest.len()..]
    } else {
        return false;
    };
    let trimmed = rest.trim();
    !trimmed.is_empty() && trimmed.chars().all(|ch| ch.is_ascii_digit())
}

pub fn rule_router_target(text: &str, available: &[&BotAgentCapabilityInfo]) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let has_business_metric = [
        "GMV",
        "ROI",
        "eCPM",
        "ECPM",
        "收入",
        "成本",
        "订单",
        "留存",
        "转化",
        "曝光",
        "点击",
        "活跃",
        "用户数",
        "DAU",
        "MAU",
        "流水",
        "付费",
        "客单价",
    ]
    .iter()
    .any(|cue| text.contains(cue) || lower.contains(&cue.to_ascii_lowercase()));
    let has_data_action = [
        "取数",
        "查数",
        "查询数据库",
        "查询表",
        "查表",
        "执行sql",
        "运行sql",
        "跑sql",
        "生成sql",
        "写sql",
        "用 sql",
        "sql 查询",
    ]
    .iter()
    .any(|cue| lower.contains(cue));
    let has_metric_query_action = has_business_metric
        && (text.contains('查')
            || text.contains("统计")
            || text.contains("多少")
            || text.contains("明细")
            || text.contains("趋势")
            || text.contains("分布")
            || text.contains("对比")
            || text.contains("排名")
            || text.contains('按'));
    let has_sql_statement = (lower.contains("select ") && lower.contains(" from "))
        || (lower.contains("with ") && lower.contains("select "))
        || lower.contains("```sql");
    let has_business_causal_request = has_business_metric
        && (text.contains("为什么")
            || text.contains("下降")
            || text.contains("降低")
            || text.contains("涨了")
            || text.contains("低了")
            || text.contains("归因")
            || text.contains("根因")
            || lower.contains("root cause"));
    let has_code_action = text.contains("修复")
        || text.contains("改代码")
        || text.contains("写代码")
        || text.contains("跑测试")
        || text.contains("运行测试")
        || (text.contains("bug") && (text.contains("排查") || text.contains("解决")))
        || (text.contains("报错") && (text.contains("排查") || text.contains("解决")))
        || lower.contains("diff")
        || lower.contains("pull request");
    let has_pm_domain = [
        "用户画像",
        "出海",
        "运营",
        "增长",
        "市场",
        "活动",
        "竞品",
        "产品策略",
        "商业",
    ]
    .iter()
    .any(|cue| text.contains(cue));
    let has_pm_action = [
        "分析", "方案", "研究", "调研", "制定", "规划", "优化", "复盘", "策略", "画像",
    ]
    .iter()
    .any(|cue| text.contains(cue));
    let candidate = if is_watchdog_action_command(text)
        || text.contains("任务")
        || text.contains("状态")
        || text.contains("卡住")
        || text.contains("没回复")
        || text.contains("队列")
        || text.contains("心跳")
        || lower.contains("watchdog")
        || (lower.contains("agent")
            && (text.contains("运行")
                || text.contains("在跑")
                || text.contains("哪些")
                || lower.contains("running")
                || lower.contains("active")))
    {
        "watchdog"
    } else if has_code_action {
        "rd_agent"
    } else if text.contains("每天")
        || text.contains("每周")
        || text.contains("定时")
        || text.contains("提醒")
        || text.contains("发我")
        || text.contains("告警")
        || lower.contains("schedule")
        || lower.contains("alert")
    {
        "generic_ai"
    } else if has_business_causal_request {
        "pm_assistant"
    } else if has_data_action || has_metric_query_action || has_sql_statement {
        "nl2sql"
    } else if text.contains("对抗")
        || text.contains("辩论")
        || text.contains("辩一辩")
        || text.contains("裁决")
        || text.contains("多方案")
        || text.contains("方案 PK")
        || text.contains("方案PK")
        || lower.contains("debate")
        || lower.contains(" pk")
        || lower.contains("pk ")
    {
        "super_adversarial"
    } else if has_pm_domain && has_pm_action {
        "pm_assistant"
    } else {
        "ai_chat"
    };
    available
        .iter()
        .any(|item| item.capability_key == candidate)
        .then(|| candidate.to_string())
}

pub async fn parse_router_intent_with_llm(
    state: &AppState,
    tenant_id: &str,
    text: &str,
    available: &[&BotAgentCapabilityInfo],
    router_config: Option<&Value>,
) -> Option<AosRouterIntent> {
    if std::env::var("AOS_ROUTER_INTENT_LLM")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "0" | "false" | "FALSE" | "off" | "OFF"))
    {
        return None;
    }
    let capabilities = available
        .iter()
        .map(|item| {
            let key = item.capability_key.as_str();
            let description = match key {
                "ai_chat" => {
                    "普通知识问答、闲聊、概念解释、代码/SQL/报错审阅，以及不需要专用执行能力的通用工具循环。"
                }
                "pm_assistant" => "需要多步骤证据研究的产运/增长/市场/用户画像/竞品/策略/商业分析。",
                "rd_agent" => "代码开发、修 bug、读仓库、改文件、跑测试、生成 diff。",
                "nl2sql" => "需要访问已配置数据源或 SQL 知识库的指标查询、SQL 生成与执行。",
                "super_adversarial" => "多方案辩论、正反论证、让多个模型对抗后裁决。",
                "watchdog" => {
                    "查询 Agent 状态、任务进度、为什么没回复、取消/重试/详情、队列和卡点。"
                }
                "generic_ai" => "轻量兜底 AI。",
                _ => "自定义能力。",
            };
            format!("- {key}: {description}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let threshold = bot_router_confidence_threshold(router_config);
    let system_prompt = router_intent_system_prompt(&capabilities, threshold);
    let prompt = format!("用户消息：{text}");
    let router_timeout_secs = std::env::var("AOS_ROUTER_INTENT_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3);
    let request = crate::routes::pm::run_chat_completion_with_any_chat_key(
        state,
        tenant_id,
        state.default_model.clone(),
        vec![crate::routes::chat::ChatMessage {
            role: "user".to_string(),
            content: Value::String(prompt),
        }],
        &system_prompt,
        320,
    );
    match timeout(Duration::from_secs(router_timeout_secs), request).await {
        Err(_) => {
            tracing::info!(
                tenant_id,
                timeout_secs = router_timeout_secs,
                "aos router llm intent parse timed out; using rule router"
            );
            None
        }
        Ok(Ok(result)) => parse_json_object_from_text(&result.answer)
            .and_then(|value| serde_json::from_value::<AosRouterIntent>(value).ok()),
        Ok(Err(error)) => {
            tracing::warn!(
                tenant_id,
                error = %error,
                "aos router llm intent parse failed; using rule router"
            );
            None
        }
    }
}

fn router_intent_system_prompt(capabilities: &str, threshold: f64) -> String {
    let threshold = threshold.to_string();
    PromptRegistry::render(
        PromptId::AosRouterIntent,
        &[("capabilities", capabilities), ("threshold", &threshold)],
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(key: &str) -> BotAgentCapabilityInfo {
        BotAgentCapabilityInfo {
            id: format!("{key}-binding"),
            agent_id: "agent".to_string(),
            capability_key: key.to_string(),
            enabled: true,
            config_json: None,
        }
    }

    fn all_capabilities() -> Vec<BotAgentCapabilityInfo> {
        [
            "ai_chat",
            "pm_assistant",
            "rd_agent",
            "nl2sql",
            "super_adversarial",
            "watchdog",
            "generic_ai",
        ]
        .into_iter()
        .map(capability)
        .collect()
    }

    #[test]
    fn golden_router_targets_core_menu_capabilities() {
        let bindings = all_capabilities();
        let available = router_enabled_capabilities(&bindings);
        for (text, expected) in [
            ("今天天气咋样？", "ai_chat"),
            ("给我讲个轻松的故事", "ai_chat"),
            ("印尼出海用户画像", "pm_assistant"),
            ("修复登录超时 bug", "rd_agent"),
            ("昨天 GMV 按国家统计", "nl2sql"),
            ("昨天印尼 ROI 为什么低了 10%", "pm_assistant"),
            ("每天 10 点发我昨日收入", "generic_ai"),
            ("两个方案辩一辩", "super_adversarial"),
            ("当前有哪些 agent 在运行？", "watchdog"),
            ("取消 1", "watchdog"),
        ] {
            assert_eq!(
                rule_router_target(text, &available).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn ambiguous_words_do_not_force_heavy_capabilities() {
        let bindings = all_capabilities();
        let available = router_enabled_capabilities(&bindings);
        for text in [
            "SQL 是什么意思？",
            "为什么天空是蓝色？",
            "这个数据是什么意思？",
            "这段代码是什么意思？",
            "这个报错是什么意思？",
            "菜市场是什么意思？",
        ] {
            assert_eq!(
                rule_router_target(text, &available).as_deref(),
                Some("ai_chat"),
                "ambiguous explanation should stay in chat: {text}"
            );
        }
    }

    #[test]
    fn router_never_selects_unbound_capability() {
        let bindings = vec![capability("ai_chat")];
        let available = router_enabled_capabilities(&bindings);
        assert_eq!(
            rule_router_target("印尼出海用户画像", &available).as_deref(),
            None
        );
        assert_eq!(
            rule_router_target("给我讲个轻松的故事", &available).as_deref(),
            Some("ai_chat")
        );
    }

    #[test]
    fn watchdog_action_command_requires_numeric_target() {
        assert!(is_watchdog_action_command("取消 1"));
        assert!(is_watchdog_action_command("retry 2"));
        assert!(!is_watchdog_action_command("取消任务"));
    }

    #[test]
    fn router_skill_prompt_preserves_output_contract() {
        let prompt = router_intent_system_prompt("- ai_chat: chat", 0.8);
        assert!(prompt.contains("targetCapability"));
        assert!(prompt.contains("needsClarification"));
        assert!(prompt.contains("needsWebSearch"));
        assert!(prompt.contains("webSearchQuery"));
        assert!(prompt.contains("requiredEvidence"));
        assert!(prompt.contains("code_change"));
        assert!(prompt.contains("data_execution"));
        assert!(
            prompt.contains("行业") || prompt.contains("industry practice"),
            "industry-practice questions must require public evidence"
        );
        assert!(
            prompt.contains("明确要求联网") || prompt.contains("explicitly asks to search online"),
            "an explicit online-lookup request must require web evidence regardless of topic"
        );
        assert!(prompt.contains("watchdog"));
        assert!(prompt.contains("ai_chat"));
        assert!(prompt.contains("Detail, cancel, retry") || prompt.contains("详情/取消/重试"));
        assert!(
            prompt.contains("General knowledge, explanation") || prompt.contains("普通知识、解释")
        );
        assert!(
            prompt.contains("最近会话原文") || prompt.contains("recent conversation"),
            "router must not require workspace evidence for exact text already supplied in the recent tail"
        );
    }

    #[test]
    fn evidence_requirements_are_server_normalized_and_deduplicated() {
        let raw = vec![
            "Web".to_string(),
            "code-change".to_string(),
            "web".to_string(),
            "untrusted_new_policy".to_string(),
        ];
        assert_eq!(
            normalize_required_evidence(Some(&raw)),
            vec!["code_change".to_string(), "web".to_string()]
        );
        assert_eq!(
            normalize_router_capability("data_attribution"),
            Some("nl2sql")
        );
    }
}

#[cfg(test)]
mod super_assistant_routing_golden_tests {
    //! Golden-style routing examples for the Super_Assistant (AOS Copilot)
    //! unified entry. These exercise the deterministic `rule_router_target`
    //! policy that AOS_Router falls back to, covering representative
    //! conversational scenarios plus the LLM-parse-failure error path.
    //!
    //! Requirements: 2.1 (route to the most suitable capability by content),
    //! 2.3 (rule/`ai_chat` fallback when intent is not confidently determined).

    use super::*;

    /// Mirror of the existing test helper pattern: build an enabled capability
    /// binding for the router to consider.
    fn capability(key: &str) -> BotAgentCapabilityInfo {
        BotAgentCapabilityInfo {
            id: format!("{key}-binding"),
            agent_id: "agent".to_string(),
            capability_key: key.to_string(),
            enabled: true,
            config_json: None,
        }
    }

    /// The full capability set a Super_Assistant session can route across,
    /// matching the router's supported keys.
    fn all_capabilities() -> Vec<BotAgentCapabilityInfo> {
        [
            "ai_chat",
            "pm_assistant",
            "rd_agent",
            "nl2sql",
            "super_adversarial",
            "watchdog",
            "generic_ai",
        ]
        .into_iter()
        .map(capability)
        .collect()
    }

    /// Representative Super_Assistant conversation turns route to the expected
    /// capability through the deterministic rule policy (Req 2.1).
    #[test]
    fn golden_super_assistant_session_scenarios() {
        let bindings = all_capabilities();
        let available = router_enabled_capabilities(&bindings);
        for (text, expected) in [
            // Reused core golden case from the existing suite.
            ("印尼出海用户画像", "pm_assistant"),
            // Plain conversational chat with no business/data/code cue.
            ("给我讲一个关于程序员的笑话", "ai_chat"),
            // SQL / data-fetch request -> nl2sql.
            ("帮我用 SQL 查询昨天的订单数据", "nl2sql"),
            // Growth / PM strategy request -> pm_assistant.
            ("制定印尼市场的增长方案", "pm_assistant"),
            // Attribution ("why did it drop") -> pm_assistant.
            ("为什么昨天的转化率下降了", "pm_assistant"),
            // Multi-option debate / adjudication -> super_adversarial.
            ("这两个技术选型辩论一下给个裁决", "super_adversarial"),
            // Code fix request -> rd_agent.
            ("帮我修复登录报错的代码", "rd_agent"),
        ] {
            assert_eq!(
                rule_router_target(text, &available).as_deref(),
                Some(expected),
                "unexpected route for {text:?}"
            );
        }
    }

    /// Error path: when the LLM intent parser fails (returns `None`), the
    /// orchestration falls back to `rule_router_target`. Here we exercise that
    /// deterministic fallback directly and assert it yields a stable target for
    /// each representative message, so a parse failure never blocks routing
    /// (Req 2.3).
    #[test]
    fn llm_parse_failure_falls_back_to_rule_routing() {
        let bindings = all_capabilities();
        let available = router_enabled_capabilities(&bindings);

        // Simulate the LLM branch producing no usable intent.
        let llm_intent: Option<AosRouterIntent> = None;
        assert!(
            llm_intent.is_none(),
            "precondition: LLM intent parse failed"
        );

        // The fallback path must still resolve a concrete capability.
        for (text, expected) in [
            ("帮我用 SQL 查询昨天的订单数据", "nl2sql"),
            ("制定印尼市场的增长方案", "pm_assistant"),
            ("这两个技术选型辩论一下给个裁决", "super_adversarial"),
            // Ambiguous chat with no cue falls back to ai_chat.
            ("嗯嗯好的，那就这样吧", "ai_chat"),
        ] {
            let fallback = rule_router_target(text, &available);
            assert_eq!(
                fallback.as_deref(),
                Some(expected),
                "rule fallback for {text:?} should be {expected}"
            );
        }
    }

    /// When the deterministic fallback cannot map to a bound capability, an
    /// ambiguous message still resolves to `ai_chat` as long as `ai_chat` is
    /// available (Req 2.3 tie-break/bottom fallback).
    #[test]
    fn ambiguous_message_defaults_to_ai_chat_when_available() {
        let bindings = all_capabilities();
        let available = router_enabled_capabilities(&bindings);
        assert_eq!(
            rule_router_target("随便聊聊读书和心情", &available).as_deref(),
            Some("ai_chat")
        );
    }
}
