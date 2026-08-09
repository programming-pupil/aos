//! Config API — read settings from aos's configuration files.
//!
//! All endpoints require JWT Bearer authentication.

use axum::{
    extract::{Extension, State},
    routing::{get as routing_get, patch as routing_patch},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use pm_domain::budget::{PmBudgetProfile, PmTimeoutBudget};

#[derive(Debug, Serialize)]
pub struct ConfigSnapshot {
    pub path: String,
    pub source: String,
    pub content: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ConfigOverview {
    pub configs: Vec<ConfigSnapshot>,
    pub permission_mode: Option<String>,
    pub current_model: Option<String>,
    pub active_plugins: Vec<String>,
    pub active_mcp_servers: Vec<String>,
}

#[derive(Debug, Serialize)]
#[expect(dead_code)]
#[allow(clippy::struct_excessive_bools)]
pub struct FeatureFlags {
    pub hooks_enabled: bool,
    pub plugins_enabled: bool,
    pub mcp_enabled: bool,
    pub telemetry_enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum EnvValueType {
    Bool,
    Integer,
    Float,
    #[allow(dead_code)]
    Secret,
    String,
}

#[derive(Debug, Clone, Copy)]
struct EnvVarDef {
    key: &'static str,
    label: &'static str,
    description: &'static str,
    value_type: EnvValueType,
    default_value: &'static str,
}

const OPS_ENV_DEFS: &[EnvVarDef] = &[
    EnvVarDef {
        key: "PM_V2_DEFAULT_ENABLED",
        label: "PM V2 默认启用",
        description: "产运 V2 功能总开关。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_FORCE_BACKGROUND_STREAM",
        label: "强制后台流式执行",
        description: "PM 问答是否默认走后台任务流。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_SCHEDULER_INTERVAL_SECS",
        label: "PM 调度周期(秒)",
        description: "PM 调度器轮询间隔（含后台任务拉起与治理汇总）。",
        value_type: EnvValueType::Integer,
        default_value: "120",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_RUNTIME_POLL_SECS",
        label: "后台任务 runtime 轮询(秒)",
        description: "后台 research runtime 轮询间隔。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "PM_SCHEDULER_ENABLE_SLO_ROLLUP",
        label: "启用 SLO 汇总",
        description: "PM 调度是否执行 SLO 日汇总。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOSD_GITHUB_TOKEN",
        label: "GitHub Token",
        description: "Skill 市场访问 GitHub API 的令牌。保存后立即生效；运行时修改不会写入 .env，重启后仍需由启动环境提供。",
        value_type: EnvValueType::Secret,
        default_value: "",
    },
    EnvVarDef {
        key: "PM_RETRIEVE_SEARCH_ONLY",
        label: "仅 Search 检索",
        description: "是否仅启用 search 类 route。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_PREFLIGHT_ENABLE_MODEL_PROBE",
        label: "启用模型预检探测",
        description: "默认关闭。仅用于诊断模型通道；正常请求会直接以真实规划调用验证模型，避免额外首包延迟。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_PREFLIGHT_ENABLE_RETRIEVAL_PROBE",
        label: "启用 preflight 检索探测",
        description: "默认关闭。仅在配置 PM_PREFLIGHT_SEARCH_BASE_URLS 后，用自定义 endpoint 做检索出网 smoke probe；真实 PM 搜索走 Search 扩展 / MCP / 模型原生 search。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_PREFLIGHT_SEARCH_BASE_URLS",
        label: "Preflight 检索探测 URL",
        description: "自定义 preflight 检索探测 URL 列表（逗号分隔，可用 endpoint|queryParam）。不配置则不探测公网搜索。",
        value_type: EnvValueType::String,
        default_value: "",
    },
    EnvVarDef {
        key: "PM_PREFLIGHT_SEARCH_BASE_URLS_APPEND",
        label: "Preflight 搜索源追加(兼容)",
        description: "兼容变量，追加自定义 preflight 检索探测 URL 列表。",
        value_type: EnvValueType::String,
        default_value: "",
    },
    EnvVarDef {
        key: "UNIFIED_NATIVE_SEARCH_TIMEOUT_SECS",
        label: "模型原生联网超时(秒)",
        description: "AI 对话、产运助手、超级对抗共用 Search Orchestrator 的普通模型原生联网单次超时。",
        value_type: EnvValueType::Integer,
        default_value: "120",
    },
    EnvVarDef {
        key: "UNIFIED_RESEARCH_NATIVE_SEARCH_TIMEOUT_SECS",
        label: "研究类原生联网超时(秒)",
        description: "产运深研、超级对抗证据层等研究类问题的模型原生联网单次超时。",
        value_type: EnvValueType::Integer,
        default_value: "180",
    },
    EnvVarDef {
        key: "UNIFIED_REPORT_STRATEGY_NATIVE_SEARCH_TIMEOUT_SECS",
        label: "报告策略原生联网超时(秒)",
        description: "长报告策略场景的模型原生联网单次超时；超时后保留已有证据并进入后备链路。",
        value_type: EnvValueType::Integer,
        default_value: "180",
    },
    EnvVarDef {
        key: "UNIFIED_PROBE_VERIFY_PROVIDER_CITATIONS",
        label: "探针强制打开原生引用",
        description: "开启后即使模型原生搜索已返回足量结构化引用，也逐页抓取验证；默认仅在引用不足或来源仍待验证时抓取。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_ENABLE_SOFT_QUALITY_GATE",
        label: "启用软质量门",
        description: "是否启用 PM 软质量门。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_VISIBLE_ANSWER_ORIGIN_MARKER",
        label: "答案来源标记",
        description: "是否在可见答案中标记来源。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_INCLUDE_HISTORICAL_HINTS_IN_PROMPT",
        label: "注入历史提示",
        description: "是否将历史策略提示注入 prompt。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_STRICT_SUBTASK_CLOSURE",
        label: "严格 subtask 闭环",
        description: "是否强制 subtask 闭环校验。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_PARALLEL_SUBTASK_USE_BEST_TURN",
        label: "合并并行研究证据",
        description: "并行子任务已有可用证据时直接合并综合，避免随后重复执行一次完整检索。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_ALLOW_TIGHTER_SOURCE_SLOT_FROM_CONTRACT",
        label: "允许合同约束收紧 source slot",
        description: "是否允许由合同约束收紧 source slot。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_APPLY_EXEC_CONSTRAINT_BUDGETS",
        label: "应用执行约束预算",
        description: "是否应用 exec constraints 预算裁剪。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_ROUTE_STRICT_MODE",
        label: "严格路由模式",
        description: "路由失败时是否严格拦截低质量 route。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_ENABLE_FALLBACK_TASK_GRAPH",
        label: "启用 fallback task graph",
        description: "缺少 task graph 时是否自动回退。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_QUALITY_VISIBLE_DEBUG",
        label: "显示质量调试信息",
        description: "是否显示 PM 质量调试信息。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_ENABLE_PARALLEL_SUBTASK_KERNEL",
        label: "启用并行 subtask 内核",
        description: "是否启用并行 subtask 调度。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_PARALLEL_SUBTASK_MAX_CANDIDATES",
        label: "并行 subtask 最大候选数",
        description: "并行模式候选上限。",
        value_type: EnvValueType::Integer,
        default_value: "6",
    },
    EnvVarDef {
        key: "PM_PARALLEL_SUBTASK_MAX_CONCURRENCY",
        label: "并行 subtask 最大并发",
        description: "并行模式最大并发数。",
        value_type: EnvValueType::Integer,
        default_value: "4",
    },
    EnvVarDef {
        key: "PM_PARALLEL_SUBTASK_MAX_ATTEMPTS",
        label: "并行 subtask 最大尝试数",
        description: "并行模式最大尝试次数。",
        value_type: EnvValueType::Integer,
        default_value: "3",
    },
    EnvVarDef {
        key: "PM_ADAPTIVE_PROBE_WAVES",
        label: "启用自适应探针波次",
        description: "首轮公平覆盖各外部 subtask，后续仅对未达标维度定向补查。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_ADAPTIVE_PROBE_REPAIR_MAX_CANDIDATES",
        label: "定向补查候选上限",
        description: "每轮针对证据薄弱 subtask 追加的探针数。",
        value_type: EnvValueType::Integer,
        default_value: "1",
    },
    EnvVarDef {
        key: "PM_SUBTASK_CANDIDATE_CAP",
        label: "subtask 候选上限",
        description: "route planning 中 subtask 候选上限。",
        value_type: EnvValueType::Integer,
        default_value: "6",
    },
    EnvVarDef {
        key: "PM_QUERY_ONLY_VARIANT_FANOUT",
        label: "query-only 变体 fanout",
        description: "query-only 模式 query 变体数量。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "PM_SUBTASK_PROBE_COVERAGE_PERCENT",
        label: "subtask probe 覆盖率(%)",
        description: "subtask probe 覆盖率百分比。",
        value_type: EnvValueType::Integer,
        default_value: "70",
    },
    EnvVarDef {
        key: "PM_SOURCE_SLOT_MIN_EFFECTIVE_SECS",
        label: "source slot 最小有效值(秒)",
        description: "source slot 预算最小有效秒数。",
        value_type: EnvValueType::Integer,
        default_value: "90",
    },
    EnvVarDef {
        key: "PM_SUBTASK_GAP_EXTRA_ATTEMPTS",
        label: "subtask 缺口额外尝试",
        description: "在自适应定向补查之外追加的全局尝试次数；默认不追加。",
        value_type: EnvValueType::Integer,
        default_value: "0",
    },
    EnvVarDef {
        key: "PM_MAX_ATTEMPTS_HARD_CAP",
        label: "最大尝试硬上限",
        description: "运行时最大尝试硬上限。",
        value_type: EnvValueType::Integer,
        default_value: "12",
    },
    EnvVarDef {
        key: "PM_SUBTASK_MAX_REPAIR_ATTEMPTS_PER_TASK",
        label: "每个 subtask 最大修复次数",
        description: "单 subtask 修复尝试上限。",
        value_type: EnvValueType::Integer,
        default_value: "2",
    },
    EnvVarDef {
        key: "PM_SUBTASK_PROBE_OUTCOME_HISTORY_CAP",
        label: "subtask probe 历史上限",
        description: "subtask probe outcome 历史容量。",
        value_type: EnvValueType::Integer,
        default_value: "640",
    },
    EnvVarDef {
        key: "PM_SUBTASK_MIN_PARALLEL_AGENTS",
        label: "subtask 最小并行 agent 数",
        description: "每个 subtask 至少需要的独立探针数；引用与来源域仍由独立质量门控制。",
        value_type: EnvValueType::Integer,
        default_value: "1",
    },
    EnvVarDef {
        key: "PM_SUBTASK_MIN_CITATIONS",
        label: "subtask 最小引用数",
        description: "subtask 质量门最小 citation 数。",
        value_type: EnvValueType::Integer,
        default_value: "3",
    },
    EnvVarDef {
        key: "PM_SUBTASK_MIN_DOMAINS",
        label: "subtask 最小域名数",
        description: "subtask 质量门最小 domain 数。",
        value_type: EnvValueType::Integer,
        default_value: "2",
    },
    EnvVarDef {
        key: "PM_SYNTHESIZE_RESERVED_WINDOW_SECS",
        label: "Synthesize 预留窗口(秒)",
        description: "汇总阶段预留时间窗口。",
        value_type: EnvValueType::Integer,
        default_value: "50",
    },
    EnvVarDef {
        key: "PM_ROUTE_FAIL_STREAK_BLOCK_THRESHOLD",
        label: "route 连续失败阻断阈值",
        description: "route 连续失败阻断阈值。",
        value_type: EnvValueType::Integer,
        default_value: "2",
    },
    EnvVarDef {
        key: "PM_ENABLE_MODEL_CONTRACT_REPAIR",
        label: "启用模型契约修复",
        description: "默认关闭。内部执行契约缺失时使用服务端确定性策略；仅诊断模型结构化输出时启用额外修复调用。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_REPORT_SEMANTIC_EXTRACT_MIN_CHARS",
        label: "报告语义抽取最小长度",
        description: "短问题复用规划结果和确定性抽取；达到该字符数的长报告再追加独立语义抽取模型调用。",
        value_type: EnvValueType::Integer,
        default_value: "900",
    },
    EnvVarDef {
        key: "PM_PREFACE_TURN_TIMEOUT_SECS",
        label: "preface 轮超时(秒)",
        description: "深研任务理解与规划轮次超时；失败时使用确定性任务图继续执行。",
        value_type: EnvValueType::Integer,
        default_value: "45",
    },
    EnvVarDef {
        key: "PM_DIRECT_ANSWER_TURN_TIMEOUT_SECS",
        label: "direct answer 轮超时(秒)",
        description: "产运助手直答分支超时。默认至少 300 秒，避免附件数据分析、长上下文汇总等非联网直答被误判失败。",
        value_type: EnvValueType::Integer,
        default_value: "300",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_TURN_TIMEOUT_SECS",
        label: "force synth 轮超时(秒)",
        description: "强制 synth 轮次超时。",
        value_type: EnvValueType::Integer,
        default_value: "150",
    },
    EnvVarDef {
        key: "PM_LLM_FINAL_EDITOR_MAX_ATTEMPTS",
        label: "最终编辑最大尝试数",
        description: "最终报告仅在格式或可读性需要修复时调用；失败后保留已校验原稿。",
        value_type: EnvValueType::Integer,
        default_value: "1",
    },
    EnvVarDef {
        key: "PM_LLM_FINAL_EDITOR_USE_PIPELINE_BUDGET",
        label: "最终编辑服从剩余预算",
        description: "最终编辑超时按深研流水线剩余时间收敛，避免格式修复形成长尾。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_LAST_CHANCE_SYNTH_TIMEOUT_SECS",
        label: "最终专家兜底超时(秒)",
        description: "当主 synth 和 transient synth 都失败时，最后一次无工具专家合成的超时时间；该阶段同时受共享合成总预算约束。",
        value_type: EnvValueType::Integer,
        default_value: "90",
    },
    EnvVarDef {
        key: "PM_CONTRACT_REPAIR_TURN_TIMEOUT_SECS",
        label: "contract repair 轮超时(秒)",
        description: "合同修复轮次超时。代码会强制不低于 60 秒，避免长 TASK_GRAPH/EXEC_CONSTRAINTS 修复被过早中断。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "PM_CONTRACT_REPAIR_MAX_RETRIES",
        label: "contract repair 最大重试",
        description: "合同修复最大重试次数。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "PM_TIMEOUT_RECOVERY_WAIT_SECS",
        label: "超时恢复等待(秒)",
        description: "超时恢复等待时长。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "PM_RETRY_BACKOFF_BASE_MS",
        label: "重试退避基线(ms)",
        description: "重试退避基线。",
        value_type: EnvValueType::Integer,
        default_value: "700",
    },
    EnvVarDef {
        key: "PM_RETRY_BACKOFF_MAX_MS",
        label: "重试退避上限(ms)",
        description: "重试退避上限。",
        value_type: EnvValueType::Integer,
        default_value: "12000",
    },
    EnvVarDef {
        key: "PM_RETRY_BACKOFF_JITTER_MS",
        label: "重试退避抖动(ms)",
        description: "重试退避随机抖动窗口。",
        value_type: EnvValueType::Integer,
        default_value: "450",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_MAX_CONCURRENT",
        label: "后台任务最大并发",
        description: "PM 后台任务最大并发。",
        value_type: EnvValueType::Integer,
        default_value: "4",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_MAX_IN_MEMORY",
        label: "后台任务内存队列上限",
        description: "PM 后台任务内存队列容量。",
        value_type: EnvValueType::Integer,
        default_value: "1000",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_EVENT_CHANNEL_CAPACITY",
        label: "任务事件通道容量",
        description: "后台任务事件通道容量。",
        value_type: EnvValueType::Integer,
        default_value: "1024",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_TTL_SECS",
        label: "后台任务 TTL(秒)",
        description: "后台任务保留时长。",
        value_type: EnvValueType::Integer,
        default_value: "3600",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_CLEANUP_INTERVAL_SECS",
        label: "后台任务清理周期(秒)",
        description: "后台任务清理间隔。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_LEASE_SECS",
        label: "后台任务租约(秒)",
        description: "后台任务租约时长。",
        value_type: EnvValueType::Integer,
        default_value: "180",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_HEARTBEAT_SECS",
        label: "后台任务心跳(秒)",
        description: "后台任务心跳间隔。",
        value_type: EnvValueType::Integer,
        default_value: "10",
    },
    EnvVarDef {
        key: "PM_RESEARCH_TASK_CLAIM_BATCH_SIZE",
        label: "后台任务 claim 批量",
        description: "后台任务 claim 批处理大小。",
        value_type: EnvValueType::Integer,
        default_value: "8",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_MAP_REDUCE_ENABLED",
        label: "启用 Map-Reduce 汇总",
        description: "将各 subtask 证据组织为本地 map 摘要，再进行一次全局模型综合。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_MAP_LLM_ENABLED",
        label: "Map 阶段使用模型",
        description: "是否为每个 subtask 额外调用模型生成 map 摘要；仅超长证据包建议开启。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_MAP_MIN_SUBTASKS",
        label: "Map 阶段最小 subtask 数",
        description: "触发 map 阶段的最小 subtask 数。",
        value_type: EnvValueType::Integer,
        default_value: "2",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_MAP_MAX_SUBTASKS",
        label: "Map 阶段最大 subtask 数",
        description: "map 阶段最多 subtask 数。",
        value_type: EnvValueType::Integer,
        default_value: "6",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_PACKET_EXCERPT_CHARS",
        label: "证据摘录字符上限",
        description: "map 输入证据摘录字符上限。",
        value_type: EnvValueType::Integer,
        default_value: "2600",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_MAP_CONTEXT_CHARS",
        label: "Map 上下文字符上限",
        description: "map prompt 上下文字符上限。",
        value_type: EnvValueType::Integer,
        default_value: "3200",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_MAP_SUMMARY_CHARS",
        label: "Map 摘要字符上限",
        description: "map 输出摘要字符上限。",
        value_type: EnvValueType::Integer,
        default_value: "1600",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_MAP_TIMEOUT_SECS",
        label: "Map 超时(秒)",
        description: "map 阶段超时时间。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "PM_FORCE_SYNTH_REDUCE_CONTEXT_CHARS",
        label: "Reduce 上下文字符上限",
        description: "reduce prompt 上下文字符上限。",
        value_type: EnvValueType::Integer,
        default_value: "22000",
    },
    EnvVarDef {
        key: "PM_TASK_IMAGE_MAX_COUNT",
        label: "任务图片最大数量",
        description: "PM 任务单次可上传图片数量上限。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "PM_TASK_IMAGE_MAX_BYTES",
        label: "单图大小上限(bytes)",
        description: "PM 任务单张图片大小上限。",
        value_type: EnvValueType::Integer,
        default_value: "8388608",
    },
    EnvVarDef {
        key: "PM_TASK_IMAGE_MAX_TOTAL_BYTES",
        label: "图片总大小上限(bytes)",
        description: "PM 任务图片附件总大小上限。",
        value_type: EnvValueType::Integer,
        default_value: "25165824",
    },
    EnvVarDef {
        key: "PM_TASK_IMAGE_SUMMARY_MODEL",
        label: "图片摘要模型",
        description: "图片附件摘要模型；留空时复用当前会话模型。",
        value_type: EnvValueType::String,
        default_value: "",
    },
    EnvVarDef {
        key: "PM_TASK_IMAGE_SUMMARY_MAX_TOKENS",
        label: "图片摘要最大 tokens",
        description: "图片附件摘要输出 token 上限。",
        value_type: EnvValueType::Integer,
        default_value: "1200",
    },
    EnvVarDef {
        key: "PM_TASK_IMAGE_SUMMARY_TIMEOUT_SECS",
        label: "图片摘要超时(秒)",
        description: "图片附件摘要模型调用超时。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "PM_TASK_DOCUMENT_MAX_COUNT",
        label: "任务文档最大数量",
        description: "PM 任务单次可上传文档数量上限。",
        value_type: EnvValueType::Integer,
        default_value: "8",
    },
    EnvVarDef {
        key: "PM_TASK_DOCUMENT_MAX_BYTES",
        label: "单文档大小上限(bytes)",
        description: "PM 任务单个文档大小上限。",
        value_type: EnvValueType::Integer,
        default_value: "10485760",
    },
    EnvVarDef {
        key: "PM_TASK_DOCUMENT_MAX_TOTAL_CHARS",
        label: "文档总字符上限",
        description: "PM 任务文档解析后的总字符上限。",
        value_type: EnvValueType::Integer,
        default_value: "48000",
    },
    EnvVarDef {
        key: "PM_TASK_DOCUMENT_MAX_CHARS_PER_FILE",
        label: "单文档字符上限",
        description: "PM 任务单个文档解析后的字符上限。",
        value_type: EnvValueType::Integer,
        default_value: "16000",
    },
    EnvVarDef {
        key: "PM_SUNO_POLL_TIMEOUT_SECS",
        label: "Suno 轮询超时(秒)",
        description: "PM 音乐生成任务等待 Suno 结果的最长时间。",
        value_type: EnvValueType::Integer,
        default_value: "180",
    },
    EnvVarDef {
        key: "PM_SUNO_POLL_INTERVAL_MS",
        label: "Suno 轮询间隔(ms)",
        description: "PM 音乐生成任务轮询 Suno 状态的间隔。",
        value_type: EnvValueType::Integer,
        default_value: "2000",
    },
    EnvVarDef {
        key: "PM_MUSIC_AUDIO_FORMAT",
        label: "音乐音频格式",
        description: "PM 音乐/语音生成的音频格式。",
        value_type: EnvValueType::String,
        default_value: "mp3",
    },
    EnvVarDef {
        key: "PM_MUSIC_AUDIO_VOICE",
        label: "音乐语音音色",
        description: "PM 音乐/语音生成的默认 voice 参数。",
        value_type: EnvValueType::String,
        default_value: "alloy",
    },
    EnvVarDef {
        key: "BOT_GATEWAY_INBOUND_POLL_SECS",
        label: "Bot 入站轮询(秒)",
        description: "Bot Gateway 入站通道后台轮询间隔。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "MCP_CHECK_INTERVAL_SECS",
        label: "MCP 健康检查周期(秒)",
        description: "周期性检查已启用 MCP Server 的间隔；0 表示关闭。",
        value_type: EnvValueType::Integer,
        default_value: "300",
    },
    EnvVarDef {
        key: "SCHEMA_REFRESH_INTERVAL_SECS",
        label: "Schema 刷新周期(秒)",
        description: "数据源 schema 后台刷新周期；0 表示关闭。",
        value_type: EnvValueType::Integer,
        default_value: "3600",
    },
    EnvVarDef {
        key: "UPLOAD_MAX_FILE_BYTES",
        label: "上传文件大小上限(bytes)",
        description: "通用上传接口单文件大小上限。",
        value_type: EnvValueType::Integer,
        default_value: "52428800",
    },
    EnvVarDef {
        key: "UPLOAD_MAX_IMAGE_BYTES",
        label: "上传图片大小上限(bytes)",
        description: "通用上传接口单图片大小上限。",
        value_type: EnvValueType::Integer,
        default_value: "1048576",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_TIMEOUT_SECS",
        label: "Web 搜索超时(秒)",
        description: "搜索 provider 请求超时。",
        value_type: EnvValueType::Integer,
        default_value: "12",
    },
    EnvVarDef {
        key: "AOSD_WEB_CONNECT_TIMEOUT_SECS",
        label: "Web 搜索连接超时(秒)",
        description: "搜索 provider HTTP 连接超时。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_MAX_RESULTS",
        label: "Web 搜索结果上限",
        description: "单次 WebSearch 返回候选结果上限。",
        value_type: EnvValueType::Integer,
        default_value: "20",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_OUTPUT_HITS",
        label: "Web 搜索输出条数",
        description: "写入工具输出摘要的搜索结果条数。",
        value_type: EnvValueType::Integer,
        default_value: "12",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_MAX_RETRIES",
        label: "Web 搜索最大重试",
        description: "搜索 provider 请求失败后的最大重试次数。",
        value_type: EnvValueType::Integer,
        default_value: "1",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_RETRY_BACKOFF_MS",
        label: "Web 搜索重试退避(ms)",
        description: "搜索 provider 重试基础退避时间。",
        value_type: EnvValueType::Integer,
        default_value: "200",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_RETRY_JITTER_MS",
        label: "Web 搜索重试抖动(ms)",
        description: "搜索 provider 重试随机抖动窗口。",
        value_type: EnvValueType::Integer,
        default_value: "120",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_KEY_COOLDOWN_SECS",
        label: "搜索 Key 冷却(秒)",
        description: "单个搜索 API key 被限流后的冷却时间。",
        value_type: EnvValueType::Integer,
        default_value: "75",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_COUNTRY",
        label: "Web 搜索国家",
        description: "搜索 country 参数，例如 US/CN/JP；留空使用 provider 默认。",
        value_type: EnvValueType::String,
        default_value: "",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_LANGUAGE",
        label: "Web 搜索语言",
        description: "搜索语言参数，例如 en/zh-hans；留空使用 provider 默认。",
        value_type: EnvValueType::String,
        default_value: "",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_LOCATION",
        label: "Web 搜索位置",
        description: "预留位置参数，供搜索 provider 或后续路由使用。",
        value_type: EnvValueType::String,
        default_value: "",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_ENABLED",
        label: "启用搜索结果富化",
        description: "WebSearch 是否抓取候选页面正文进行富化。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_TARGET_VALID_PAGES",
        label: "富化目标有效页数",
        description: "搜索富化希望获得的有效页面数量。",
        value_type: EnvValueType::Integer,
        default_value: "8",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_INITIAL_FETCH_CANDIDATES",
        label: "富化初始抓取候选数",
        description: "搜索富化初始抓取候选页面数量。",
        value_type: EnvValueType::Integer,
        default_value: "8",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_MAX_FETCH_CANDIDATES",
        label: "富化最大抓取候选数",
        description: "搜索富化最大抓取候选页面数量。",
        value_type: EnvValueType::Integer,
        default_value: "20",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_MIN_CHARS",
        label: "富化最小正文字符",
        description: "候选页面被视为有效内容的最小字符数。",
        value_type: EnvValueType::Integer,
        default_value: "450",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_MAX_CHARS",
        label: "富化最大正文字符",
        description: "单个富化页面保留的最大字符数。",
        value_type: EnvValueType::Integer,
        default_value: "12000",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_FETCH_TIMEOUT_SECS",
        label: "富化抓取超时(秒)",
        description: "搜索富化页面抓取超时。",
        value_type: EnvValueType::Integer,
        default_value: "12",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_CONNECT_TIMEOUT_SECS",
        label: "富化连接超时(秒)",
        description: "搜索富化页面抓取连接超时。",
        value_type: EnvValueType::Integer,
        default_value: "4",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_ATTEMPTS",
        label: "富化抓取重试次数",
        description: "搜索富化页面抓取失败后的重试次数。",
        value_type: EnvValueType::Integer,
        default_value: "2",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_BACKOFF_MS",
        label: "富化重试退避(ms)",
        description: "搜索富化抓取重试基础退避时间。",
        value_type: EnvValueType::Integer,
        default_value: "350",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_JITTER_MS",
        label: "富化重试抖动(ms)",
        description: "搜索富化抓取重试随机抖动窗口。",
        value_type: EnvValueType::Integer,
        default_value: "220",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_JINA_FALLBACK_ENABLED",
        label: "启用 Jina 富化兜底",
        description: "普通抓取失败时是否启用 Jina reader 兜底。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOSD_WEB_SEARCH_ENRICH_DOMAIN_REPEAT_PENALTY",
        label: "富化同域惩罚",
        description: "搜索富化排序中同域名重复内容的惩罚系数。",
        value_type: EnvValueType::Float,
        default_value: "0.08",
    },
];

const ANALYTICS_ENV_DEFS: &[EnvVarDef] = &[
    EnvVarDef {
        key: "NL2SQL_TOP_K_TABLES_FOR_LLM",
        label: "候选表 Top-K",
        description: "传给 LLM 的候选表数量。",
        value_type: EnvValueType::Integer,
        default_value: "20",
    },
    EnvVarDef {
        key: "NL2SQL_MIN_TABLE_SIM",
        label: "最小表相似度",
        description: "表匹配相似度下限。",
        value_type: EnvValueType::Float,
        default_value: "0.2",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_AGENT_STEPS",
        label: "Agent 最大步骤数",
        description: "多步执行计划最大步骤。",
        value_type: EnvValueType::Integer,
        default_value: "10",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_CROSS_DS_TABLES",
        label: "跨源最大表数",
        description: "跨数据源计划最大表数。",
        value_type: EnvValueType::Integer,
        default_value: "4",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_CROSS_DS_ROWS",
        label: "跨源最大行数",
        description: "跨数据源执行最大行数。",
        value_type: EnvValueType::Integer,
        default_value: "10000",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_ROWS_PER_STEP",
        label: "单步最大行数",
        description: "多步执行单步最大行数。",
        value_type: EnvValueType::Integer,
        default_value: "10000",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_AGENT_RESPONSE_ROWS",
        label: "Agent 响应最大行数",
        description: "agent 汇总返回前裁剪行数。",
        value_type: EnvValueType::Integer,
        default_value: "300",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_SELF_CORRECT_ATTEMPTS",
        label: "自纠错最大次数",
        description: "SQL 执行失败后的自纠错轮次。",
        value_type: EnvValueType::Integer,
        default_value: "2",
    },
    EnvVarDef {
        key: "NL2SQL_CONVERSATION_SUMMARY_THRESHOLD",
        label: "会话摘要阈值",
        description: "触发会话摘要的消息数阈值。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "NL2SQL_ENABLE_QUERY_UNDERSTANDING",
        label: "启用 Query Understanding",
        description: "是否启用 Query Understanding。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_ENABLE_RESULT_VALIDATION",
        label: "启用结果校验",
        description: "是否启用执行结果校验。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_ENABLE_DOMAIN_ROUTING",
        label: "启用领域路由",
        description: "是否启用 business domain 路由上下文。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_RESULT_ROWS",
        label: "结果最大行数",
        description: "执行结果硬上限。",
        value_type: EnvValueType::Integer,
        default_value: "10000",
    },
    EnvVarDef {
        key: "NL2SQL_ROUTING_LLM_MODEL",
        label: "Routing LLM 模型",
        description: "路由阶段 LLM 默认模型。",
        value_type: EnvValueType::String,
        default_value: "gpt-4o-mini",
    },
    EnvVarDef {
        key: "NL2SQL_ROUTING_LLM_TIMEOUT_SECS",
        label: "Routing LLM 超时(秒)",
        description: "路由阶段 LLM 调用超时。",
        value_type: EnvValueType::Integer,
        default_value: "180",
    },
    EnvVarDef {
        key: "NL2SQL_USE_ANN_INDEX",
        label: "启用 ANN 索引",
        description: "向量检索是否使用 ANN。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_DS_EMBED_PRE_FILTER",
        label: "启用数据源向量预过滤",
        description: "是否按 datasource embedding 做预过滤。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_DS_EMBED_THRESHOLD",
        label: "数据源向量阈值",
        description: "datasource embedding 过滤阈值。",
        value_type: EnvValueType::Float,
        default_value: "0.3",
    },
    EnvVarDef {
        key: "NL2SQL_USE_RRFS",
        label: "启用 RRFS 融合",
        description: "是否启用 RRFS 多路融合。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_RRFS_SHORT_CIRCUIT",
        label: "RRFS 短路",
        description: "强置信场景下是否短路 RRFS。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_RRFS_K",
        label: "RRFS K",
        description: "RRFS 融合参数 K。",
        value_type: EnvValueType::Float,
        default_value: "60.0",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_TABLES_PER_DATASOURCE",
        label: "每数据源最大表数",
        description: "schema discover 单数据源最大表数。",
        value_type: EnvValueType::Integer,
        default_value: "100000",
    },
    EnvVarDef {
        key: "NL2SQL_CLICKHOUSE_FK_INFERENCE",
        label: "启用 ClickHouse FK 推断",
        description: "是否启用采样 FK 推断。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "NL2SQL_CH_FK_COVERAGE_THRESH",
        label: "CH FK 覆盖率阈值",
        description: "ClickHouse FK 识别覆盖率阈值。",
        value_type: EnvValueType::Float,
        default_value: "0.85",
    },
    EnvVarDef {
        key: "NL2SQL_CH_FK_CARD_RATIO_THRESH",
        label: "CH FK 基数比阈值",
        description: "ClickHouse FK 识别基数比阈值。",
        value_type: EnvValueType::Float,
        default_value: "1.2",
    },
    EnvVarDef {
        key: "NL2SQL_COLLECT_STATS",
        label: "采集统计信息",
        description: "refresh 时是否采集表/列统计。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "NL2SQL_RESULT_CACHE_TTL_HOURS",
        label: "结果缓存 TTL(小时)",
        description: "结果缓存有效期。",
        value_type: EnvValueType::Integer,
        default_value: "1",
    },
    EnvVarDef {
        key: "NL2SQL_RESULT_CACHE_MAX_ROWS",
        label: "缓存最大行数",
        description: "结果缓存单次最大行数。",
        value_type: EnvValueType::Integer,
        default_value: "1000",
    },
    EnvVarDef {
        key: "NL2SQL_DS_POOL_MAX_CONNS",
        label: "数据源连接池最大连接",
        description: "每数据源连接池最大连接数。",
        value_type: EnvValueType::Integer,
        default_value: "4",
    },
    EnvVarDef {
        key: "NL2SQL_DS_POOL_IDLE_SECS",
        label: "连接池空闲回收(秒)",
        description: "连接池空闲超时。",
        value_type: EnvValueType::Integer,
        default_value: "300",
    },
    EnvVarDef {
        key: "NL2SQL_DS_POOL_ACQUIRE_SECS",
        label: "连接池获取超时(秒)",
        description: "连接池 acquire 超时。",
        value_type: EnvValueType::Integer,
        default_value: "30",
    },
    EnvVarDef {
        key: "NL2SQL_LLM_RATE_LIMIT_RPM",
        label: "LLM 限流 RPM",
        description: "每租户 LLM 请求速率上限。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "NL2SQL_DISTRIBUTED_RATE_LIMIT",
        label: "分布式限流",
        description: "是否启用 DB 分布式限流。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "NL2SQL_QU_CACHE_TTL_MINUTES",
        label: "QU 缓存 TTL(分钟)",
        description: "Query Understanding 缓存 TTL。",
        value_type: EnvValueType::Integer,
        default_value: "30",
    },
    EnvVarDef {
        key: "NL2SQL_MAX_CLARIFICATION_TURNS",
        label: "澄清最大轮次",
        description: "进入软兜底前的澄清轮次。",
        value_type: EnvValueType::Integer,
        default_value: "5",
    },
    EnvVarDef {
        key: "NL2SQL_SOFT_FALLBACK_GRANULARITY",
        label: "软兜底时间粒度",
        description: "澄清超限后的默认时间粒度。",
        value_type: EnvValueType::String,
        default_value: "daily",
    },
    EnvVarDef {
        key: "NL2SQL_QUERY_TASK_MAX_CONCURRENT",
        label: "Query Task 最大并发",
        description: "Query 异步任务并发上限。",
        value_type: EnvValueType::Integer,
        default_value: "8",
    },
    EnvVarDef {
        key: "NL2SQL_QUERY_TASK_MAX_IN_MEMORY",
        label: "Query Task 内存上限",
        description: "Query 异步任务内存容量。",
        value_type: EnvValueType::Integer,
        default_value: "2000",
    },
    EnvVarDef {
        key: "NL2SQL_QUERY_TASK_TTL_SECS",
        label: "Query Task TTL(秒)",
        description: "Query 异步任务保留时长。",
        value_type: EnvValueType::Integer,
        default_value: "1800",
    },
    EnvVarDef {
        key: "NL2SQL_QUERY_TASK_CLEANUP_INTERVAL_SECS",
        label: "Query Task 清理周期(秒)",
        description: "Query 异步任务清理间隔。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "NL2SQL_AGENT_TASK_MAX_CONCURRENT",
        label: "Agent Task 最大并发",
        description: "Agent 异步任务并发上限。",
        value_type: EnvValueType::Integer,
        default_value: "8",
    },
    EnvVarDef {
        key: "NL2SQL_AGENT_TASK_MAX_IN_MEMORY",
        label: "Agent Task 内存上限",
        description: "Agent 异步任务内存容量。",
        value_type: EnvValueType::Integer,
        default_value: "2000",
    },
    EnvVarDef {
        key: "NL2SQL_AGENT_TASK_TTL_SECS",
        label: "Agent Task TTL(秒)",
        description: "Agent 异步任务保留时长。",
        value_type: EnvValueType::Integer,
        default_value: "1800",
    },
    EnvVarDef {
        key: "NL2SQL_AGENT_TASK_CLEANUP_INTERVAL_SECS",
        label: "Agent Task 清理周期(秒)",
        description: "Agent 异步任务清理间隔。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "NL2SQL_CLARIFY_TASK_MAX_CONCURRENT",
        label: "Clarify Task 最大并发",
        description: "Clarify 异步任务并发上限。",
        value_type: EnvValueType::Integer,
        default_value: "8",
    },
    EnvVarDef {
        key: "NL2SQL_CLARIFY_TASK_MAX_IN_MEMORY",
        label: "Clarify Task 内存上限",
        description: "Clarify 异步任务内存容量。",
        value_type: EnvValueType::Integer,
        default_value: "2000",
    },
    EnvVarDef {
        key: "NL2SQL_CLARIFY_TASK_TTL_SECS",
        label: "Clarify Task TTL(秒)",
        description: "Clarify 异步任务保留时长。",
        value_type: EnvValueType::Integer,
        default_value: "1800",
    },
    EnvVarDef {
        key: "NL2SQL_CLARIFY_TASK_CLEANUP_INTERVAL_SECS",
        label: "Clarify Task 清理周期(秒)",
        description: "Clarify 异步任务清理间隔。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "NL2SQL_ROUTE_TASK_MAX_CONCURRENT",
        label: "Route Task 最大并发",
        description: "Route 异步任务并发上限。",
        value_type: EnvValueType::Integer,
        default_value: "16",
    },
    EnvVarDef {
        key: "NL2SQL_ROUTE_TASK_MAX_IN_MEMORY",
        label: "Route Task 内存上限",
        description: "Route 异步任务内存容量。",
        value_type: EnvValueType::Integer,
        default_value: "2000",
    },
    EnvVarDef {
        key: "NL2SQL_ROUTE_TASK_TTL_SECS",
        label: "Route Task TTL(秒)",
        description: "Route 异步任务保留时长。",
        value_type: EnvValueType::Integer,
        default_value: "1800",
    },
    EnvVarDef {
        key: "NL2SQL_ROUTE_TASK_CLEANUP_INTERVAL_SECS",
        label: "Route Task 清理周期(秒)",
        description: "Route 异步任务清理间隔。",
        value_type: EnvValueType::Integer,
        default_value: "60",
    },
    EnvVarDef {
        key: "NL2SQL_ROUTE_TASK_HARD_TIMEOUT_SECS",
        label: "Route Task 硬超时(秒)",
        description: "Route 异步任务硬超时。",
        value_type: EnvValueType::Integer,
        default_value: "420",
    },
    EnvVarDef {
        key: "NL2SQL_ANN_SNAPSHOT_INTERVAL_SECS",
        label: "ANN 快照周期(秒)",
        description: "ANN 索引落盘周期。",
        value_type: EnvValueType::Integer,
        default_value: "30",
    },
    EnvVarDef {
        key: "NL2SQL_EMBEDDING_DB_CHECK",
        label: "Embedding DB 诊断级别",
        description: "embedding 数据库诊断强度：off/basic/full。",
        value_type: EnvValueType::String,
        default_value: "basic",
    },
];

const ENGINEERING_ENV_DEFS: &[EnvVarDef] = &[
    EnvVarDef {
        key: "AOS_RD_RUNTIME_EXECUTOR",
        label: "Code runtime 执行器",
        description: "研发代码任务默认走 agent-gateway runtime。保持 runtime 可复用 CLI 级读码、工具、MCP、Skills 与 Hooks 能力；仅在兼容排障时改为 direct/completion。",
        value_type: EnvValueType::String,
        default_value: "runtime",
    },
    EnvVarDef {
        key: "AOS_RD_RUNTIME_TIMEOUT_SECS",
        label: "Code runtime 超时(秒)",
        description: "单次研发 runtime turn 的最长等待时间。代码库理解、候选工作区编辑和 Diff 生成通常比普通问答慢，默认 1800 秒。",
        value_type: EnvValueType::Integer,
        default_value: "1800",
    },
    EnvVarDef {
        key: "AOS_RD_TEST_COMMAND_TIMEOUT_SECS",
        label: "Code 测试命令超时(秒)",
        description: "用户确认后运行测试/验证命令的最长等待时间。默认 180 秒；大型 Java/前端项目可适当调高。",
        value_type: EnvValueType::Integer,
        default_value: "180",
    },
    EnvVarDef {
        key: "AOS_RD_AUTO_REPAIR_MAX_ATTEMPTS",
        label: "Code 自动修复轮次",
        description: "测试失败后自动读取错误并生成修复 Diff 的最大轮次。默认 3 轮；设为 0 可关闭自动修复。",
        value_type: EnvValueType::Integer,
        default_value: "3",
    },
    EnvVarDef {
        key: "AOS_RD_RUNTIME_DIRECT_FALLBACK",
        label: "允许直连模型回退",
        description: "runtime 失败时是否允许退回普通模型调用。默认关闭，避免 Code 开发能力静默退化为无工具聊天。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "AOS_RD_RUNTIME_BASH_ENABLED",
        label: "允许 runtime Bash",
        description: "研发 runtime 是否允许 Bash 工具。默认关闭，避免无审批扩大执行面。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "AOS_RD_RUNTIME_WRITE_TOOLS_ENABLED",
        label: "允许 runtime 写工具",
        description: "研发 runtime 是否允许写文件类工具。默认关闭，候选工作区编辑由受控流程承担。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "AOS_RD_CANDIDATE_BASH_ENABLED",
        label: "允许候选 Bash",
        description: "候选工作区生成/修复阶段是否允许 Bash 工具。",
        value_type: EnvValueType::Bool,
        default_value: "false",
    },
    EnvVarDef {
        key: "AOS_RD_CANDIDATE_WORKTREE_ENABLED",
        label: "启用候选工作区",
        description: "研发任务是否使用隔离候选 worktree 生成 Diff。默认开启以降低污染真实仓库风险。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOS_RD_CANDIDATE_VERIFY_ENABLED",
        label: "启用候选验证",
        description: "候选工作区是否执行测试命令并基于失败结果继续修复。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOS_RD_CANDIDATE_MAX_FIX_ATTEMPTS",
        label: "候选最大修复轮次",
        description: "候选工作区测试失败后的追加修复轮次。",
        value_type: EnvValueType::Integer,
        default_value: "2",
    },
    EnvVarDef {
        key: "AOS_RD_REVIEWER_PASS_ENABLED",
        label: "启用独立 Review Agent",
        description: "主 Coding Agent 生成 Diff 后，是否再用研发 runtime 启动一轮独立审查，补充风险、缺失测试和 PR 建议。默认开启以优先保证代码质量。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOS_RD_ARCHITECTURE_PASS_ENABLED",
        label: "启用 Architecture Agent",
        description: "仓库绑定任务开始前，是否先用研发 runtime 读取真实仓库并生成架构/相关文件/验证建议，作为主 Coding Agent 的上下文。默认开启以提升定位质量。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOS_RD_LLM_CONTEXT_SUMMARY_ENABLED",
        label: "启用上下文摘要",
        description: "研发 LLM 请求是否启用历史上下文摘要压缩。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOS_RD_LLM_CONTEXT_PLANNER_ENABLED",
        label: "启用上下文规划",
        description: "研发 LLM 请求是否启用上下文规划与候选文件收敛。",
        value_type: EnvValueType::Bool,
        default_value: "true",
    },
    EnvVarDef {
        key: "AOS_RD_WORKFLOW_POST_STAGE_PASSES",
        label: "Workflow 后置阶段上限",
        description: "多 Agent 工作流主实现后的 review/test/pr 等后置阶段数量上限。",
        value_type: EnvValueType::Integer,
        default_value: "4",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const EXPECTED_MANAGED_ENV_KEYS: &[&str] = &[
        "AOSD_WEB_CONNECT_TIMEOUT_SECS",
        "AOSD_WEB_SEARCH_COUNTRY",
        "AOSD_WEB_SEARCH_ENRICH_CONNECT_TIMEOUT_SECS",
        "AOSD_WEB_SEARCH_ENRICH_DOMAIN_REPEAT_PENALTY",
        "AOSD_WEB_SEARCH_ENRICH_ENABLED",
        "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_ATTEMPTS",
        "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_BACKOFF_MS",
        "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_JITTER_MS",
        "AOSD_WEB_SEARCH_ENRICH_FETCH_TIMEOUT_SECS",
        "AOSD_WEB_SEARCH_ENRICH_INITIAL_FETCH_CANDIDATES",
        "AOSD_WEB_SEARCH_ENRICH_JINA_FALLBACK_ENABLED",
        "AOSD_WEB_SEARCH_ENRICH_MAX_CHARS",
        "AOSD_WEB_SEARCH_ENRICH_MAX_FETCH_CANDIDATES",
        "AOSD_WEB_SEARCH_ENRICH_MIN_CHARS",
        "AOSD_WEB_SEARCH_ENRICH_TARGET_VALID_PAGES",
        "AOSD_WEB_SEARCH_KEY_COOLDOWN_SECS",
        "AOSD_WEB_SEARCH_LANGUAGE",
        "AOSD_WEB_SEARCH_LOCATION",
        "AOSD_WEB_SEARCH_MAX_RESULTS",
        "AOSD_WEB_SEARCH_MAX_RETRIES",
        "AOSD_WEB_SEARCH_OUTPUT_HITS",
        "AOSD_WEB_SEARCH_RETRY_BACKOFF_MS",
        "AOSD_WEB_SEARCH_RETRY_JITTER_MS",
        "AOSD_WEB_SEARCH_TIMEOUT_SECS",
        "AOSD_GITHUB_TOKEN",
        "AOS_RD_ARCHITECTURE_PASS_ENABLED",
        "AOS_RD_AUTO_REPAIR_MAX_ATTEMPTS",
        "AOS_RD_CANDIDATE_BASH_ENABLED",
        "AOS_RD_CANDIDATE_MAX_FIX_ATTEMPTS",
        "AOS_RD_CANDIDATE_VERIFY_ENABLED",
        "AOS_RD_CANDIDATE_WORKTREE_ENABLED",
        "AOS_RD_LLM_CONTEXT_PLANNER_ENABLED",
        "AOS_RD_LLM_CONTEXT_SUMMARY_ENABLED",
        "AOS_RD_REVIEWER_PASS_ENABLED",
        "AOS_RD_RUNTIME_BASH_ENABLED",
        "AOS_RD_RUNTIME_DIRECT_FALLBACK",
        "AOS_RD_RUNTIME_EXECUTOR",
        "AOS_RD_RUNTIME_TIMEOUT_SECS",
        "AOS_RD_RUNTIME_WRITE_TOOLS_ENABLED",
        "AOS_RD_TEST_COMMAND_TIMEOUT_SECS",
        "AOS_RD_WORKFLOW_POST_STAGE_PASSES",
        "BOT_GATEWAY_INBOUND_POLL_SECS",
        "MCP_CHECK_INTERVAL_SECS",
        "NL2SQL_EMBEDDING_DB_CHECK",
        "PM_MUSIC_AUDIO_FORMAT",
        "PM_MUSIC_AUDIO_VOICE",
        "PM_SUNO_POLL_INTERVAL_MS",
        "PM_SUNO_POLL_TIMEOUT_SECS",
        "PM_TASK_DOCUMENT_MAX_BYTES",
        "PM_TASK_DOCUMENT_MAX_CHARS_PER_FILE",
        "PM_TASK_DOCUMENT_MAX_COUNT",
        "PM_TASK_DOCUMENT_MAX_TOTAL_CHARS",
        "PM_TASK_IMAGE_MAX_BYTES",
        "PM_TASK_IMAGE_MAX_COUNT",
        "PM_TASK_IMAGE_MAX_TOTAL_BYTES",
        "PM_TASK_IMAGE_SUMMARY_MAX_TOKENS",
        "PM_TASK_IMAGE_SUMMARY_MODEL",
        "PM_TASK_IMAGE_SUMMARY_TIMEOUT_SECS",
        "SCHEMA_REFRESH_INTERVAL_SECS",
        "UPLOAD_MAX_FILE_BYTES",
        "UPLOAD_MAX_IMAGE_BYTES",
    ];

    const SECURITY_BOUNDARY_ENV_KEYS: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "AOS_SQLITE_ACQUIRE_TIMEOUT_SECS",
        "AOS_SQLITE_BUSY_TIMEOUT_MS",
        "AOS_SQLITE_MAX_CONNECTIONS",
        "AOS_STRICT_ENCRYPTION",
        "JWT_SECRET",
        "OPENAI_API_KEY",
        "SMTP_PASSWORD",
    ];

    const PROVIDER_ENV_KEYS_NOT_MANAGED: &[&str] = &[
        "AOSD_BRAVE_API_KEY",
        "AOSD_BRAVE_BASE_URL",
        "AOSD_EXA_API_KEY",
        "AOSD_EXA_BASE_URL",
        "AOSD_SERPER_API_KEY",
        "AOSD_SERPER_BASE_URL",
        "AOSD_TAVILY_API_KEY",
        "AOSD_TAVILY_BASE_URL",
        "AOSD_TAVILY_SEARCH_DEPTH",
        "AOSD_WEB_SEARCH_PROVIDER",
        "AOSD_WEB_SEARCH_PROVIDER_ORDER",
    ];

    #[test]
    fn config_management_contains_expected_safe_runtime_knobs() {
        let managed_keys = all_env_defs().map(|def| def.key).collect::<BTreeSet<_>>();
        for key in EXPECTED_MANAGED_ENV_KEYS {
            assert!(
                managed_keys.contains(key),
                "missing managed config key: {key}"
            );
        }
    }

    #[test]
    fn config_management_keeps_secrets_and_bootstrap_keys_out() {
        let managed_keys = all_env_defs().map(|def| def.key).collect::<BTreeSet<_>>();
        for key in SECURITY_BOUNDARY_ENV_KEYS {
            assert!(
                !managed_keys.contains(key),
                "security boundary key must not be editable from config management: {key}"
            );
        }
    }

    #[test]
    fn config_management_keeps_search_provider_env_keys_out() {
        let managed_keys = all_env_defs().map(|def| def.key).collect::<BTreeSet<_>>();
        for key in PROVIDER_ENV_KEYS_NOT_MANAGED {
            assert!(
                !managed_keys.contains(key),
                "Search Extension env key must be configured from the DB-backed Search Extensions page, not config management: {key}"
            );
        }
    }

    #[test]
    fn config_management_masks_secret_values() {
        assert_eq!(mask_secret_value("short"), "********");
        assert_eq!(mask_secret_value("brave-secret-value"), "brav****alue");
    }

    #[test]
    fn github_token_is_managed_as_a_secret() {
        let def = find_env_def("AOSD_GITHUB_TOKEN").expect("GitHub token config");
        assert!(matches!(def.value_type, EnvValueType::Secret));
    }

    #[test]
    fn config_management_keys_are_unique() {
        let mut seen = BTreeSet::new();
        for def in all_env_defs() {
            assert!(
                seen.insert(def.key),
                "duplicate managed config key: {}",
                def.key
            );
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvConfigEntry {
    key: String,
    label: String,
    description: String,
    value_type: EnvValueType,
    value: String,
    default_value: String,
    source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigManagementTab {
    env: Vec<EnvConfigEntry>,
    pm_budget_profile: Option<PmBudgetProfileDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigManagementOverview {
    operations: ConfigManagementTab,
    analytics: ConfigManagementTab,
    engineering: ConfigManagementTab,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PmBudgetProfileDto {
    profile_key: String,
    enabled: bool,
    is_default: bool,
    priority: i32,
    pipeline_timeout_secs: i32,
    max_attempts: i32,
    retrieve_max_tool_calls: i32,
    max_calls_per_source: i32,
    source_slot_search_secs: i32,
    source_slot_browser_secs: i32,
    source_slot_api_fetch_secs: i32,
    preflight_model_timeout_secs: i32,
    preflight_probe_timeout_secs: i32,
    preflight_overall_timeout_secs: i32,
    retry_step_budget_secs: i32,
    retry_total_budget_secs: i32,
    source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePmBudgetProfileRequest {
    profile_key: Option<String>,
    enabled: Option<bool>,
    is_default: Option<bool>,
    priority: Option<i32>,
    pipeline_timeout_secs: Option<i32>,
    max_attempts: Option<i32>,
    retrieve_max_tool_calls: Option<i32>,
    max_calls_per_source: Option<i32>,
    source_slot_search_secs: Option<i32>,
    source_slot_browser_secs: Option<i32>,
    source_slot_api_fetch_secs: Option<i32>,
    preflight_model_timeout_secs: Option<i32>,
    preflight_probe_timeout_secs: Option<i32>,
    preflight_overall_timeout_secs: Option<i32>,
    retry_step_budget_secs: Option<i32>,
    retry_total_budget_secs: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateEnvConfigRequest {
    key: String,
    value: Option<String>,
    clear: Option<bool>,
}

fn ensure_admin(claims: &Claims) -> Result<()> {
    if claims.role == "admin" || claims.role == "superadmin" {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn normalize_bool_input(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some("true"),
        "0" | "false" | "no" | "off" => Some("false"),
        _ => None,
    }
}

fn all_env_defs() -> impl Iterator<Item = &'static EnvVarDef> {
    OPS_ENV_DEFS
        .iter()
        .chain(ANALYTICS_ENV_DEFS.iter())
        .chain(ENGINEERING_ENV_DEFS.iter())
}

fn find_env_def(key: &str) -> Option<&'static EnvVarDef> {
    all_env_defs().find(|def| def.key == key)
}

fn build_env_entry(def: &EnvVarDef) -> EnvConfigEntry {
    let current = std::env::var(def.key).ok();
    let (value, source) = if let Some(v) = current {
        let value = match def.value_type {
            EnvValueType::Secret => mask_secret_value(&v),
            _ => v,
        };
        (value, "env".to_string())
    } else {
        (def.default_value.to_string(), "code_default".to_string())
    };
    EnvConfigEntry {
        key: def.key.to_string(),
        label: def.label.to_string(),
        description: def.description.to_string(),
        value_type: def.value_type,
        value,
        default_value: def.default_value.to_string(),
        source,
    }
}

fn mask_secret_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let char_count = trimmed.chars().count();
    if char_count <= 8 {
        return "********".to_string();
    }
    let prefix = trimmed.chars().take(4).collect::<String>();
    let suffix = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{prefix}****{suffix}")
}

async fn load_pm_budget_profile(state: &AppState, tenant_id: &str) -> PmBudgetProfileDto {
    let mut row = sqlx::query(
        "SELECT profile_key, enabled, is_default, priority,
                pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls, max_calls_per_source,
                source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
                preflight_model_timeout_secs, preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                retry_step_budget_secs, retry_total_budget_secs
         FROM pm_budget_profiles
         WHERE tenant_id = ? AND is_default = 1
         ORDER BY priority DESC, updated_at DESC
         LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    if row.is_none() {
        row = sqlx::query(
            "SELECT profile_key, enabled, is_default, priority,
                    pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls, max_calls_per_source,
                    source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
                    preflight_model_timeout_secs, preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                    retry_step_budget_secs, retry_total_budget_secs
             FROM pm_budget_profiles
             WHERE tenant_id = ? AND profile_key = 'normal'
             ORDER BY updated_at DESC
             LIMIT 1",
        )
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    }

    if let Some(row) = row {
        return PmBudgetProfileDto {
            profile_key: row.get::<String, _>(0),
            enabled: row.get::<bool, _>(1),
            is_default: row.get::<bool, _>(2),
            priority: row.get::<i32, _>(3),
            pipeline_timeout_secs: row.get::<i32, _>(4),
            max_attempts: row.get::<i32, _>(5),
            retrieve_max_tool_calls: row.get::<i32, _>(6),
            max_calls_per_source: row.get::<i32, _>(7),
            source_slot_search_secs: row.get::<i32, _>(8),
            source_slot_browser_secs: row.get::<i32, _>(9),
            source_slot_api_fetch_secs: row.get::<i32, _>(10),
            preflight_model_timeout_secs: row.get::<i32, _>(11),
            preflight_probe_timeout_secs: row.get::<i32, _>(12),
            preflight_overall_timeout_secs: row.get::<i32, _>(13),
            retry_step_budget_secs: row.get::<i32, _>(14),
            retry_total_budget_secs: row.get::<i32, _>(15),
            source: "table".to_string(),
        };
    }

    let defaults = PmTimeoutBudget::baseline_for_profile(PmBudgetProfile::Normal);
    PmBudgetProfileDto {
        profile_key: "normal".to_string(),
        enabled: true,
        is_default: true,
        priority: 100,
        pipeline_timeout_secs: i32::try_from(defaults.pipeline_timeout_secs).unwrap_or(360),
        max_attempts: i32::try_from(defaults.max_attempts).unwrap_or(2),
        retrieve_max_tool_calls: i32::try_from(defaults.retrieve_max_tool_calls).unwrap_or(6),
        max_calls_per_source: i32::try_from(defaults.max_calls_per_source).unwrap_or(3),
        source_slot_search_secs: i32::try_from(defaults.source_slot_search_secs).unwrap_or(75),
        source_slot_browser_secs: i32::try_from(defaults.source_slot_browser_secs).unwrap_or(90),
        source_slot_api_fetch_secs: i32::try_from(defaults.source_slot_api_fetch_secs)
            .unwrap_or(60),
        preflight_model_timeout_secs: i32::try_from(defaults.preflight_model_timeout_secs)
            .unwrap_or(30),
        preflight_probe_timeout_secs: i32::try_from(defaults.preflight_probe_timeout_secs)
            .unwrap_or(10),
        preflight_overall_timeout_secs: i32::try_from(defaults.preflight_overall_timeout_secs)
            .unwrap_or(45),
        retry_step_budget_secs: i32::try_from(defaults.retry_step_budget_secs).unwrap_or(60),
        retry_total_budget_secs: i32::try_from(defaults.retry_total_budget_secs).unwrap_or(120),
        source: "table_default".to_string(),
    }
}

async fn upsert_pm_budget_profile(
    state: &AppState,
    tenant_id: &str,
    data: &PmBudgetProfileDto,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO pm_budget_profiles
            (tenant_id, profile_key, display_name, enabled, is_default, priority,
             pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls, max_calls_per_source,
             source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
             preflight_model_timeout_secs, preflight_probe_timeout_secs, preflight_overall_timeout_secs,
             retry_step_budget_secs, retry_total_budget_secs)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            enabled = excluded.enabled,
            is_default = excluded.is_default,
            priority = excluded.priority,
            pipeline_timeout_secs = excluded.pipeline_timeout_secs,
            max_attempts = excluded.max_attempts,
            retrieve_max_tool_calls = excluded.retrieve_max_tool_calls,
            max_calls_per_source = excluded.max_calls_per_source,
            source_slot_search_secs = excluded.source_slot_search_secs,
            source_slot_browser_secs = excluded.source_slot_browser_secs,
            source_slot_api_fetch_secs = excluded.source_slot_api_fetch_secs,
            preflight_model_timeout_secs = excluded.preflight_model_timeout_secs,
            preflight_probe_timeout_secs = excluded.preflight_probe_timeout_secs,
            preflight_overall_timeout_secs = excluded.preflight_overall_timeout_secs,
            retry_step_budget_secs = excluded.retry_step_budget_secs,
            retry_total_budget_secs = excluded.retry_total_budget_secs,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(&data.profile_key)
    .bind(&data.profile_key)
    .bind(data.enabled)
    .bind(data.is_default)
    .bind(data.priority)
    .bind(data.pipeline_timeout_secs)
    .bind(data.max_attempts)
    .bind(data.retrieve_max_tool_calls)
    .bind(data.max_calls_per_source)
    .bind(data.source_slot_search_secs)
    .bind(data.source_slot_browser_secs)
    .bind(data.source_slot_api_fetch_secs)
    .bind(data.preflight_model_timeout_secs)
    .bind(data.preflight_probe_timeout_secs)
    .bind(data.preflight_overall_timeout_secs)
    .bind(data.retry_step_budget_secs)
    .bind(data.retry_total_budget_secs)
    .execute(&state.db)
    .await?;

    if data.is_default {
        sqlx::query(
            "UPDATE pm_budget_profiles
             SET is_default = CASE WHEN profile_key = ? THEN 1 ELSE 0 END
             WHERE tenant_id = ?",
        )
        .bind(&data.profile_key)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
pub async fn overview(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<Json<ConfigOverview>> {
    let settings_path = state.data_dir.join(".claude/settings.json");
    let local_settings_path = state.data_dir.join(".claude/local/settings.json");

    let mut configs = Vec::new();

    for (path, source) in [
        (settings_path.clone(), "user"),
        (local_settings_path, "local"),
    ] {
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let parsed: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::Value::Null);
            configs.push(ConfigSnapshot {
                path: path.to_string_lossy().to_string(),
                source: source.to_string(),
                content: parsed,
            });
        }
    }

    let mut permission_mode = None;
    let mut current_model = None;
    let mut plugin_set = std::collections::BTreeSet::new();
    let mut mcp_set = std::collections::BTreeSet::new();
    for config in &configs {
        if let Some(obj) = config.content.as_object() {
            // Local config appears later in `configs` and should override user-level values.
            if let Some(v) = obj.get("permissionMode").and_then(|v| v.as_str()) {
                permission_mode = Some(v.to_string());
            }
            if let Some(v) = obj.get("model").and_then(|v| v.as_str()) {
                current_model = Some(v.to_string());
            }
            if let Some(plugins) = obj.get("plugins").and_then(|p| p.as_object()) {
                for (name, enabled) in plugins {
                    if enabled.as_bool().unwrap_or(false) {
                        plugin_set.insert(name.clone());
                    }
                }
            }
            if let Some(mcp) = obj
                .get("mcp")
                .and_then(|m| m.get("mcpServers"))
                .and_then(|ms| ms.as_object())
            {
                for name in mcp.keys() {
                    mcp_set.insert(name.clone());
                }
            }
        }
    }
    let active_plugins = plugin_set.into_iter().collect();
    let active_mcp_servers = mcp_set.into_iter().collect();

    Ok(Json(ConfigOverview {
        configs,
        permission_mode,
        current_model,
        active_plugins,
        active_mcp_servers,
    }))
}

async fn management_overview(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ConfigManagementOverview>> {
    ensure_admin(&claims)?;

    let operations_env = OPS_ENV_DEFS.iter().map(build_env_entry).collect::<Vec<_>>();
    let analytics_env = ANALYTICS_ENV_DEFS
        .iter()
        .map(build_env_entry)
        .collect::<Vec<_>>();
    let engineering_env = ENGINEERING_ENV_DEFS
        .iter()
        .map(build_env_entry)
        .collect::<Vec<_>>();

    let pm_budget_profile = load_pm_budget_profile(&state, &claims.tenant_id).await;

    Ok(Json(ConfigManagementOverview {
        operations: ConfigManagementTab {
            env: operations_env,
            pm_budget_profile: Some(pm_budget_profile),
        },
        analytics: ConfigManagementTab {
            env: analytics_env,
            pm_budget_profile: None,
        },
        engineering: ConfigManagementTab {
            env: engineering_env,
            pm_budget_profile: None,
        },
    }))
}

async fn update_env_config(
    Extension(claims): Extension<Claims>,
    Json(req): Json<UpdateEnvConfigRequest>,
) -> Result<Json<EnvConfigEntry>> {
    ensure_admin(&claims)?;

    let def = find_env_def(req.key.trim())
        .ok_or_else(|| AppError::ValidationError(format!("unknown config key: {}", req.key)))?;

    let should_clear = req.clear.unwrap_or(false) || req.value.is_none();

    if should_clear {
        std::env::remove_var(def.key);
    } else {
        let raw_value = req.value.unwrap_or_default();
        let normalized = match def.value_type {
            EnvValueType::Bool => normalize_bool_input(&raw_value)
                .ok_or_else(|| {
                    AppError::ValidationError(format!(
                        "invalid boolean value for {}: {}",
                        def.key, raw_value
                    ))
                })?
                .to_string(),
            EnvValueType::Integer => raw_value
                .trim()
                .parse::<i64>()
                .map_err(|_| {
                    AppError::ValidationError(format!(
                        "invalid integer value for {}: {}",
                        def.key, raw_value
                    ))
                })?
                .to_string(),
            EnvValueType::Float => raw_value
                .trim()
                .parse::<f64>()
                .map_err(|_| {
                    AppError::ValidationError(format!(
                        "invalid float value for {}: {}",
                        def.key, raw_value
                    ))
                })?
                .to_string(),
            EnvValueType::Secret => raw_value.trim().to_string(),
            EnvValueType::String => raw_value,
        };

        if matches!(def.value_type, EnvValueType::Secret)
            && (normalized.contains("****") || normalized == "********")
        {
            return Err(AppError::ValidationError(
                "masked secret values cannot be saved".to_string(),
            ));
        }
        std::env::set_var(def.key, normalized);
    }

    Ok(Json(build_env_entry(def)))
}

async fn update_pm_budget_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<UpdatePmBudgetProfileRequest>,
) -> Result<Json<PmBudgetProfileDto>> {
    ensure_admin(&claims)?;

    let mut current = load_pm_budget_profile(&state, &claims.tenant_id).await;

    if let Some(v) = req.profile_key {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            current.profile_key = trimmed.to_string();
        }
    }
    if let Some(v) = req.enabled {
        current.enabled = v;
    }
    if let Some(v) = req.is_default {
        current.is_default = v;
    }
    if let Some(v) = req.priority {
        current.priority = v;
    }

    macro_rules! apply_i32 {
        ($field:ident) => {
            if let Some(v) = req.$field {
                current.$field = v.max(1);
            }
        };
    }

    apply_i32!(pipeline_timeout_secs);
    apply_i32!(max_attempts);
    apply_i32!(retrieve_max_tool_calls);
    apply_i32!(max_calls_per_source);
    apply_i32!(source_slot_search_secs);
    apply_i32!(source_slot_browser_secs);
    apply_i32!(source_slot_api_fetch_secs);
    apply_i32!(preflight_model_timeout_secs);
    apply_i32!(preflight_probe_timeout_secs);
    apply_i32!(preflight_overall_timeout_secs);
    apply_i32!(retry_step_budget_secs);
    apply_i32!(retry_total_budget_secs);

    upsert_pm_budget_profile(&state, &claims.tenant_id, &current).await?;
    let latest = load_pm_budget_profile(&state, &claims.tenant_id).await;
    Ok(Json(latest))
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(overview))
        .route("/management", routing_get(management_overview))
        .route("/management/env", routing_patch(update_env_config))
        .route(
            "/management/pm-budget-profile",
            routing_patch(update_pm_budget_profile),
        )
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}
