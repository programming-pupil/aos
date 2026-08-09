//! Deterministic context planning policy for RD coding tasks.

use serde_json::{json, Value};

use crate::{RdContextBudget, RdContextProfile};

pub fn normalize_rd_profile_for_mode(profile: RdContextProfile, mode: &str) -> RdContextProfile {
    match mode {
        "modify" => RdContextProfile::Modify,
        "explain" => RdContextProfile::Explain,
        "review" => {
            if profile == RdContextProfile::DeepReview {
                RdContextProfile::DeepReview
            } else {
                RdContextProfile::Review
            }
        }
        _ => match profile {
            RdContextProfile::Overview => RdContextProfile::Overview,
            RdContextProfile::DeepReview | RdContextProfile::Review => RdContextProfile::FocusedAsk,
            RdContextProfile::Modify | RdContextProfile::Explain => RdContextProfile::FocusedAsk,
            RdContextProfile::FocusedAsk => RdContextProfile::FocusedAsk,
        },
    }
}

pub const fn default_rd_context_depth(profile: RdContextProfile) -> &'static str {
    match profile {
        RdContextProfile::Overview => "shallow",
        RdContextProfile::DeepReview => "deep",
        _ => "standard",
    }
}

pub fn normalize_rd_context_depth(
    depth: Option<&str>,
    profile: RdContextProfile,
    should_deep_scan: bool,
) -> String {
    let normalized = depth
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match normalized.as_str() {
        "shallow" | "light" | "overview" => "shallow".to_string(),
        "deep" | "full" | "audit" => "deep".to_string(),
        "standard" | "normal" | "focused" => "standard".to_string(),
        _ if should_deep_scan || profile == RdContextProfile::DeepReview => "deep".to_string(),
        _ => default_rd_context_depth(profile).to_string(),
    }
}

pub fn rd_context_budget_json(budget: RdContextBudget) -> Value {
    json!({
        "runtimeHintBytes": budget.runtime_hint_bytes,
        "retrievalBytes": budget.retrieval_bytes,
        "inlineContextBytes": budget.inline_context_bytes,
        "retrievalFileLimit": budget.retrieval_file_limit,
        "retrievalNotesPerFile": budget.retrieval_notes_per_file,
        "retrievalTermLimit": budget.retrieval_term_limit,
        "semanticTopK": budget.semantic_top_k,
        "treeItemLimit": budget.tree_item_limit,
        "manifestSectionBytes": budget.manifest_section_bytes,
        "runtimeReadFileBudget": budget.runtime_read_file_budget,
        "runtimeSearchBudget": budget.runtime_search_budget,
    })
}

pub fn build_rd_context_policy_section(context_profile: RdContextProfile) -> String {
    let budget = context_profile.budget();
    let mode_guidance = match context_profile {
        RdContextProfile::Overview => {
            "这是项目概览/架构问答，不是全仓库审计。优先使用 README、manifest、入口文件、路由/模块索引、文件摘要和 embedding/词法召回；最多读取少量关键文件核对事实。不要逐个文件扫描，不要为了“完整性”继续搜索。若证据足够，请直接输出项目定位、模块分层、核心数据流和 Mermaid 架构图；如果用户需要逐文件审查，再建议切换深度审计。"
        }
        RdContextProfile::FocusedAsk => {
            "这是定向代码库问答。先用索引召回和搜索定位相关文件，再按需读取关键片段。不要全仓库盲扫；如果问题无法用当前证据回答，请说明还需要读取哪些文件，而不是扩大到无界扫描。"
        }
        RdContextProfile::Explain => {
            "这是报错解释。优先围绕错误栈、日志关键词、失败命令和相关调用链定向搜索；读取错误附近文件和直接依赖，不要扫描无关模块。"
        }
        RdContextProfile::Modify => {
            "这是代码修改任务。先给计划，定位最小相关文件集，再生成可审查 Diff。只有在测试失败或证据不足时逐步扩大搜索范围；默认不要全仓库扫描。"
        }
        RdContextProfile::Review => {
            "这是代码审查任务。允许比普通问答读取更多文件，但仍应通过索引召回、风险模式和模块边界分批审查；优先输出高置信 findings，不要把读取所有文件当作必要前提。"
        }
        RdContextProfile::DeepReview => {
            "这是深度审计任务。可以使用更大读取预算和多轮搜索，但仍必须分阶段进行：先索引召回和风险入口，再抽样/聚类扩展，避免重复读取同一文件和重复输出同类工具结果。"
        }
    };
    format!(
        "## Context Engineering 策略\n- 当前 profile：{}（{}）。\n- 读取预算建议：read_file 约 {} 个关键文件以内，glob/grep 搜索约 {} 次以内；深度审计除外也要分阶段扩大。\n- 使用 progressive disclosure：索引/摘要/embedding/词法召回 -> 少量关键文件核对 -> 必要时扩大搜索。\n- 工具输出只当证据，不要重复读取已经足够的信息；证据足够时停止搜索并回答。\n- {}\n",
        context_profile.as_str(),
        context_profile.display_name(),
        budget.runtime_read_file_budget,
        budget.runtime_search_budget,
        mode_guidance
    )
}

pub fn build_rd_context_plan_section(
    context_profile: RdContextProfile,
    has_repository: bool,
    has_explicit_files: bool,
    has_workflow: bool,
) -> (String, Value) {
    let budget = context_profile.budget();
    let stages: Vec<&'static str> = match context_profile {
        RdContextProfile::Overview => vec![
            "先使用缓存仓库/目录摘要、README/manifest/入口文件建立项目地图",
            "用 embedding/词法/symbol/import 多路召回补充候选文件",
            "只读取少量关键真实文件核对架构事实",
            "证据足够后输出项目定位、模块分层、核心数据流和 Mermaid 架构图",
        ],
        RdContextProfile::FocusedAsk => vec![
            "先从缓存摘要和多路召回定位相关模块",
            "读取最相关的真实文件片段核对",
            "证据足够后回答；不足时说明还需要哪些上下文",
        ],
        RdContextProfile::Explain => vec![
            "先从错误栈/日志/命令关键词定位文件和调用链",
            "读取错误附近文件和直接依赖",
            "给出原因、验证步骤和可选修复方向",
        ],
        RdContextProfile::Modify => vec![
            "先生成修改计划和候选相关文件集",
            "读取真实文件确认接口、调用链和测试约束",
            "做最小必要修改并输出可审查 Diff",
            "如测试失败，再基于失败输出和候选 Diff 做下一轮修复",
        ],
        RdContextProfile::Review => vec![
            "先用缓存摘要、多路召回和风险入口确定审查范围",
            "按模块分批读取关键文件，优先高风险路径",
            "输出文件/行级 findings、严重级别、风险和建议",
        ],
        RdContextProfile::DeepReview => vec![
            "先建立全局风险地图和模块分组",
            "分阶段扩大读取范围，避免重复读取和重复 findings",
            "保留证据链并输出高置信问题，低置信项明确标注",
        ],
    };
    let stop_conditions: Vec<&'static str> = match context_profile {
        RdContextProfile::Overview | RdContextProfile::FocusedAsk => vec![
            "已经能解释项目目的、核心模块和关键数据流",
            "继续搜索只会重复已有证据",
            "需要逐文件审查时应建议切换深度审计",
        ],
        RdContextProfile::Explain => vec![
            "错误根因、涉及文件和验证步骤已明确",
            "继续搜索无法提高结论置信度",
        ],
        RdContextProfile::Modify => vec![
            "相关文件、接口约束和最小 Diff 已明确",
            "Diff 已能解释并可验证",
        ],
        RdContextProfile::Review | RdContextProfile::DeepReview => vec![
            "高风险路径已覆盖且 findings 有文件/行级证据",
            "新增读取只产生重复问题",
            "预算不足时输出已覆盖范围和建议的下一阶段审计范围",
        ],
    };
    let section = format!(
        "## Context Planner\n\
         - 有绑定仓库：{}\n\
         - 用户显式文件：{}\n\
         - 启用工作流：{}\n\
         - 计划读取上限建议：read_file≈{}，grep/glob≈{}。这是效果优先的自适应预算，不是机械截断；如果证据不足，可以说明原因并建议进入更深 profile。\n\
         - 执行阶段：\n  - {}\n\
         - 停止条件：\n  - {}",
        has_repository,
        has_explicit_files,
        has_workflow,
        budget.runtime_read_file_budget,
        budget.runtime_search_budget,
        stages.join("\n  - "),
        stop_conditions.join("\n  - ")
    );
    let detail = json!({
        "contextProfile": context_profile.as_str(),
        "contextProfileName": context_profile.display_name(),
        "hasRepository": has_repository,
        "hasExplicitFiles": has_explicit_files,
        "hasWorkflow": has_workflow,
        "budget": rd_context_budget_json(budget),
        "stages": stages,
        "stopConditions": stop_conditions,
        "effectFirst": true,
        "hardCutoff": false,
    });
    (section, detail)
}
