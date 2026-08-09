use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EffectiveChatSearchMode {
    On,
    Off,
    Unavailable,
}

impl EffectiveChatSearchMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ChatTurnPlan {
    pub(super) options: agent_gateway::AgentTurnOptions,
    pub(super) search_mode: EffectiveChatSearchMode,
    pub(super) reasoning_budget: agent_gateway::InternalReasoningBudget,
    pub(super) trace: Vec<serde_json::Value>,
}

pub(super) struct ChatTurnEngineInput<'a> {
    pub(super) state: &'a AppState,
    pub(super) tenant_id: &'a str,
    pub(super) user_id: &'a str,
    pub(super) session_id: &'a str,
    pub(super) model: &'a str,
    pub(super) message: &'a str,
    pub(super) turn_options: ChatTurnOptions,
    pub(super) memory_instructions: Vec<String>,
    pub(super) has_documents: bool,
    pub(super) mark_memory_pollution: bool,
    pub(super) reasoning_budget_override: Option<agent_gateway::InternalReasoningBudget>,
}

struct ChatSearchAvailability {
    any_available: bool,
    explicit_search_available: bool,
}

impl ChatSearchAvailability {
    fn prefer_native_search(&self) -> bool {
        !self.explicit_search_available
    }

    fn suppress_native_search(&self) -> bool {
        self.explicit_search_available
    }
}

async fn chat_search_availability(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> ChatSearchAvailability {
    let snapshot =
        crate::routes::search_orchestrator_runtime::build_unified_search_capability_snapshot(
            state,
            tenant_id,
            Some(model),
            false,
            true,
        )
        .await;
    let configured_search_available = snapshot
        .configured_providers
        .iter()
        .any(|provider| provider.enabled && provider.health_status != "unhealthy");
    let explicit_search_available = configured_search_available || snapshot.mcp_search.available;
    ChatSearchAvailability {
        any_available: explicit_search_available || snapshot.native_search.available,
        explicit_search_available,
    }
}

pub(super) async fn resolve_effective_chat_search_mode(
    state: &AppState,
    tenant_id: &str,
    model: &str,
    options: &ChatTurnOptions,
) -> EffectiveChatSearchMode {
    if !options.search_enabled && !matches!(options.search_mode, ChatSearchMode::On) {
        return EffectiveChatSearchMode::Off;
    }
    if !chat_search_availability(state, tenant_id, model)
        .await
        .any_available
    {
        return EffectiveChatSearchMode::Unavailable;
    }
    EffectiveChatSearchMode::On
}

pub(super) fn chat_search_instruction() -> String {
    "Web search is enabled for this turn. Follow AOS Search Orchestrator order: use configured Search Extension/WebSearch providers first when they are healthy; if none are configured or they fail, use model-native streaming Responses web_search when available; then MCP search/browser/fetch; then local/RAG evidence. Search before answering when current/public facts are needed. Cite sources with URLs in the final answer. If search fails or sources are insufficient, say so clearly and do not present unsupported claims as searched facts.".to_string()
}

pub(super) fn chat_no_search_instruction() -> String {
    "Web search is disabled for this turn. Do not use WebSearch, WebFetch, or MCP browser/search/fetch tools. If the user asks for current information, answer from stable knowledge only and clearly say that live search was not used.".to_string()
}

pub(super) fn chat_search_unavailable_instruction() -> String {
    "The user requested or the planner selected web search, but no configured Search Extension, model-native search, or MCP search/browser/fetch tool is available. Do not pretend to have searched. Give the best stable answer you can and clearly state that live search was unavailable.".to_string()
}

pub(super) fn chat_file_grounding_instruction(
    options: &ChatTurnOptions,
    has_documents: bool,
) -> Option<String> {
    let file_context = options.file_context.as_ref()?;
    if file_context.mode == ChatFileContextMode::None && !has_documents {
        return None;
    }
    let selected_count = file_context.file_ids.len();
    let scope = match file_context.mode {
        ChatFileContextMode::None => "attached files",
        ChatFileContextMode::Selected => "selected files",
        ChatFileContextMode::AllAttached => "all attached files",
        ChatFileContextMode::Workspace => "chat file workspace",
    };
    let strict = if file_context.strict_grounding {
        "Strict file grounding is enabled: answer only from the provided file context. If the answer is not supported by the file excerpts, say the evidence is insufficient."
    } else {
        "Use provided file context as primary evidence. If you use general knowledge, clearly separate it from file-supported facts."
    };
    Some(format!(
        "{strict} The active file scope is {scope}; selected file id count: {selected_count}. Cite filenames or visible snippets when possible."
    ))
}

pub(super) fn chat_reasoning_budget(
    message: &str,
    options: &ChatTurnOptions,
    has_documents: bool,
) -> agent_gateway::InternalReasoningBudget {
    let text = message.trim();
    let lower = text.to_ascii_lowercase();
    let char_count = text.chars().count();
    let file_context_active = has_documents
        || options
            .file_context
            .as_ref()
            .is_some_and(|file_context| file_context.mode != ChatFileContextMode::None);

    if matches!(options.search_mode, ChatSearchMode::On)
        || options.search_enabled
        || file_context_active
        || char_count > 700
    {
        return agent_gateway::InternalReasoningBudget::Deep;
    }

    let deep_needles = [
        "plan",
        "方案",
        "设计",
        "架构",
        "代码",
        "bug",
        "debug",
        "review",
        "对比",
        "比较",
        "复盘",
        "分析",
        "推理",
        "why",
        "为什么",
        "原因",
        "tradeoff",
        "risk",
        "风险",
        "implement",
        "实现",
        "diagnose",
        "诊断",
        "总结这篇",
        "长文",
    ];
    if deep_needles.iter().any(|needle| lower.contains(needle)) {
        return agent_gateway::InternalReasoningBudget::Deep;
    }

    let fast_needles = [
        "hi",
        "hello",
        "hey",
        "thanks",
        "thank you",
        "谢谢",
        "你好",
        "翻译",
        "translate",
    ];
    if char_count <= 80 || fast_needles.iter().any(|needle| lower.contains(needle)) {
        return agent_gateway::InternalReasoningBudget::Fast;
    }

    agent_gateway::InternalReasoningBudget::Standard
}

pub(super) fn chat_reasoning_budget_instruction(
    budget: agent_gateway::InternalReasoningBudget,
) -> String {
    match budget {
        agent_gateway::InternalReasoningBudget::Fast => {
            "Use a low-latency answer strategy for this turn: answer directly and concisely, while staying accurate. Do not expose hidden chain-of-thought.".to_string()
        }
        agent_gateway::InternalReasoningBudget::Standard => {
            "Use a balanced answer strategy for this turn: analyze enough to be reliable, then provide a clear concise answer. Do not expose hidden chain-of-thought.".to_string()
        }
        agent_gateway::InternalReasoningBudget::Deep => {
            "Use a deep answer strategy for this turn: decompose the problem, verify assumptions against available evidence, compare alternatives when useful, and provide a well-supported final answer. Do not expose hidden chain-of-thought; summarize reasoning at a high level only.".to_string()
        }
    }
}

pub(super) async fn plan_chat_turn(input: ChatTurnEngineInput<'_>) -> ChatTurnPlan {
    let mut options = agent_gateway::AgentTurnOptions::default();
    for instruction in input.memory_instructions {
        options.system_instructions.push(instruction);
    }
    let search_mode = resolve_effective_chat_search_mode(
        input.state,
        input.tenant_id,
        input.model,
        &input.turn_options,
    )
    .await;
    let reasoning_budget = input.reasoning_budget_override.unwrap_or_else(|| {
        chat_reasoning_budget(input.message, &input.turn_options, input.has_documents)
    });
    options.reasoning_budget = reasoning_budget;
    options
        .system_instructions
        .push(chat_reasoning_budget_instruction(reasoning_budget));
    let mut trace = vec![serde_json::json!({
        "event": "chat.turn.planned",
        "searchMode": search_mode.as_str(),
        "reasoningBudget": format!("{reasoning_budget:?}").to_ascii_lowercase(),
        "documentCount": if input.has_documents { 1 } else { 0 },
    })];

    if matches!(search_mode, EffectiveChatSearchMode::On) {
        let search_availability =
            chat_search_availability(input.state, input.tenant_id, input.model).await;
        options.prefer_native_web_search = search_availability.prefer_native_search();
        options.suppress_native_web_search = search_availability.suppress_native_search();
        trace.push(serde_json::json!({
            "event": "chat.search.order.selected",
            "explicitSearchAvailable": search_availability.explicit_search_available,
            "nativeWebSearchPreferred": options.prefer_native_web_search,
            "nativeWebSearchSuppressed": options.suppress_native_web_search,
            "order": serde_json::json!(["configured_search_provider_when_healthy", "native_model_streaming_when_no_provider_or_provider_failed", "mcp", "rag_local"]),
        }));
        if input.mark_memory_pollution {
            crate::routes::memory_continuity::mark_thread_memory_polluted(
                &input.state.db,
                input.tenant_id,
                input.user_id,
                input.session_id,
                "web_search_context",
                Some(serde_json::json!({ "searchMode": search_mode.as_str() })),
            )
            .await;
        }
        options.system_instructions.push(chat_search_instruction());
    } else if matches!(search_mode, EffectiveChatSearchMode::Unavailable) {
        options.blocked_tools.extend([
            "WebSearch".to_string(),
            "WebFetch".to_string(),
            agent_gateway::runtime_builder::CHAT_BLOCK_MCP_SEARCH_TOOLS.to_string(),
        ]);
        options
            .system_instructions
            .push(chat_search_unavailable_instruction());
        trace.push(serde_json::json!({
            "event": "chat.search.unavailable",
            "fallback": "stable_knowledge",
        }));
    } else {
        options.blocked_tools.extend([
            "WebSearch".to_string(),
            "WebFetch".to_string(),
            agent_gateway::runtime_builder::CHAT_BLOCK_MCP_SEARCH_TOOLS.to_string(),
        ]);
        options
            .system_instructions
            .push(chat_no_search_instruction());
    }

    if let Some(instruction) =
        chat_file_grounding_instruction(&input.turn_options, input.has_documents)
    {
        options.system_instructions.push(instruction);
    }

    ChatTurnPlan {
        options,
        search_mode,
        reasoning_budget,
        trace,
    }
}

pub(super) fn chat_memory_mode_str(mode: &ChatMemoryMode) -> &'static str {
    match mode {
        ChatMemoryMode::Auto => "auto",
        ChatMemoryMode::Off => "off",
        ChatMemoryMode::PinnedOnly => "pinned_only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(search_enabled: bool, mode: ChatFileContextMode) -> ChatTurnOptions {
        ChatTurnOptions {
            search_mode: if search_enabled {
                ChatSearchMode::On
            } else {
                ChatSearchMode::Off
            },
            search_enabled,
            file_context: Some(ChatFileContextOptions {
                mode,
                file_ids: vec![],
                strict_grounding: false,
            }),
            memory_mode: ChatMemoryMode::Off,
        }
    }

    #[test]
    fn chat_reasoning_budget_uses_fast_for_short_simple_turns() {
        assert_eq!(
            chat_reasoning_budget("你好", &opts(false, ChatFileContextMode::None), false),
            agent_gateway::InternalReasoningBudget::Fast
        );
    }

    #[test]
    fn chat_reasoning_budget_uses_deep_for_search_or_files() {
        assert_eq!(
            chat_reasoning_budget(
                "查一下今天的公开信息",
                &opts(true, ChatFileContextMode::None),
                false
            ),
            agent_gateway::InternalReasoningBudget::Deep
        );
        assert_eq!(
            chat_reasoning_budget(
                "总结这些文件",
                &opts(false, ChatFileContextMode::AllAttached),
                true
            ),
            agent_gateway::InternalReasoningBudget::Deep
        );
    }

    #[test]
    fn chat_reasoning_budget_uses_deep_for_complex_analysis() {
        assert_eq!(
            chat_reasoning_budget(
                "帮我分析这个方案的风险和 tradeoff",
                &opts(false, ChatFileContextMode::None),
                false
            ),
            agent_gateway::InternalReasoningBudget::Deep
        );
    }

    #[test]
    fn chat_search_instructions_match_manual_search_contract() {
        let on = chat_search_instruction();
        assert!(on.contains("Search Orchestrator order"));
        assert!(on.contains("configured Search Extension"));
        assert!(on.contains("model-native streaming Responses web_search"));
        assert!(on.contains("configured Search Extension"));
        assert!(on.contains("Cite sources"));

        let off = chat_no_search_instruction();
        assert!(off.contains("Web search is disabled"));
        assert!(off.contains("Do not use WebSearch"));
    }

    #[test]
    fn chat_search_flags_prefer_provider_before_native_when_explicit_search_exists() {
        let explicit = ChatSearchAvailability {
            any_available: true,
            explicit_search_available: true,
        };
        let native_only = ChatSearchAvailability {
            any_available: true,
            explicit_search_available: false,
        };

        assert!(!explicit.prefer_native_search());
        assert!(explicit.suppress_native_search());
        assert!(native_only.prefer_native_search());
        assert!(!native_only.suppress_native_search());
    }
}
