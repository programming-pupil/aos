use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json_utils::extract_named_json_object;
use crate::report_strategy::pm_is_report_strategy_mode;
use crate::task_graph::sanitize_pm_task_graph_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmTurnClass {
    SimpleChat,
    SimpleAnswer,
    LiveLookup,
    GeneralResearch,
    PmStrategy,
    PmReportStrategy,
}

impl PmTurnClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SimpleChat => "simple_chat",
            Self::SimpleAnswer => "simple_answer",
            Self::LiveLookup => "live_lookup",
            Self::GeneralResearch => "general_research",
            Self::PmStrategy => "pm_strategy",
            Self::PmReportStrategy => "pm_report_strategy",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "simple_chat" | "chat" => Some(Self::SimpleChat),
            "simple_answer" | "direct_answer" | "answer" => Some(Self::SimpleAnswer),
            "live_lookup" | "current_fact" | "fresh_fact" | "lookup" => Some(Self::LiveLookup),
            "general_research" | "research" => Some(Self::GeneralResearch),
            "pm_strategy" | "product_ops_strategy" | "operations_strategy" => {
                Some(Self::PmStrategy)
            }
            "pm_report_strategy" | "business_report_strategy" | "report_strategy" => {
                Some(Self::PmReportStrategy)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmDomainScope {
    General,
    ProductOps,
    Unknown,
}

impl PmDomainScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::ProductOps => "product_ops",
            Self::Unknown => "unknown",
        }
    }

    fn from_str(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "general" | "non_pm" | "non_product_ops" => Self::General,
            "product_ops" | "pm" | "product" | "operations" | "ops" => Self::ProductOps,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmSearchNeed {
    None,
    FreshFact,
    EvidenceAugmented,
    DeepResearch,
}

impl PmSearchNeed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FreshFact => "fresh_fact",
            Self::EvidenceAugmented => "evidence_augmented",
            Self::DeepResearch => "deep_research",
        }
    }

    fn from_str_known(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        let compact = normalized.replace(['-', ' '], "_");
        if matches!(compact.as_str(), "none" | "no_search") {
            return Some(Self::None);
        }
        if matches!(
            compact.as_str(),
            "fresh_fact" | "current_fact" | "live_lookup"
        ) {
            return Some(Self::FreshFact);
        }
        if matches!(compact.as_str(), "deep_research" | "deep") {
            return Some(Self::DeepResearch);
        }
        if matches!(
            compact.as_str(),
            "evidence_augmented" | "search" | "web_search" | "research"
        ) {
            return Some(Self::EvidenceAugmented);
        }
        if compact.contains("no_search")
            || compact.contains("without_search")
            || compact.contains("disabled")
        {
            return Some(Self::None);
        }
        if compact.contains("deep") {
            return Some(Self::DeepResearch);
        }
        if compact.contains("fresh")
            || compact.contains("current")
            || compact.contains("live")
            || compact.contains("real_time")
            || compact.contains("realtime")
        {
            return Some(Self::FreshFact);
        }
        if compact.contains("search")
            || compact.contains("research")
            || compact.contains("evidence")
            || compact.contains("source")
        {
            return Some(Self::EvidenceAugmented);
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmAnswerContract {
    ShortAnswer,
    SourceGroundedAnswer,
    GeneralResearchAnswer,
    PmDecisionPackage,
}

impl PmAnswerContract {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShortAnswer => "short_answer",
            Self::SourceGroundedAnswer => "source_grounded_answer",
            Self::GeneralResearchAnswer => "general_research_answer",
            Self::PmDecisionPackage => "pm_decision_package",
        }
    }

    fn from_str_known(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "short_answer" | "direct_answer" => Some(Self::ShortAnswer),
            "source_grounded_answer" | "lookup_answer" => Some(Self::SourceGroundedAnswer),
            "pm_decision_package" | "strategy_package" => Some(Self::PmDecisionPackage),
            "general_research_answer" | "research_answer" => Some(Self::GeneralResearchAnswer),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmRouteEngine {
    ChatDirect,
    ChatToolLoop,
    AosDeepResearch,
}

impl PmRouteEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatDirect => "chat_direct",
            Self::ChatToolLoop => "chat_tool_loop",
            Self::AosDeepResearch => "aos_deep_research",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "chat_direct" | "direct" | "direct_chat" => Some(Self::ChatDirect),
            "chat_tool_loop" | "tool_loop" | "codex_like_chat" | "chat" => Some(Self::ChatToolLoop),
            "aos_deep_research" | "deep_research" | "pm_deep_research" | "pm_pipeline" => {
                Some(Self::AosDeepResearch)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmSearchPolicy {
    Disabled,
    Allowed,
    Required,
}

impl PmSearchPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Allowed => "allowed",
            Self::Required => "required",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "disabled" | "off" | "none" | "no_search" => Some(Self::Disabled),
            "allowed" | "auto" | "optional" => Some(Self::Allowed),
            "required" | "on" | "must_search" | "fresh" => Some(Self::Required),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmFilePolicy {
    Auto,
    Required,
    Off,
}

impl PmFilePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Required => "required",
            Self::Off => "off",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "allowed" => Some(Self::Auto),
            "required" | "must_use" | "files_required" => Some(Self::Required),
            "off" | "disabled" | "none" => Some(Self::Off),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmReasoningDepth {
    Fast,
    Standard,
    Deep,
}

impl PmReasoningDepth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fast" | "low" | "quick" => Some(Self::Fast),
            "standard" | "balanced" | "normal" => Some(Self::Standard),
            "deep" | "high" | "thorough" => Some(Self::Deep),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmTurnRoute {
    pub engine: PmRouteEngine,
    pub search_policy: PmSearchPolicy,
    pub file_policy: PmFilePolicy,
    pub reasoning_depth: PmReasoningDepth,
    pub turn_class: PmTurnClass,
    pub domain_scope: PmDomainScope,
    pub search_need: PmSearchNeed,
    pub answer_contract: PmAnswerContract,
    pub complexity_score: u8,
    pub reason: String,
}

impl PmTurnRoute {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "engine": self.engine.as_str(),
            "searchPolicy": self.search_policy.as_str(),
            "filePolicy": self.file_policy.as_str(),
            "reasoningDepth": self.reasoning_depth.as_str(),
            "turnClass": self.turn_class.as_str(),
            "domainScope": self.domain_scope.as_str(),
            "searchNeed": self.search_need.as_str(),
            "answerContract": self.answer_contract.as_str(),
            "complexityScore": self.complexity_score,
            "reason": self.reason,
        })
    }

    pub fn is_pm_deep_strategy(self: &Self) -> bool {
        matches!(self.engine, PmRouteEngine::AosDeepResearch)
    }

    pub fn is_lightweight_lookup(self: &Self) -> bool {
        matches!(self.turn_class, PmTurnClass::LiveLookup)
            || (matches!(self.answer_contract, PmAnswerContract::SourceGroundedAnswer)
                && !self.is_pm_deep_strategy())
    }
}

fn legacy_engine_from_route(
    turn_class: PmTurnClass,
    search_need: PmSearchNeed,
    answer_contract: PmAnswerContract,
    complexity_score: u8,
    domain_scope: PmDomainScope,
) -> PmRouteEngine {
    if matches!(answer_contract, PmAnswerContract::PmDecisionPackage)
        || matches!(
            turn_class,
            PmTurnClass::PmStrategy | PmTurnClass::PmReportStrategy
        )
        || matches!(search_need, PmSearchNeed::DeepResearch)
    {
        PmRouteEngine::AosDeepResearch
    } else if matches!(
        search_need,
        PmSearchNeed::FreshFact | PmSearchNeed::EvidenceAugmented
    ) || matches!(
        turn_class,
        PmTurnClass::LiveLookup | PmTurnClass::GeneralResearch
    ) || complexity_score >= 45
        || matches!(domain_scope, PmDomainScope::ProductOps)
    {
        PmRouteEngine::ChatToolLoop
    } else {
        PmRouteEngine::ChatDirect
    }
}

fn legacy_search_policy_from_need(
    search_need: PmSearchNeed,
    engine: PmRouteEngine,
) -> PmSearchPolicy {
    match (engine, search_need) {
        (PmRouteEngine::AosDeepResearch, _) => PmSearchPolicy::Required,
        (_, PmSearchNeed::None) => PmSearchPolicy::Disabled,
        (_, PmSearchNeed::FreshFact | PmSearchNeed::DeepResearch) => PmSearchPolicy::Required,
        (_, PmSearchNeed::EvidenceAugmented) => PmSearchPolicy::Allowed,
    }
}

fn legacy_reasoning_depth(
    engine: PmRouteEngine,
    turn_class: PmTurnClass,
    complexity_score: u8,
) -> PmReasoningDepth {
    if matches!(engine, PmRouteEngine::AosDeepResearch) || complexity_score >= 65 {
        PmReasoningDepth::Deep
    } else if complexity_score <= 15 && matches!(turn_class, PmTurnClass::SimpleChat) {
        PmReasoningDepth::Fast
    } else {
        PmReasoningDepth::Standard
    }
}

fn default_search_need_for_turn_class(turn_class: PmTurnClass) -> PmSearchNeed {
    match turn_class {
        PmTurnClass::SimpleChat | PmTurnClass::SimpleAnswer => PmSearchNeed::None,
        PmTurnClass::LiveLookup => PmSearchNeed::FreshFact,
        PmTurnClass::PmStrategy | PmTurnClass::PmReportStrategy => PmSearchNeed::DeepResearch,
        PmTurnClass::GeneralResearch => PmSearchNeed::EvidenceAugmented,
    }
}

fn default_answer_contract_for_turn_class(turn_class: PmTurnClass) -> PmAnswerContract {
    match turn_class {
        PmTurnClass::SimpleChat | PmTurnClass::SimpleAnswer => PmAnswerContract::ShortAnswer,
        PmTurnClass::LiveLookup => PmAnswerContract::SourceGroundedAnswer,
        PmTurnClass::PmStrategy | PmTurnClass::PmReportStrategy => {
            PmAnswerContract::PmDecisionPackage
        }
        PmTurnClass::GeneralResearch => PmAnswerContract::GeneralResearchAnswer,
    }
}

fn default_complexity_for_turn_class(turn_class: PmTurnClass) -> u8 {
    match turn_class {
        PmTurnClass::SimpleChat => 5,
        PmTurnClass::SimpleAnswer | PmTurnClass::LiveLookup => 20,
        PmTurnClass::GeneralResearch => 55,
        PmTurnClass::PmStrategy | PmTurnClass::PmReportStrategy => 80,
    }
}

fn infer_turn_class_from_route_metadata(
    engine: Option<PmRouteEngine>,
    search_policy: Option<PmSearchPolicy>,
    search_need: Option<PmSearchNeed>,
    answer_contract: Option<PmAnswerContract>,
    domain_scope: PmDomainScope,
) -> PmTurnClass {
    match engine {
        Some(PmRouteEngine::AosDeepResearch) => return PmTurnClass::PmStrategy,
        Some(PmRouteEngine::ChatDirect) => return PmTurnClass::SimpleAnswer,
        Some(PmRouteEngine::ChatToolLoop) => {}
        None => {}
    }

    if matches!(answer_contract, Some(PmAnswerContract::PmDecisionPackage))
        || matches!(search_need, Some(PmSearchNeed::DeepResearch))
    {
        return PmTurnClass::PmStrategy;
    }
    if matches!(search_need, Some(PmSearchNeed::FreshFact))
        || matches!(search_policy, Some(PmSearchPolicy::Required))
    {
        return PmTurnClass::LiveLookup;
    }
    if matches!(search_need, Some(PmSearchNeed::EvidenceAugmented))
        || matches!(search_policy, Some(PmSearchPolicy::Allowed))
        || matches!(domain_scope, PmDomainScope::ProductOps)
    {
        return PmTurnClass::GeneralResearch;
    }
    PmTurnClass::SimpleAnswer
}

pub fn parse_pm_turn_route_from_value(value: &Value) -> Option<PmTurnRoute> {
    let obj = value.as_object()?;
    let engine = obj
        .get("engine")
        .or_else(|| obj.get("routeEngine"))
        .or_else(|| obj.get("route_engine"))
        .and_then(Value::as_str)
        .and_then(PmRouteEngine::from_str);
    let search_policy = obj
        .get("searchPolicy")
        .or_else(|| obj.get("search_policy"))
        .and_then(Value::as_str)
        .and_then(PmSearchPolicy::from_str);
    let domain_scope = obj
        .get("domainScope")
        .or_else(|| obj.get("domain_scope"))
        .and_then(Value::as_str)
        .map(PmDomainScope::from_str)
        .unwrap_or(PmDomainScope::Unknown);
    let raw_search_need = obj
        .get("searchNeed")
        .or_else(|| obj.get("search_need"))
        .and_then(Value::as_str)
        .and_then(PmSearchNeed::from_str_known);
    let raw_answer_contract = obj
        .get("answerContract")
        .or_else(|| obj.get("answer_contract"))
        .and_then(Value::as_str)
        .and_then(PmAnswerContract::from_str_known);
    let mut turn_class = obj
        .get("turnClass")
        .or_else(|| obj.get("turn_class"))
        .and_then(Value::as_str)
        .and_then(PmTurnClass::from_str)
        .unwrap_or_else(|| {
            infer_turn_class_from_route_metadata(
                engine,
                search_policy,
                raw_search_need,
                raw_answer_contract,
                domain_scope,
            )
        });
    let search_need = obj
        .get("searchNeed")
        .or_else(|| obj.get("search_need"))
        .and_then(Value::as_str)
        .and_then(PmSearchNeed::from_str_known)
        .unwrap_or_else(|| default_search_need_for_turn_class(turn_class));
    let answer_contract = obj
        .get("answerContract")
        .or_else(|| obj.get("answer_contract"))
        .and_then(Value::as_str)
        .and_then(PmAnswerContract::from_str_known)
        .unwrap_or_else(|| default_answer_contract_for_turn_class(turn_class));
    let complexity_score = obj
        .get("complexityScore")
        .or_else(|| obj.get("complexity_score"))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_f64().map(|raw| raw.round().max(0.0) as u64))
        })
        .unwrap_or_else(|| default_complexity_for_turn_class(turn_class) as u64)
        .clamp(0, 100) as u8;
    let reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .and_then(|raw| sanitize_pm_task_graph_text(raw, 240))
        .unwrap_or_else(|| "model_routed_turn".to_string());
    let engine = engine.unwrap_or_else(|| {
        legacy_engine_from_route(
            turn_class,
            search_need,
            answer_contract,
            complexity_score,
            domain_scope,
        )
    });
    let mut search_policy =
        search_policy.unwrap_or_else(|| legacy_search_policy_from_need(search_need, engine));
    let file_policy = obj
        .get("filePolicy")
        .or_else(|| obj.get("file_policy"))
        .and_then(Value::as_str)
        .and_then(PmFilePolicy::from_str)
        .unwrap_or(PmFilePolicy::Auto);
    let mut reasoning_depth = obj
        .get("reasoningDepth")
        .or_else(|| obj.get("reasoning_depth"))
        .and_then(Value::as_str)
        .and_then(PmReasoningDepth::from_str)
        .unwrap_or_else(|| legacy_reasoning_depth(engine, turn_class, complexity_score));
    let mut answer_contract = answer_contract;
    let mut search_need = search_need;
    if matches!(engine, PmRouteEngine::AosDeepResearch) {
        if !matches!(
            turn_class,
            PmTurnClass::PmStrategy | PmTurnClass::PmReportStrategy
        ) {
            turn_class = PmTurnClass::PmStrategy;
        }
        search_policy = PmSearchPolicy::Required;
        reasoning_depth = PmReasoningDepth::Deep;
        answer_contract = PmAnswerContract::PmDecisionPackage;
        search_need = PmSearchNeed::DeepResearch;
    } else {
        if matches!(
            turn_class,
            PmTurnClass::PmStrategy | PmTurnClass::PmReportStrategy
        ) {
            turn_class = match engine {
                PmRouteEngine::ChatDirect => PmTurnClass::SimpleAnswer,
                PmRouteEngine::ChatToolLoop
                    if matches!(search_policy, PmSearchPolicy::Required) =>
                {
                    PmTurnClass::LiveLookup
                }
                PmRouteEngine::ChatToolLoop => PmTurnClass::GeneralResearch,
                PmRouteEngine::AosDeepResearch => turn_class,
            };
        }
        if matches!(engine, PmRouteEngine::ChatDirect) {
            search_policy = PmSearchPolicy::Disabled;
            search_need = PmSearchNeed::None;
            answer_contract = PmAnswerContract::ShortAnswer;
        } else {
            if matches!(search_policy, PmSearchPolicy::Disabled) {
                search_need = PmSearchNeed::None;
                if matches!(turn_class, PmTurnClass::LiveLookup) {
                    turn_class = PmTurnClass::SimpleAnswer;
                }
                if matches!(
                    answer_contract,
                    PmAnswerContract::SourceGroundedAnswer | PmAnswerContract::PmDecisionPackage
                ) {
                    answer_contract = PmAnswerContract::ShortAnswer;
                }
            } else if matches!(search_policy, PmSearchPolicy::Required)
                && matches!(search_need, PmSearchNeed::None)
            {
                search_need = PmSearchNeed::FreshFact;
            } else if matches!(search_need, PmSearchNeed::DeepResearch) {
                search_need = PmSearchNeed::EvidenceAugmented;
            }
            if matches!(answer_contract, PmAnswerContract::PmDecisionPackage) {
                answer_contract = match search_need {
                    PmSearchNeed::FreshFact => PmAnswerContract::SourceGroundedAnswer,
                    PmSearchNeed::EvidenceAugmented => PmAnswerContract::GeneralResearchAnswer,
                    PmSearchNeed::None => PmAnswerContract::ShortAnswer,
                    PmSearchNeed::DeepResearch => PmAnswerContract::GeneralResearchAnswer,
                };
            }
            if matches!(search_need, PmSearchNeed::None)
                && matches!(answer_contract, PmAnswerContract::SourceGroundedAnswer)
            {
                answer_contract = PmAnswerContract::ShortAnswer;
            }
        }
    }

    Some(PmTurnRoute {
        engine,
        search_policy,
        file_policy,
        reasoning_depth,
        turn_class,
        domain_scope,
        search_need,
        answer_contract,
        complexity_score,
        reason,
    })
}

pub fn extract_pm_turn_route(text: &str) -> Option<PmTurnRoute> {
    extract_named_json_object(text, "TURN_ROUTE")
        .and_then(|value| parse_pm_turn_route_from_value(&value))
}

pub fn pm_plan_turn_route(plan: &Value) -> Option<PmTurnRoute> {
    plan.get("turnRoute")
        .or_else(|| plan.get("turn_route"))
        .and_then(parse_pm_turn_route_from_value)
}

pub fn apply_pm_turn_route_to_plan(plan: &mut Value, route: &PmTurnRoute) {
    if let Some(obj) = plan.as_object_mut() {
        obj.insert("turnRoute".to_string(), route.to_json());
        if route.is_pm_deep_strategy() {
            obj.insert(
                "answerContract".to_string(),
                Value::String("pm".to_string()),
            );
        }
    }
}

pub fn pm_turn_route_allows_deep_strategy(plan: &Value, question: &str) -> bool {
    let _ = question;
    if pm_is_report_strategy_mode(plan) {
        return true;
    }
    pm_plan_turn_route(plan).is_some_and(|route| route.is_pm_deep_strategy())
}

fn json_string_array_contains(value: Option<&Value>, needles: &[&str]) -> bool {
    value.and_then(Value::as_array).is_some_and(|items| {
        items.iter().filter_map(Value::as_str).any(|raw| {
            let normalized = raw.trim().to_ascii_lowercase();
            needles.iter().any(|needle| normalized == *needle)
        })
    })
}

fn text_contains_any(text: &str, lower: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| {
        if token.is_ascii() {
            lower.contains(&token.to_ascii_lowercase())
        } else {
            text.contains(token)
        }
    })
}

fn count_text_matches(text: &str, lower: &str, tokens: &[&str]) -> usize {
    tokens
        .iter()
        .filter(|token| {
            if token.is_ascii() {
                lower.contains(&token.to_ascii_lowercase())
            } else {
                text.contains(**token)
            }
        })
        .count()
}

fn pm_question_has_professional_strategy_signal(question: &str) -> bool {
    let text = question.trim();
    if text.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    const STRATEGY_TOKENS: &[&str] = &[
        "策略",
        "方案",
        "打法",
        "玩法",
        "实验",
        "灰度",
        "保护指标",
        "停止条件",
        "市场规模",
        "竞品",
        "对标",
        "用户画像",
        "人群",
        "分层",
        "增长",
        "变现",
        "商业化",
        "留存",
        "转化",
        "收入提升",
        "提升收入",
        "降本增效",
        "定价",
        "获客",
        "召回",
        "风控",
        "strategy",
        "playbook",
        "operating plan",
        "growth",
        "monetization",
        "pricing",
        "gtm",
        "go-to-market",
        "competitive",
        "competitor",
        "market sizing",
        "user research",
        "segmentation",
        "retention",
        "conversion",
        "activation",
        "experiment",
        "guardrail",
        "kill criteria",
    ];
    const CONTEXT_TOKENS: &[&str] = &[
        "产品",
        "业务",
        "企业",
        "公司",
        "用户",
        "客户",
        "运营",
        "市场",
        "行业",
        "渠道",
        "收入",
        "成本",
        "利润",
        "转化",
        "留存",
        "投放",
        "买量",
        "广告",
        "平台",
        "app",
        "小程序",
        "电商",
        "门店",
        "医疗",
        "教育",
        "游戏",
        "saas",
        "b2b",
        "b2c",
        "product",
        "business",
        "operations",
        "market",
        "industry",
        "customer",
        "user",
        "revenue",
        "cost",
        "retention",
        "campaign",
        "funnel",
        "platform",
        "startup",
        "enterprise",
    ];
    let strategy_count = count_text_matches(text, &lower, STRATEGY_TOKENS);
    let context_count = count_text_matches(text, &lower, CONTEXT_TOKENS);
    if strategy_count >= 2 && context_count >= 1 {
        return true;
    }
    strategy_count >= 1
        && context_count >= 2
        && text_contains_any(
            text,
            &lower,
            &[
                "深度",
                "全面",
                "专业",
                "决策",
                "报告",
                "directly actionable",
                "decision",
                "report",
            ],
        )
}

fn pm_question_is_self_contained_analysis(question: &str) -> bool {
    let text = question.trim();
    if text.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    text_contains_any(
        text,
        &lower,
        &[
            "sql",
            "select ",
            "create table",
            "group by",
            "join ",
            "where ",
            "字段",
            "建表",
            "查询",
            "分组统计",
            "如下数据",
            "这组数据",
            "附件",
            "csv",
            "表格",
            "对比",
            "汇总",
            "计算",
            "百分比",
            "翻译",
            "translate",
            "summarize this",
        ],
    )
}

fn pm_plan_has_professional_strategy_fallback_signal(
    question: &str,
    plan: &Value,
    decomposition: &str,
    subtask_count: usize,
    complexity: u8,
) -> bool {
    let task_graph = plan.get("taskGraph").unwrap_or(&Value::Null);
    let intent = task_graph
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let has_research_graph = matches!(intent.as_str(), "research" | "decision_support")
        && (decomposition != "none" || subtask_count > 0 || complexity >= 65);
    if !has_research_graph {
        return false;
    }

    let hint = plan.get("reportStrategyHint");
    let hint_matched = hint
        .and_then(|value| value.get("matched"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let hint_score = hint
        .and_then(|value| value.get("score"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let hint_has_strategy = json_string_array_contains(
        hint.and_then(|value| value.get("reasons")),
        &["strategy_request"],
    );
    let hint_has_business_context = json_string_array_contains(
        hint.and_then(|value| value.get("reasons")),
        &[
            "specific_business_context",
            "explicit_user_segmentation",
            "rich_metric_context",
            "dense_first_party_numbers",
            "prior_experiment_context",
            "structured_report",
        ],
    );

    if hint_matched || (hint_has_strategy && hint_has_business_context && hint_score >= 3) {
        return true;
    }

    pm_question_has_professional_strategy_signal(question)
        && !pm_question_is_self_contained_analysis(question)
}

pub fn build_pm_fallback_turn_route(question: &str, plan: &Value) -> PmTurnRoute {
    if pm_is_report_strategy_mode(plan) {
        return PmTurnRoute {
            engine: PmRouteEngine::AosDeepResearch,
            search_policy: PmSearchPolicy::Required,
            file_policy: PmFilePolicy::Auto,
            reasoning_depth: PmReasoningDepth::Deep,
            turn_class: PmTurnClass::PmReportStrategy,
            domain_scope: PmDomainScope::ProductOps,
            search_need: PmSearchNeed::DeepResearch,
            answer_contract: PmAnswerContract::PmDecisionPackage,
            complexity_score: 85,
            reason: "first-party business report strategy signal detected".to_string(),
        };
    }
    let task_graph = plan.get("taskGraph").unwrap_or(&Value::Null);
    let decomposition = task_graph
        .get("decompositionMode")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let subtask_count = task_graph
        .get("subtasks")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let complexity = task_graph
        .get("complexityScore")
        .and_then(Value::as_u64)
        .unwrap_or(40)
        .clamp(0, 100) as u8;
    let requires_search =
        crate::route_plan::pm_question_likely_requires_external_evidence(question);
    if decomposition == "none" && subtask_count == 0 {
        return PmTurnRoute {
            engine: PmRouteEngine::ChatToolLoop,
            search_policy: if requires_search {
                PmSearchPolicy::Required
            } else {
                PmSearchPolicy::Disabled
            },
            file_policy: PmFilePolicy::Auto,
            reasoning_depth: if complexity >= 60 {
                PmReasoningDepth::Deep
            } else {
                PmReasoningDepth::Standard
            },
            turn_class: PmTurnClass::SimpleAnswer,
            domain_scope: PmDomainScope::Unknown,
            search_need: if requires_search {
                PmSearchNeed::FreshFact
            } else {
                PmSearchNeed::None
            },
            answer_contract: PmAnswerContract::ShortAnswer,
            complexity_score: complexity.min(40),
            reason: "fallback route to shared chat tool loop from non-decomposed task graph"
                .to_string(),
        };
    }
    if pm_plan_has_professional_strategy_fallback_signal(
        question,
        plan,
        decomposition,
        subtask_count,
        complexity,
    ) {
        return PmTurnRoute {
            engine: PmRouteEngine::AosDeepResearch,
            search_policy: PmSearchPolicy::Required,
            file_policy: PmFilePolicy::Auto,
            reasoning_depth: PmReasoningDepth::Deep,
            turn_class: PmTurnClass::PmStrategy,
            domain_scope: PmDomainScope::ProductOps,
            search_need: PmSearchNeed::DeepResearch,
            answer_contract: PmAnswerContract::PmDecisionPackage,
            complexity_score: complexity.max(75),
            reason: "fallback route to AOS deep research from research task graph plus professional strategy signal".to_string(),
        };
    }
    if requires_search {
        return PmTurnRoute {
            engine: PmRouteEngine::ChatToolLoop,
            search_policy: PmSearchPolicy::Required,
            file_policy: PmFilePolicy::Auto,
            reasoning_depth: if complexity >= 65 {
                PmReasoningDepth::Deep
            } else {
                PmReasoningDepth::Standard
            },
            turn_class: PmTurnClass::LiveLookup,
            domain_scope: PmDomainScope::General,
            search_need: PmSearchNeed::FreshFact,
            answer_contract: PmAnswerContract::SourceGroundedAnswer,
            complexity_score: complexity.min(60),
            reason: "fallback route to shared chat tool loop for external factual lookup"
                .to_string(),
        };
    }
    if pm_question_is_self_contained_analysis(question) {
        return PmTurnRoute {
            engine: PmRouteEngine::ChatToolLoop,
            search_policy: PmSearchPolicy::Disabled,
            file_policy: PmFilePolicy::Auto,
            reasoning_depth: if complexity >= 60 {
                PmReasoningDepth::Deep
            } else {
                PmReasoningDepth::Standard
            },
            turn_class: PmTurnClass::SimpleAnswer,
            domain_scope: PmDomainScope::General,
            search_need: PmSearchNeed::None,
            answer_contract: PmAnswerContract::ShortAnswer,
            complexity_score: complexity.min(60),
            reason: "fallback route to shared chat tool loop for self-contained analysis"
                .to_string(),
        };
    }
    PmTurnRoute {
        engine: PmRouteEngine::ChatToolLoop,
        search_policy: PmSearchPolicy::Allowed,
        file_policy: PmFilePolicy::Auto,
        reasoning_depth: if complexity >= 65 {
            PmReasoningDepth::Deep
        } else {
            PmReasoningDepth::Standard
        },
        turn_class: PmTurnClass::GeneralResearch,
        domain_scope: PmDomainScope::Unknown,
        search_need: PmSearchNeed::EvidenceAugmented,
        answer_contract: PmAnswerContract::GeneralResearchAnswer,
        complexity_score: complexity,
        reason: "fallback route from research task graph without product/ops strategy signal"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(
        turn_class: PmTurnClass,
        domain_scope: PmDomainScope,
        search_need: PmSearchNeed,
        answer_contract: PmAnswerContract,
        complexity_score: u8,
    ) -> PmTurnRoute {
        let engine = legacy_engine_from_route(
            turn_class,
            search_need,
            answer_contract,
            complexity_score,
            domain_scope,
        );
        PmTurnRoute {
            engine,
            search_policy: legacy_search_policy_from_need(search_need, engine),
            file_policy: PmFilePolicy::Auto,
            reasoning_depth: legacy_reasoning_depth(engine, turn_class, complexity_score),
            turn_class,
            domain_scope,
            search_need,
            answer_contract,
            complexity_score,
            reason: "test".to_string(),
        }
    }

    #[test]
    fn parses_turn_route_block() {
        let text = r#"TURN_ROUTE {"engine":"chat_tool_loop","searchPolicy":"required","filePolicy":"auto","reasoningDepth":"standard","turnClass":"live_lookup","domainScope":"general","searchNeed":"fresh_fact","answerContract":"source_grounded_answer","complexityScore":18,"reason":"current fact"}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
        assert_eq!(route.turn_class, PmTurnClass::LiveLookup);
        assert!(route.is_lightweight_lookup());
        assert!(!route.is_pm_deep_strategy());
    }

    #[test]
    fn parses_wrapped_turn_route_object_from_model_output() {
        let text = r#"{
  "TURN_ROUTE": {
    "engine": "chat_tool_loop",
    "searchPolicy": "required",
    "filePolicy": "off",
    "reasoningDepth": "standard",
    "turnClass": "live_lookup",
    "domainScope": "general",
    "searchNeed": "fresh_fact",
    "answerContract": "source_grounded_answer",
    "complexityScore": 4,
    "reason": "current public lookup"
  }
}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
        assert_eq!(route.turn_class, PmTurnClass::LiveLookup);
        assert_eq!(route.search_need, PmSearchNeed::FreshFact);
    }

    #[test]
    fn parses_chat_tool_loop_for_file_or_data_analysis() {
        let text = r#"TURN_ROUTE {"engine":"chat_tool_loop","searchPolicy":"disabled","filePolicy":"required","reasoningDepth":"deep","turnClass":"simple_answer","domainScope":"general","searchNeed":"none","answerContract":"short_answer","complexityScore":45,"reason":"use attached/user data in the shared chat loop"}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.file_policy, PmFilePolicy::Required);
        assert_eq!(route.reasoning_depth, PmReasoningDepth::Deep);
        assert_eq!(route.turn_class, PmTurnClass::SimpleAnswer);
        assert_eq!(route.search_need, PmSearchNeed::None);
        assert!(!route.is_pm_deep_strategy());
    }

    #[test]
    fn aos_deep_research_route_is_normalized_to_required_deep_decision_package() {
        let text = r#"TURN_ROUTE {"engine":"aos_deep_research","searchPolicy":"allowed","filePolicy":"auto","reasoningDepth":"standard","turnClass":"pm_strategy","domainScope":"product_ops","searchNeed":"evidence_augmented","answerContract":"general_research_answer","complexityScore":78,"reason":"professional strategy research"}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::AosDeepResearch);
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
        assert_eq!(route.reasoning_depth, PmReasoningDepth::Deep);
        assert_eq!(route.search_need, PmSearchNeed::DeepResearch);
        assert_eq!(route.answer_contract, PmAnswerContract::PmDecisionPackage);
        assert!(route.is_pm_deep_strategy());
    }

    #[test]
    fn invalid_turn_class_with_chat_direct_engine_still_parses_as_direct_chat() {
        let text = r#"TURN_ROUTE {"engine":"chat_direct","searchPolicy":"disabled","filePolicy":"auto","reasoningDepth":"fast","turnClass":"greeting","domainScope":"general","searchNeed":"none","answerContract":"short_answer","complexityScore":3,"reason":"stable greeting or translation"}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::ChatDirect);
        assert_eq!(route.turn_class, PmTurnClass::SimpleAnswer);
        assert_eq!(route.search_policy, PmSearchPolicy::Disabled);
        assert_eq!(route.search_need, PmSearchNeed::None);
        assert_eq!(route.answer_contract, PmAnswerContract::ShortAnswer);
        assert!(!route.is_pm_deep_strategy());
    }

    #[test]
    fn invalid_turn_class_with_required_tool_loop_infers_live_lookup() {
        let text = r#"TURN_ROUTE {"engine":"chat_tool_loop","searchPolicy":"required","filePolicy":"off","reasoningDepth":"standard","turnClass":"current_public_fact_lookup","domainScope":"general","searchNeed":"current_weather_lookup","answerContract":"source_grounded_answer","complexityScore":18,"reason":"current public fact requires latest sources"}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.turn_class, PmTurnClass::LiveLookup);
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
        assert_eq!(route.search_need, PmSearchNeed::FreshFact);
        assert_eq!(
            route.answer_contract,
            PmAnswerContract::SourceGroundedAnswer
        );
        assert!(route.is_lightweight_lookup());
        assert!(!route.is_pm_deep_strategy());
    }

    #[test]
    fn disabled_search_tool_loop_drops_source_grounded_contract() {
        let text = r#"TURN_ROUTE {"engine":"chat_tool_loop","searchPolicy":"disabled","filePolicy":"auto","reasoningDepth":"standard","turnClass":"simple_answer","domainScope":"product_ops","searchNeed":"none","answerContract":"source_grounded_answer","complexityScore":42,"reason":"self-contained data analysis"}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.search_policy, PmSearchPolicy::Disabled);
        assert_eq!(route.search_need, PmSearchNeed::None);
        assert_eq!(route.answer_contract, PmAnswerContract::ShortAnswer);
        assert_eq!(route.turn_class, PmTurnClass::SimpleAnswer);
        assert!(!route.is_lightweight_lookup());
    }

    #[test]
    fn aos_deep_research_engine_overrides_invalid_compatibility_metadata() {
        let text = r#"TURN_ROUTE {"engine":"aos_deep_research","searchPolicy":"allowed","filePolicy":"auto","reasoningDepth":"standard","turnClass":"market_strategy_deliverable","domainScope":"product_ops","searchNeed":"competitive_strategy","answerContract":"research_answer","complexityScore":82,"reason":"professional product and market strategy"}"#;
        let route = extract_pm_turn_route(text).expect("route");
        assert_eq!(route.engine, PmRouteEngine::AosDeepResearch);
        assert_eq!(route.turn_class, PmTurnClass::PmStrategy);
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
        assert_eq!(route.reasoning_depth, PmReasoningDepth::Deep);
        assert_eq!(route.search_need, PmSearchNeed::DeepResearch);
        assert_eq!(route.answer_contract, PmAnswerContract::PmDecisionPackage);
        assert!(route.is_pm_deep_strategy());
    }

    #[test]
    fn report_strategy_allows_deep_strategy() {
        let plan = serde_json::json!({"mode": "business_report_strategy"});
        assert!(pm_turn_route_allows_deep_strategy(
            &plan,
            "produce a strategy"
        ));
    }

    #[test]
    fn live_lookup_route_blocks_pm_strategy_package() {
        let mut plan = serde_json::json!({
            "mode": "auto",
            "taskGraph": {"intent": "research", "complexityScore": 80, "decompositionMode": "light", "subtasks": []}
        });
        let route = route(
            PmTurnClass::LiveLookup,
            PmDomainScope::General,
            PmSearchNeed::FreshFact,
            PmAnswerContract::SourceGroundedAnswer,
            20,
        );
        apply_pm_turn_route_to_plan(&mut plan, &route);
        assert!(!pm_turn_route_allows_deep_strategy(&plan, "查一下实时信息"));
    }

    #[test]
    fn fallback_route_for_non_decomposed_graph_remains_direct_only_for_stable_question() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "chat",
                "complexityScore": 5,
                "decompositionMode": "none",
                "subtasks": []
            }
        });
        let route = build_pm_fallback_turn_route("ROI 是什么意思", &plan);
        assert_eq!(route.turn_class, PmTurnClass::SimpleAnswer);
        assert_eq!(route.search_need, PmSearchNeed::None);
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
    }

    #[test]
    fn fallback_route_for_non_decomposed_analysis_uses_shared_chat_loop() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "analysis",
                "complexityScore": 40,
                "decompositionMode": "none",
                "subtasks": []
            }
        });
        let route = build_pm_fallback_turn_route(
            "我给你一组实验数据，请对比AIPU、ECPM、收入、成本、ROI，并用表格输出百分比。",
            &plan,
        );
        assert_eq!(route.turn_class, PmTurnClass::SimpleAnswer);
        assert_eq!(route.search_need, PmSearchNeed::None);
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.answer_contract, PmAnswerContract::ShortAnswer);
    }

    #[test]
    fn fallback_route_for_strategy_research_graph_uses_aos_deep_research() {
        let plan = serde_json::json!({
            "reportStrategyHint": {
                "advisory": true,
                "matched": false,
                "score": 4,
                "reasons": ["strategy_request", "specific_business_context"],
                "primaryTerms": []
            },
            "taskGraph": {
                "intent": "research",
                "complexityScore": 68,
                "decompositionMode": "light",
                "subtasks": [
                    {
                        "id": "cases",
                        "title": "可比案例",
                        "goal": "寻找可借鉴机制",
                        "queries": ["comparable product growth mechanism"],
                        "deliverable": "机制启发",
                        "requiredEvidenceType": "external",
                        "priority": "high"
                    }
                ]
            }
        });
        let route = build_pm_fallback_turn_route(
            "我们是一个新业务，想看用户画像、市场规模、竞品分析和增长玩法策略。",
            &plan,
        );
        assert_eq!(route.engine, PmRouteEngine::AosDeepResearch);
        assert_eq!(route.turn_class, PmTurnClass::PmStrategy);
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
        assert_eq!(route.search_need, PmSearchNeed::DeepResearch);
        assert_eq!(route.answer_contract, PmAnswerContract::PmDecisionPackage);
    }

    #[test]
    fn fallback_route_for_data_or_sql_graph_stays_shared_chat_loop() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "analysis",
                "complexityScore": 70,
                "decompositionMode": "light",
                "subtasks": [
                    {
                        "id": "sql",
                        "title": "SQL改写",
                        "goal": "按字段分组统计",
                        "queries": [],
                        "deliverable": "SQL",
                        "requiredEvidenceType": "first_party",
                        "priority": "high"
                    }
                ]
            }
        });
        let route = build_pm_fallback_turn_route(
            "表结构是 create table orders(id bigint, channel varchar(32), roi double); 给我写SQL按channel分组统计roi。",
            &plan,
        );
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.search_policy, PmSearchPolicy::Disabled);
        assert_eq!(route.search_need, PmSearchNeed::None);
        assert_eq!(route.answer_contract, PmAnswerContract::ShortAnswer);
    }

    #[test]
    fn fallback_route_for_live_lookup_stays_shared_chat_tool_loop() {
        let plan = serde_json::json!({
            "taskGraph": {
                "intent": "research",
                "complexityScore": 55,
                "decompositionMode": "light",
                "subtasks": [
                    {
                        "id": "weather",
                        "title": "实时天气",
                        "goal": "查询城市天气",
                        "queries": ["北京 上海 天津 天气"],
                        "deliverable": "天气结论",
                        "requiredEvidenceType": "external",
                        "priority": "high"
                    }
                ]
            }
        });
        let route = build_pm_fallback_turn_route("北京和上海天气咋样？天津呢", &plan);
        assert_eq!(route.engine, PmRouteEngine::ChatToolLoop);
        assert_eq!(route.turn_class, PmTurnClass::LiveLookup);
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
        assert_eq!(route.search_need, PmSearchNeed::FreshFact);
    }

    #[test]
    fn pm_strategy_route_allows_pm_strategy_package() {
        let mut plan = serde_json::json!({"mode": "auto"});
        let route = route(
            PmTurnClass::PmStrategy,
            PmDomainScope::ProductOps,
            PmSearchNeed::DeepResearch,
            PmAnswerContract::PmDecisionPackage,
            82,
        );
        apply_pm_turn_route_to_plan(&mut plan, &route);
        assert!(pm_turn_route_allows_deep_strategy(
            &plan,
            "给我产品运营策略"
        ));
    }
}
