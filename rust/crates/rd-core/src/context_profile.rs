//! Context profile and budget policy for repository-aware coding tasks.

const MAX_CONTEXT_BYTES: usize = 80_000;
const RD_INLINE_CONTEXT_BUDGET_BYTES: usize = 52_000;
const RD_RUNTIME_CONTEXT_HINT_BUDGET_BYTES: usize = 16_000;
const RD_RETRIEVAL_CONTEXT_BUDGET_BYTES: usize = 18_000;
const RD_OVERVIEW_CONTEXT_HINT_BUDGET_BYTES: usize = 12_000;
const RD_OVERVIEW_RETRIEVAL_CONTEXT_BUDGET_BYTES: usize = 9_000;
const RD_OVERVIEW_INLINE_CONTEXT_BUDGET_BYTES: usize = 28_000;
const RD_SEMANTIC_RETRIEVAL_TOP_K: usize = 28;
const RD_OVERVIEW_SEMANTIC_RETRIEVAL_TOP_K: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdContextProfile {
    Overview,
    FocusedAsk,
    Explain,
    Modify,
    Review,
    DeepReview,
}

impl RdContextProfile {
    pub fn from_task(mode: &str, prompt: &str) -> Self {
        match mode {
            "modify" => Self::Modify,
            "explain" => Self::Explain,
            "review" => {
                if is_deep_review_prompt(prompt) {
                    Self::DeepReview
                } else {
                    Self::Review
                }
            }
            _ if is_overview_prompt(prompt) => Self::Overview,
            _ => Self::FocusedAsk,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::FocusedAsk => "focused_ask",
            Self::Explain => "explain",
            Self::Modify => "modify",
            Self::Review => "review",
            Self::DeepReview => "deep_review",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Overview => "项目概览/架构问答",
            Self::FocusedAsk => "定向代码库问答",
            Self::Explain => "报错解释",
            Self::Modify => "代码修改",
            Self::Review => "代码审查",
            Self::DeepReview => "深度审计",
        }
    }

    pub const fn budget(self) -> RdContextBudget {
        match self {
            Self::Overview => RdContextBudget {
                runtime_hint_bytes: RD_OVERVIEW_CONTEXT_HINT_BUDGET_BYTES,
                retrieval_bytes: RD_OVERVIEW_RETRIEVAL_CONTEXT_BUDGET_BYTES,
                inline_context_bytes: RD_OVERVIEW_INLINE_CONTEXT_BUDGET_BYTES,
                retrieval_file_limit: 8,
                retrieval_notes_per_file: 3,
                retrieval_term_limit: 10,
                semantic_top_k: RD_OVERVIEW_SEMANTIC_RETRIEVAL_TOP_K,
                tree_item_limit: 120,
                manifest_section_bytes: 8_000,
                runtime_read_file_budget: 8,
                runtime_search_budget: 4,
            },
            Self::FocusedAsk => RdContextBudget {
                runtime_hint_bytes: RD_RUNTIME_CONTEXT_HINT_BUDGET_BYTES,
                retrieval_bytes: 14_000,
                inline_context_bytes: RD_INLINE_CONTEXT_BUDGET_BYTES,
                retrieval_file_limit: 14,
                retrieval_notes_per_file: 5,
                retrieval_term_limit: 8,
                semantic_top_k: 20,
                tree_item_limit: 180,
                manifest_section_bytes: 12_000,
                runtime_read_file_budget: 14,
                runtime_search_budget: 6,
            },
            Self::Explain => RdContextBudget {
                runtime_hint_bytes: RD_RUNTIME_CONTEXT_HINT_BUDGET_BYTES,
                retrieval_bytes: 16_000,
                inline_context_bytes: RD_INLINE_CONTEXT_BUDGET_BYTES,
                retrieval_file_limit: 16,
                retrieval_notes_per_file: 5,
                retrieval_term_limit: 10,
                semantic_top_k: 24,
                tree_item_limit: 180,
                manifest_section_bytes: 10_000,
                runtime_read_file_budget: 16,
                runtime_search_budget: 8,
            },
            Self::Modify => RdContextBudget {
                runtime_hint_bytes: 18_000,
                retrieval_bytes: RD_RETRIEVAL_CONTEXT_BUDGET_BYTES,
                inline_context_bytes: RD_INLINE_CONTEXT_BUDGET_BYTES,
                retrieval_file_limit: 18,
                retrieval_notes_per_file: 6,
                retrieval_term_limit: 10,
                semantic_top_k: RD_SEMANTIC_RETRIEVAL_TOP_K,
                tree_item_limit: 220,
                manifest_section_bytes: 12_000,
                runtime_read_file_budget: 24,
                runtime_search_budget: 10,
            },
            Self::Review => RdContextBudget {
                runtime_hint_bytes: 20_000,
                retrieval_bytes: 22_000,
                inline_context_bytes: RD_INLINE_CONTEXT_BUDGET_BYTES,
                retrieval_file_limit: 24,
                retrieval_notes_per_file: 6,
                retrieval_term_limit: 12,
                semantic_top_k: 32,
                tree_item_limit: 260,
                manifest_section_bytes: 12_000,
                runtime_read_file_budget: 30,
                runtime_search_budget: 14,
            },
            Self::DeepReview => RdContextBudget {
                runtime_hint_bytes: 28_000,
                retrieval_bytes: 30_000,
                inline_context_bytes: MAX_CONTEXT_BYTES,
                retrieval_file_limit: 36,
                retrieval_notes_per_file: 8,
                retrieval_term_limit: 14,
                semantic_top_k: 48,
                tree_item_limit: 360,
                manifest_section_bytes: 14_000,
                runtime_read_file_budget: 60,
                runtime_search_budget: 24,
            },
        }
    }

    pub const fn should_run_architecture_pass(self) -> bool {
        matches!(self, Self::Modify | Self::Review | Self::DeepReview)
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "overview" | "project_overview" | "architecture" => Some(Self::Overview),
            "focused_ask" | "focused" | "ask" | "qa" => Some(Self::FocusedAsk),
            "explain" | "diagnose" | "error" => Some(Self::Explain),
            "modify" | "code" | "edit" => Some(Self::Modify),
            "review" | "audit" => Some(Self::Review),
            "deep_review" | "deep_audit" | "full_audit" => Some(Self::DeepReview),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RdContextBudget {
    pub runtime_hint_bytes: usize,
    pub retrieval_bytes: usize,
    pub inline_context_bytes: usize,
    pub retrieval_file_limit: usize,
    pub retrieval_notes_per_file: usize,
    pub retrieval_term_limit: usize,
    pub semantic_top_k: usize,
    pub tree_item_limit: usize,
    pub manifest_section_bytes: usize,
    pub runtime_read_file_budget: usize,
    pub runtime_search_budget: usize,
}

pub fn is_overview_prompt(prompt: &str) -> bool {
    let text = prompt.trim().to_lowercase();
    if text.is_empty() {
        return false;
    }
    contains_any(
        &text,
        &[
            "这个项目",
            "项目是干啥",
            "项目是做什么",
            "项目是什么",
            "干啥的",
            "做什么的",
            "整体架构",
            "架构图",
            "架构是",
            "模块关系",
            "模块划分",
            "项目概览",
            "项目介绍",
            "怎么启动",
            "如何启动",
            "启动流程",
            "技术栈",
            "overview",
            "architecture",
            "diagram",
            "mermaid",
            "what is this project",
            "how does this project work",
            "how to start",
        ],
    )
}

pub fn is_deep_review_prompt(prompt: &str) -> bool {
    let text = prompt.trim().to_lowercase();
    contains_any(
        &text,
        &[
            "全量审计",
            "全仓库审计",
            "全项目审计",
            "检查所有问题",
            "所有风险",
            "所有bug",
            "所有 bug",
            "逐个文件",
            "逐行",
            "深度审计",
            "完整审计",
            "安全审计",
            "deep audit",
            "full audit",
            "entire codebase",
            "whole repository",
        ],
    )
}

pub fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}
