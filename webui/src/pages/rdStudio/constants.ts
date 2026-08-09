import type { RdRiskLevel } from './types';

export const RD_TASK_EVENT_PAGE_SIZE = 20;
export const RD_FILE_MENTION_LIMIT = 36;
export const DIFF_COLLAPSE_LINE_LIMIT = 520;
export const DIFF_PREVIEW_HEAD_LINES = 300;
export const DIFF_PREVIEW_TAIL_LINES = 120;

export const RD_SHARE_PREVIEW_MAX_CHARS = 18000;
export const RD_FOLLOW_UP_CONTEXT_MAX_CHARS = 14000;
export const RD_FOLLOW_UP_DIFF_MAX_CHARS = 7200;
export const RD_STALE_RUNTIME_TOOL_MS = 45 * 60 * 1000;

export const RD_MODIFY_INTENT_HINTS = [
  '修复',
  '新增',
  '实现',
  '修改',
  '改成',
  '删除',
  '重构',
  '接入',
  '支持',
  '优化',
  '补齐',
  '开发',
  '生成diff',
  '给出 diff',
  'fix',
  'implement',
  'add ',
  'change',
  'refactor',
  'remove',
  'update',
  'optimize',
  'patch',
];

export const RD_REVIEW_INTENT_HINTS = [
  '代码审查',
  'code review',
  'review',
  '审查',
  '找出问题',
  '找问题',
  '风险',
  '缺失测试',
  '安全审计',
  '全站巡检',
];

export const RD_EXPLAIN_INTENT_HINTS = [
  '解释报错',
  '报错',
  '错误',
  '异常',
  '堆栈',
  '日志',
  '失败',
  '为什么',
  'error',
  'exception',
  'stack trace',
  'traceback',
  'panic',
  'failed',
];

export const STATUS_COLORS: Record<string, string> = {
  queued: 'default',
  running: 'processing',
  waiting_approval: 'warning',
  completed: 'success',
  failed: 'error',
  cancelled: 'default',
  skipped: 'default',
  passed: 'success',
  timeout: 'error',
  stale: 'warning',
};

export const CONTEXT_PROFILE_FALLBACKS: Record<string, string> = {
  overview: '项目概览/架构问答',
  focused_ask: '定向代码库问答',
  explain: '报错解释',
  modify: '代码修改',
  review: '代码审查',
  deep_review: '深度审计',
};

export const CONTEXT_DEPTH_FALLBACKS: Record<string, string> = {
  shallow: '轻量',
  standard: '标准',
  deep: '深度',
};

export const RD_RETRIEVAL_SOURCE_FALLBACKS: Record<string, string> = {
  embedding_context: '仓库语义摘要',
  embedding_summary: '文件语义摘要',
  embedding_symbol: '符号语义召回',
  embedding_import: '依赖语义召回',
  embedding_task: '相似历史任务',
  explicit_file: '用户指定文件',
  file_summary: '文件摘要',
  symbol_index: '符号索引',
  import_index: '依赖索引',
  dependency_graph: '依赖图',
  retrieval_context: '索引召回',
};

export const RD_TOKEN_STAGE_FALLBACKS: Record<string, string> = {
  context_plan_llm: 'LLM 上下文规划',
  runtime_usage: 'Runtime 主循环',
  runtime: 'Runtime 主循环',
  summary: '主生成',
};

export const RD_RISK_LEVEL_COLORS: Record<RdRiskLevel, string> = {
  low: 'success',
  medium: 'warning',
  high: 'error',
  critical: 'magenta',
};

export const RD_RISK_LEVEL_FALLBACKS: Record<RdRiskLevel, string> = {
  low: '低风险',
  medium: '中风险',
  high: '高风险',
  critical: '严重风险',
};
