//! Repo-shipped skill contracts for AOS core agent behavior.
//!
//! These files are intentionally used only for prompt/strategy contracts. Core
//! execution concerns such as DB writes, runtime cancellation, permissions,
//! queues, SQL safety, and AgentOps actions must stay in Rust code.

pub const SKILL_AOS_ROUTER: &str = "aos-router";
pub const SKILL_WATCHDOG: &str = "watchdog";
pub const SKILL_CODE_STUDIO_CODE: &str = "code-studio-code";
pub const SKILL_CODE_STUDIO_SPEC: &str = "code-studio-spec";
pub const SKILL_NL2SQL_REFERENCE: &str = "nl2sql-reference";
pub const SKILL_PM_ASSISTANT: &str = "pm-assistant";
pub const SKILL_SUPER_ADVERSARIAL: &str = "super-adversarial";

const SECTION_CLOSE: &str = "<!-- /aos:section -->";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PromptId {
    AosRouterIntent,
    WatchdogIntent,
    CodeStudioSpecGenerateSpec,
    CodeStudioSpecGenerateDesign,
    CodeStudioSpecGenerateTasks,
    CodeStudioSpecImplementTask,
    CodeStudioSpecFinalReport,
    SuperAdversarialInitial,
    SuperAdversarialReview,
    SuperAdversarialJudge,
    SuperAdversarialFinal,
}

impl PromptId {
    pub fn skill_name(self) -> &'static str {
        match self {
            Self::AosRouterIntent => SKILL_AOS_ROUTER,
            Self::WatchdogIntent => SKILL_WATCHDOG,
            Self::CodeStudioSpecGenerateSpec
            | Self::CodeStudioSpecGenerateDesign
            | Self::CodeStudioSpecGenerateTasks
            | Self::CodeStudioSpecImplementTask
            | Self::CodeStudioSpecFinalReport => SKILL_CODE_STUDIO_SPEC,
            Self::SuperAdversarialInitial
            | Self::SuperAdversarialReview
            | Self::SuperAdversarialJudge
            | Self::SuperAdversarialFinal => SKILL_SUPER_ADVERSARIAL,
        }
    }

    pub fn section(self) -> &'static str {
        match self {
            Self::AosRouterIntent => "router-intent",
            Self::WatchdogIntent => "watchdog-intent",
            Self::CodeStudioSpecGenerateSpec => "plan-generate-spec",
            Self::CodeStudioSpecGenerateDesign => "plan-generate-design",
            Self::CodeStudioSpecGenerateTasks => "plan-generate-tasks",
            Self::CodeStudioSpecImplementTask => "plan-implement-task",
            Self::CodeStudioSpecFinalReport => "plan-final-report",
            Self::SuperAdversarialInitial => "initial-system",
            Self::SuperAdversarialReview => "review-system",
            Self::SuperAdversarialJudge => "judge-system",
            Self::SuperAdversarialFinal => "final-system",
        }
    }

    fn legacy_template(self) -> &'static str {
        match self {
            Self::AosRouterIntent => LEGACY_AOS_ROUTER_INTENT,
            Self::WatchdogIntent => LEGACY_WATCHDOG_INTENT,
            Self::CodeStudioSpecGenerateSpec => LEGACY_PLAN_GENERATE_SPEC,
            Self::CodeStudioSpecGenerateDesign => LEGACY_PLAN_GENERATE_DESIGN,
            Self::CodeStudioSpecGenerateTasks => LEGACY_PLAN_GENERATE_TASKS,
            Self::CodeStudioSpecImplementTask => LEGACY_PLAN_IMPLEMENT_TASK,
            Self::CodeStudioSpecFinalReport => LEGACY_PLAN_FINAL_REPORT,
            Self::SuperAdversarialInitial => LEGACY_SUPER_ADVERSARIAL_INITIAL,
            Self::SuperAdversarialReview => LEGACY_SUPER_ADVERSARIAL_REVIEW,
            Self::SuperAdversarialJudge => LEGACY_SUPER_ADVERSARIAL_JUDGE,
            Self::SuperAdversarialFinal => LEGACY_SUPER_ADVERSARIAL_FINAL,
        }
    }
}

pub struct PromptRegistry;

impl PromptRegistry {
    pub fn render(prompt_id: PromptId, replacements: &[(&str, &str)]) -> String {
        if runtime_skill_prompts_enabled() {
            if let Some(rendered) = Self::render_skill(prompt_id, replacements) {
                return rendered;
            }
        }
        Self::render_legacy(prompt_id, replacements)
    }

    pub fn render_legacy(prompt_id: PromptId, replacements: &[(&str, &str)]) -> String {
        render_template(prompt_id.legacy_template(), replacements)
    }

    pub fn render_skill(prompt_id: PromptId, replacements: &[(&str, &str)]) -> Option<String> {
        render_builtin_skill_template(prompt_id.skill_name(), prompt_id.section(), replacements)
    }
}

pub fn load_builtin_skill(name: &str) -> Option<&'static str> {
    match normalize_skill_name(name)?.as_str() {
        SKILL_AOS_ROUTER => Some(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-skills/aos-router/SKILL.md"
        ))),
        SKILL_WATCHDOG => Some(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-skills/watchdog/SKILL.md"
        ))),
        SKILL_CODE_STUDIO_CODE => Some(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-skills/code-studio-code/SKILL.md"
        ))),
        SKILL_CODE_STUDIO_SPEC => Some(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-skills/code-studio-spec/SKILL.md"
        ))),
        SKILL_NL2SQL_REFERENCE => Some(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-skills/nl2sql-reference/SKILL.md"
        ))),
        SKILL_PM_ASSISTANT => Some(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-skills/pm-assistant/SKILL.md"
        ))),
        SKILL_SUPER_ADVERSARIAL => Some(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/builtin-skills/super-adversarial/SKILL.md"
        ))),
        _ => None,
    }
}

pub fn builtin_skill_section(name: &str, section: &str) -> Option<&'static str> {
    let doc = load_builtin_skill(name)?;
    let section = section.trim();
    if section.is_empty() {
        return None;
    }
    let open = format!("<!-- aos:section {section} -->");
    let start = doc.find(&open)? + open.len();
    let after_open = doc.get(start..)?;
    let end = after_open.find(SECTION_CLOSE)?;
    Some(after_open[..end].trim_matches('\n'))
}

pub fn render_builtin_skill_template(
    name: &str,
    section: &str,
    replacements: &[(&str, &str)],
) -> Option<String> {
    let template = builtin_skill_section(name, section)?;
    Some(render_template(template, replacements))
}

pub fn runtime_skill_prompts_enabled() -> bool {
    std::env::var("AOS_BUILTIN_SKILL_RUNTIME_PROMPTS")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
}

fn normalize_skill_name(name: &str) -> Option<String> {
    let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
    (!normalized.is_empty()).then_some(normalized)
}

fn render_template(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open_index) = rest.find("{{") {
        output.push_str(&rest[..open_index]);
        let after_open = &rest[open_index + 2..];
        let Some(close_index) = after_open.find("}}") else {
            output.push_str(&rest[open_index..]);
            return output;
        };
        let key = &after_open[..close_index];
        if let Some((_, value)) = replacements
            .iter()
            .find(|(candidate, _)| candidate.trim() == key.trim())
        {
            output.push_str(value);
        } else {
            output.push_str("{{");
            output.push_str(key);
            output.push_str("}}");
        }
        rest = &after_open[close_index + 2..];
    }
    output.push_str(rest);
    output
}

const LEGACY_AOS_ROUTER_INTENT: &str = r#"你是 AOS Router Agent，只做路由决策，不回答用户问题。只能输出 JSON 对象。
候选能力：
{{capabilities}}

输出字段：
targetCapability: 必须是候选能力之一，或 null
confidence: 0 到 1
reason: 简短中文理由
needsWebSearch: true/false；仅当回答依赖会随时间变化的外部事实，或需要公网证据/来源支撑时为 true
webSearchQuery: needsWebSearch=true 时给一句简洁检索 query，否则 null
webSearchReason: 联网判定的简短理由，否则 null
requiredEvidence: 数组，只能包含 web | workspace | code_change | data_execution | deep_research | super_adversarial；不需要工具证据时为空数组
needsClarification: true/false
clarificationQuestion: 需要澄清时给一个短问题，否则 null
rewrittenPrompt: 可选，交给目标能力的改写请求；不需要改写则 null

规则：
- 根据用户真正需要的执行能力和交付物做语义判断，不得因为单个关键词直接路由。
- 查询任务详情/取消/重试、状态，或排查队列与心跳才使用 watchdog。
- 需要读改仓库、修代码、生成 diff 或运行测试才使用 rd_agent；解释代码或报错不一定需要 rd_agent。
- 只有需要访问已配置数据源、生成/执行查询、读取业务指标或复用 SQL 知识库时才使用 nl2sql。“SQL 是什么意思”“解释这段 SQL/数据”等概念或审阅问题应使用 ai_chat，除非用户明确要求执行。
- 需要多步骤外部研究、产运/增长/市场策略或业务指标根因研究时使用 pm_assistant；普通“为什么”问题和稳定知识解释使用 ai_chat。
- 数据归因由用户显式模式开关决定，Router 不得仅根据“下降/归因/ROI”等词自动启动数据归因。
- 只有用户明确要求多方案交锋、正反论证或裁决时使用 super_adversarial。
- 普通知识、解释、闲聊以及能力不明确的请求使用 ai_chat 或 generic_ai。
- 当前用户消息是最高优先级；历史上下文只用于理解代词和承接，不得让上一轮主题覆盖本轮任务。
- 联网判定必须按语义判断，不按固定关键词。用户明确要求联网、搜索、浏览网页或查公开资料时，无论主题是什么都必须 needsWebSearch=true 且 requiredEvidence 包含 web。用户询问当前行业/业界实践、主流方案、真实企业如何实施、竞品现状或外部基准时，也需要公开证据，因此设置 web。只有无需外部证据的稳定概念解释、纯文本改写或私有资料内部分析才可为 false。
- 回答依赖附件、项目文件、SQL 知识库、未随本次路由请求提供的历史原文或其他私有工作区内容时，requiredEvidence 包含 workspace。路由输入中已经附带的最近会话原文是本轮已授权上下文；如果答案可直接从其中逐字得到，不要要求 workspace 重复取证。
- 用户要求实际修改/创建/修复代码并交付验证结果时，requiredEvidence 包含 code_change；只解释代码或报错时不包含。
- 用户要求从已配置数据源得到真实业务结果或实际执行 SQL 时，requiredEvidence 包含 data_execution；只解释或审阅 SQL 时不包含。
- 用户明确要求完整、多来源、可追溯的深度调研或研究报告时，requiredEvidence 包含 deep_research；普通知识问答不包含。
- 用户明确要求多方案交锋、正反论证或裁决时，requiredEvidence 包含 super_adversarial；普通问答不包含。
- 如果最高置信度低于 {{threshold}}，needsClarification=true。
- 不要选择未在候选能力里的能力。
- 不要 Markdown，不要额外文本."#;

const LEGACY_WATCHDOG_INTENT: &str = r#"你是 AOS WatchDog 的意图解析器，只输出 JSON，不回答用户问题。
任务：把用户问题解析成结构化查询意图。不能编造任务、状态、数量或原因。

输出 JSON 字段：
intent: list_tasks | task_detail | queue_health | stale_tasks | explain_no_reply | capability_health | action
scope: conversation | user | tenant。默认 scope 是 {{default_scope}}；只有用户明确说“全部/all/租户/所有”才用 tenant。
capability: ai_chat | pm_assistant | rd_agent | nl2sql | super_adversarial | watchdog | generic_ai | null
status: 数组或 null。用户说“运行/在跑/工作中/执行中/active/running”时必须输出 ["queued","claimed","running","waiting_input","retrying","cancelling"]。
queueIntent: all | dead | stale_lease | null
staleMinutes: 数字或 null。用户说“卡住/没心跳/超时/没回复很久”默认 10。
taskIndex: 数字或 null，用于“详情 1/取消 1/重试 1”。
action: detail | cancel | retry | null
limit: 1 到 50，默认 20。
needsLlmSummary: true 或 false。

动作优先：详情/取消/重试必须解析为 intent=action。
示例：
当前有哪些 agent 在运行？ => {"intent":"list_tasks","scope":"{{default_scope}}","capability":null,"status":["queued","claimed","running","waiting_input","retrying","cancelling"],"queueIntent":null,"staleMinutes":null,"taskIndex":null,"action":null,"limit":10,"needsLlmSummary":true}
产运助手有几个在工作？ => capability=pm_assistant 且 status 为活跃状态。
为什么飞书刚才没回复？ => intent=explain_no_reply, staleMinutes=10。
死信任务 => intent=queue_health, queueIntent=dead。
只输出一个 JSON 对象."#;

const LEGACY_PLAN_GENERATE_SPEC: &str = r#"你是 AOS Code Studio 的 Plan Mode 规格 Agent。

任务：把用户需求整理为可确认的规格文档，不要实现代码，不要生成 Diff。

输出必须是 JSON：
{
  "requirementsMd": string,
  "acceptanceMd": string
}

要求：
- requirementsMd 写清目标、用户场景、功能边界、非目标和关键约束。
- acceptanceMd 写成可验证验收标准。
- 如果信息不足，在 requirementsMd 中列出需要确认的问题，但仍给出可推进的初版。
- 仓库上下文按“仓库”分段时，requirementsMd 必须列出各服务的职责、协作边界和待核实项；不得把一个仓库的证据归到另一个仓库。

用户需求：
{{requirement}}

仓库上下文：
{{repo_context}}"#;

const LEGACY_PLAN_GENERATE_DESIGN: &str = r#"你是 AOS Code Studio 的 Plan Mode 设计 Agent。

任务：基于已确认规格生成技术设计，不要实现代码，不要生成 Diff。

输出必须是 JSON：
{
  "designMd": string
}

要求：
- designMd 包含架构方案、受影响模块、数据/API变化、执行流程、风险与测试策略。
- 必须结合仓库上下文，不要泛泛而谈。
- 按仓库分别列出实际受影响文件、符号/入口和证据；跨服务调用必须说明请求方向、契约、兼容策略和发布顺序。
- 仓库上下文明确缺失、读取失败或未同步时，必须在 designMd 标记证据缺口，不得虚构文件路径、接口或已检查代码。
- 对不确定项明确标注。

用户原始需求：
{{requirement}}

已确认规格：
{{requirements_md}}

验收标准：
{{acceptance_md}}

仓库上下文：
{{repo_context}}"#;

const LEGACY_PLAN_GENERATE_TASKS: &str = r#"你是 AOS Code Studio 的 Plan Mode 任务拆解 Agent。

任务：把已确认规格和设计拆解为可逐项实现的开发任务，不要实现代码，不要生成 Diff。

输出必须是 JSON：
{
  "tasksMd": string,
  "taskItems": [
    {
      "id": "task-1",
      "title": string,
      "description": string,
      "priority": "p0" | "p1" | "p2",
      "acceptance": string[]
    }
  ]
}

要求：
- taskItems 必须有稳定 id，按实现顺序排列。
- 每个任务只覆盖一个明确改动目标。
- acceptance 写成该任务完成后的检查点。
- 每个跨服务任务必须注明目标仓库和依赖任务；tasksMd 需要给出跨仓库的集成、联调、兼容与回滚检查点。

用户原始需求：
{{requirement}}

已确认规格：
{{requirements_md}}

已确认设计：
{{design_md}}

总体验收：
{{acceptance_md}}

仓库上下文：
{{repo_context}}"#;

const LEGACY_PLAN_IMPLEMENT_TASK: &str = r#"请基于以下已确认 Plan Mode 规格实现一个开发任务。

严格约束：
- 只实现当前 task item，不要顺手做其他任务。
- 必须读取真实代码后再修改。
- 可以在候选工作区改文件并跑测试。
- 最终必须通过 AOS Diff-first 审批流输出可审查 Diff。
- 不要声称主仓库已被修改，除非用户后续应用 Diff。

Plan 标题：
{{title}}

已确认规格：
{{requirements_md}}

已确认设计：
{{design_md}}

当前 task item：
{{task_item_json}}"#;

const LEGACY_PLAN_FINAL_REPORT: &str = r#"你是 AOS Code Studio 的 Plan Mode 收尾 Agent。

任务：基于规格、设计、任务列表和实现摘要生成最终交付报告。

输出必须是 JSON：
{
  "finalReportMd": string
}

报告必须包含：
- 已完成内容
- 未完成或待人工应用的 Diff
- 测试结果
- 风险和后续建议

Plan 标题：
{{title}}

规格：
{{requirements_md}}

设计：
{{design_md}}

任务列表：
{{tasks_md}}

实现摘要：
{{implementation_summary_json}}"#;

const LEGACY_SUPER_ADVERSARIAL_INITIAL: &str = "你是超级对抗模式中的参赛模型 {{model}}。请立即独立回答用户问题，不要等待或假设系统已经完成联网检索。要求：事实优先；不知道就说不知道；不要编造来源；不要为了显得不同而反驳；给出清晰、可执行、完整的答案。如果结论确实依赖实时、外部或权威事实，正文后按调用方协议提出精确证据请求，后续轮次会获得共享证据；纯推理和已有上下文足够时不要请求检索。如果存在追问上下文，请继承仍然有效的信息，同时以用户新问题为最高优先级。";

const LEGACY_SUPER_ADVERSARIAL_REVIEW: &str = "你是超级对抗模式中的参赛模型 {{model}}，当前第 {{round}} 轮。你会看到自己的上一轮完整答案、其他行业专家/模型的上一轮完整答案、历史观点轨迹以及按需取得的共享证据。请把其他答案当作外部专家评审意见来校准你的结论：正确的吸收，错误的纠正，不确定的标注。不要为了反驳而反驳，也不要因为面子保留错误观点。只有未决异议确实必须依赖新的外部事实时才请求定向补证；逻辑或已有材料足以解决时不要搜索。只有认可共同核心结论且没有重大未决异议时，才能投出一致认可票。";

const LEGACY_SUPER_ADVERSARIAL_JUDGE: &str = "你是多模型对抗审查的中立裁判。你的目标是事实正确、逻辑严谨、承认不确定性。服务端只会在至少两个健康参赛模型明确认可共同结论后调用你；不要因为需要对抗而强行反驳。必须逐项审计数字、日期、因果、否定关系、适用边界和证据等关键 claim，并通过 claim_audit_complete 与 critical_conflicts 明确返回审计结果。只有完成审计、没有关键冲突且共同结论没有明显错误时，才判定 resolved=true，并从真实参赛模型中选择一个胜出者。请只返回 JSON。";

const LEGACY_SUPER_ADVERSARIAL_FINAL: &str = "你是最终答案整理者。你需要基于多模型互审结果，输出一个标准答案。坚持事实优先、逻辑优先；错误就是错误，正确就承认正确；不要为了戏剧性而制造分歧。";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_repo_shipped_skill_contracts() {
        let router = load_builtin_skill(SKILL_AOS_ROUTER).expect("router skill");
        assert!(router.contains("# AOS Router Agent"));
        let section = builtin_skill_section(SKILL_AOS_ROUTER, "router-intent")
            .expect("router intent section");
        assert!(section.contains("targetCapability"));
        assert!(builtin_skill_section("missing", "router-intent").is_none());
    }

    #[test]
    fn all_builtin_skill_docs_are_ascii_and_loadable() {
        for name in [
            SKILL_AOS_ROUTER,
            SKILL_WATCHDOG,
            SKILL_CODE_STUDIO_CODE,
            SKILL_CODE_STUDIO_SPEC,
            SKILL_NL2SQL_REFERENCE,
            SKILL_PM_ASSISTANT,
            SKILL_SUPER_ADVERSARIAL,
        ] {
            let doc = load_builtin_skill(name).expect("builtin skill should load");
            assert!(doc.is_ascii(), "{name} must stay English/ASCII");
            assert!(doc.contains("# AOS"));
        }
    }

    #[test]
    fn renders_templates_without_mutating_inserted_values() {
        let rendered = render_template(
            "A={{a}}\nB={{b}}",
            &[("a", "{{b}} should stay literal"), ("b", "done")],
        );
        assert_eq!(rendered, "A={{b}} should stay literal\nB=done");
    }

    #[test]
    fn prompt_registry_preserves_legacy_runtime_contracts_by_default() {
        let router = PromptRegistry::render(
            PromptId::AosRouterIntent,
            &[("capabilities", "- ai_chat: chat"), ("threshold", "0.8")],
        );
        assert!(router.contains("你是 AOS Router Agent"));
        assert!(router.contains("详情/取消/重试"));
        assert!(router.contains("- ai_chat: chat"));

        let watchdog = PromptRegistry::render(
            PromptId::WatchdogIntent,
            &[("default_scope", "conversation")],
        );
        assert!(watchdog.contains("当前有哪些 agent 在运行？"));
        assert!(watchdog.contains("\"scope\":\"conversation\""));

        let spec = PromptRegistry::render(
            PromptId::CodeStudioSpecGenerateSpec,
            &[("requirement", "需求"), ("repo_context", "上下文")],
        );
        assert!(spec.contains("\"requirementsMd\""));
        assert!(spec.contains("需求"));

        let debate = PromptRegistry::render(
            PromptId::SuperAdversarialReview,
            &[("model", "B"), ("round", "3")],
        );
        assert!(debate.contains("参赛模型 B"));
        assert!(debate.contains("当前第 3 轮"));
    }

    #[test]
    fn skill_prompts_share_required_output_contracts_with_legacy_prompts() {
        for (prompt_id, replacements, required_terms) in [
            (
                PromptId::AosRouterIntent,
                vec![("capabilities", "- ai_chat: chat"), ("threshold", "0.8")],
                vec![
                    "targetCapability",
                    "confidence",
                    "needsClarification",
                    "needsWebSearch",
                ],
            ),
            (
                PromptId::WatchdogIntent,
                vec![("default_scope", "conversation")],
                vec!["intent", "scope", "action", "queued"],
            ),
            (
                PromptId::CodeStudioSpecGenerateTasks,
                vec![
                    ("requirement", "req"),
                    ("requirements_md", "requirements"),
                    ("design_md", "design"),
                    ("acceptance_md", "acceptance"),
                    ("repo_context", "repo"),
                ],
                vec!["tasksMd", "taskItems", "priority"],
            ),
            (
                PromptId::SuperAdversarialFinal,
                vec![],
                vec!["final answer", "facts"],
            ),
        ] {
            let legacy = PromptRegistry::render_legacy(prompt_id, &replacements);
            let skill = PromptRegistry::render_skill(prompt_id, &replacements)
                .expect("skill prompt should render");
            for term in required_terms {
                let legacy_contains = legacy
                    .to_ascii_lowercase()
                    .contains(&term.to_ascii_lowercase());
                let skill_contains = skill
                    .to_ascii_lowercase()
                    .contains(&term.to_ascii_lowercase());
                assert!(
                    legacy_contains || skill_contains,
                    "missing contract term {term} for {prompt_id:?}"
                );
            }
        }
    }
}
