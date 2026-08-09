import dayjs from 'dayjs';
import type {
  AgentExecuteResponse,
  AppliedRuleHit,
  ConversationMessage,
  MatchedTable,
} from '@/types';
import type { TFunction } from 'i18next';
import type { AgentStepResult, NlTurn, QueryResult } from './types';

export const LEGACY_ROUTE_STAGE_ZH_MAP: Record<string, string> = {
  queued: '排队中',
  vector_matching: '向量相似度匹配中',
  search_candidates: '候选表检索中',
  rrfs_ranking: 'RRFS 融合排序中',
  domain_classifying: '业务域分类中',
  llm_routing: 'LLM 路由决策中',
  route_selected: '已匹配数据源',
  request_validation: '校验请求中',
  load_schema: '加载 Schema 中',
  load_context: '加载上下文中',
  query_understanding: '意图分析中',
  cache_lookup: '缓存检查中',
  clarification_gate: '澄清判断中',
  generate_sql: '生成 SQL 中',
  policy_enforcement: '策略校验中',
  persist_result: '结果持久化中',
  done: '已完成',
  failed: '失败',
  ai_confirming: 'AI 确认中',
};

export function getRouteStageText(t: TFunction, stage: string | null | undefined): string {
  if (!stage) return '';
  return t(`nl2sql.routeStages.${stage}` as any, { defaultValue: stage });
}

export function resolveRouteStageMessage(
  t: TFunction,
  stage: string | null | undefined,
  rawMessage: string | null | undefined,
): string {
  const label = getRouteStageText(t, stage);
  const message = (rawMessage ?? '').trim();
  if (!message) return label;
  if (stage && LEGACY_ROUTE_STAGE_ZH_MAP[stage] === message) return label;
  if (stage && (message === stage || message === label)) return label;
  return message;
}

export function appendOrUpdateStageTimeline(
  timeline: NlTurn['queryStageTimeline'],
  stage: string,
  messageText: string | null,
  elapsedMs: number,
  stageElapsedMs: number | null,
  opts?: { kind?: 'stage' | 'step'; label?: string },
) {
  const current = timeline ?? [];
  const idx = current.findIndex((item) => item.stage === stage);
  if (idx >= 0) {
    const cloned = [...current];
    cloned[idx] = {
      ...cloned[idx],
      message: messageText ?? cloned[idx].message ?? null,
      atElapsedMs: Math.max(cloned[idx].atElapsedMs, elapsedMs),
      stageElapsedMs: stageElapsedMs ?? cloned[idx].stageElapsedMs ?? null,
      kind: opts?.kind ?? cloned[idx].kind ?? 'stage',
      label: opts?.label ?? cloned[idx].label,
    };
    return cloned;
  }
  return [
    ...current,
    {
      stage,
      message: messageText ?? null,
      atElapsedMs: elapsedMs,
      stageElapsedMs: stageElapsedMs ?? null,
      kind: opts?.kind ?? 'stage',
      label: opts?.label,
    },
  ];
}

export function appendMultiSourceStepTimeline(
  timeline: NlTurn['queryStageTimeline'],
  steps: AgentStepResult[],
  elapsedMsAnchor: number,
  t: TFunction,
): NlTurn['queryStageTimeline'] {
  let elapsed = Math.max(elapsedMsAnchor, 0);
  let next = timeline ?? [];
  steps.forEach((step, idx) => {
    const stepElapsed = Math.max(step.execution_ms ?? 0, 0);
    elapsed += stepElapsed;
    const labelBase = step.step_type === 'merge'
      ? t('nl2sql.agent.mergeResult')
      : t('nl2sql.agent.step', { n: idx + 1 });
    const label = step.datasource_id
      ? `${labelBase} · ${step.datasource_id}`
      : labelBase;
    next = appendOrUpdateStageTimeline(
      next,
      `agent_step_${step.step_id}_${idx}`,
      step.error ?? step.description ?? null,
      elapsed,
      stepElapsed,
      { kind: 'step', label },
    );
  });
  return next;
}

export function resolveMultiSourceResult(
  response: AgentExecuteResponse,
  steps: AgentStepResult[],
): QueryResult {
  const rawFinal: {
    rows?: Record<string, unknown>[];
    columns?: string[];
    row_count?: number;
    rowCount?: number;
  } | null =
    (response as unknown as { final_result?: {
      rows?: Record<string, unknown>[];
      columns?: string[];
      row_count?: number;
      rowCount?: number;
    }; finalResult?: {
      rows?: Record<string, unknown>[];
      columns?: string[];
      row_count?: number;
      rowCount?: number;
    } }).final_result
    ?? (response as unknown as { final_result?: {
      rows?: Record<string, unknown>[];
      columns?: string[];
      row_count?: number;
      rowCount?: number;
    }; finalResult?: {
      rows?: Record<string, unknown>[];
      columns?: string[];
      row_count?: number;
      rowCount?: number;
    } }).finalResult
    ?? null;
  const finalRows = rawFinal?.rows ?? [];
  const finalColumns = rawFinal?.columns ?? [];
  const finalRowCount = rawFinal?.row_count ?? rawFinal?.rowCount ?? finalRows.length;
  const normalizedFinalColumns = finalColumns.length > 0
    ? finalColumns
    : (finalRows.length > 0 ? Object.keys(finalRows[0]) : []);
  const hasUsableFinal = normalizedFinalColumns.length > 0 && finalRows.length > 0;

  if (hasUsableFinal) {
    return {
      columns: normalizedFinalColumns,
      rows: finalRows,
      row_count: Math.max(finalRowCount, finalRows.length),
      total_rows: Math.max(finalRowCount, finalRows.length),
      has_more: false,
      limit: finalRows.length,
      offset: 0,
      execution_time_ms: response.total_execution_ms,
    };
  }

  const fallback = [...steps].reverse().find((step) => {
    const rows = Array.isArray(step.rows) ? step.rows : [];
    const columns = Array.isArray(step.columns) ? step.columns : [];
    return rows.length > 0 && columns.length > 0;
  });

  if (fallback) {
    return {
      columns: fallback.columns,
      rows: fallback.rows,
      row_count: Math.max(fallback.row_count ?? 0, fallback.rows.length),
      total_rows: Math.max(fallback.row_count ?? 0, fallback.rows.length),
      has_more: false,
      limit: fallback.rows.length,
      offset: 0,
      execution_time_ms: response.total_execution_ms,
    };
  }

  return {
    columns: normalizedFinalColumns,
    rows: finalRows,
    row_count: Math.max(finalRowCount, finalRows.length),
    total_rows: Math.max(finalRowCount, finalRows.length),
    has_more: false,
    limit: finalRows.length,
    offset: 0,
    execution_time_ms: response.total_execution_ms,
  };
}

export function buildSemanticContextFromMatchedTables(matchedTables: MatchedTable[] | undefined) {
  if (!matchedTables || matchedTables.length === 0) return undefined;
  return {
    matched_tables: matchedTables.map((mt) => ({
      data_source_id: mt.data_source_id,
      table_name: mt.table_name,
      column_name: mt.best_column,
      reason: mt.column_description,
      sim_score: mt.similarity_score,
    })),
  };
}

export type SqlErrorType = 'syntax' | 'table_not_found' | 'column_not_found' | 'permission' | 'execution' | 'unknown';

export function parseSqlError(raw: string): { type: SqlErrorType; message: string } {
  const lower = raw.toLowerCase();
  if (lower.startsWith('[syntax_error]')) {
    return { type: 'syntax', message: raw.replace(/^\[syntax_error\]\s*/i, '') };
  }
  if (lower.startsWith('[table_not_found]') || (lower.includes('table') && (lower.includes('doesn\'t exist') || lower.includes('not found') || lower.includes('does not exist')))) {
    return { type: 'table_not_found', message: raw.replace(/^\[\w+_error\]\s*/i, '') };
  }
  if (lower.startsWith('[column_not_found]') || (lower.includes('column') && (lower.includes('doesn\'t exist') || lower.includes('not found') || lower.includes('unknown column') || lower.includes('does not exist')))) {
    return { type: 'column_not_found', message: raw.replace(/^\[\w+_error\]\s*/i, '') };
  }
  if (lower.startsWith('[query_policy_denied]')) {
    return { type: 'permission', message: raw.replace(/^\[query_policy_denied\]\s*/i, '') };
  }
  if (lower.startsWith('[execution_error]') || lower.startsWith('[execution]')) {
    return { type: 'execution', message: raw.replace(/^\[\w+_error\]\s*/i, '') };
  }
  return { type: 'unknown', message: raw };
}

export function normalizeNl2sqlErrorMessage(raw: string, t?: TFunction): string {
  const text = String(raw ?? '').trim();
  if (!text) return text;
  const lower = text.toLowerCase();
  if (lower.includes('[no_datasource_configured]')) {
    return t
      ? t('nl2sql.noDatasourceConfigured')
      : '当前没有可查询的数据源。请先在「数据接入」中配置数据源。Skill 中的连接信息不会绕过 AOS 的数据权限与审计。';
  }
  if (lower.includes('[no_datasource_access]') || lower.includes('[no_queryable_datasource]')) {
    return t
      ? t('nl2sql.noDatasourceAccess')
      : '当前账号没有可查询的数据源。请联系管理员授予数据源访问权限。Skill 中的连接信息不会绕过 AOS 的数据权限与审计。';
  }
  if (
    lower.includes('out of quota')
    || lower.includes('billing balance')
    || lower.includes('insufficient_quota')
    || lower.includes('insufficient balance')
    || lower.includes('余额不足')
    || lower.includes('欠费')
  ) {
    return t
      ? t('nl2sql.apiKeyQuotaExceeded')
      : '当前用于 NL2SQL 的 API Key 余额不足或已欠费，请到「API Keys」页面充值或切换可用密钥后重试。';
  }
  return text;
}

export function mapConversationMessagesToTurns(messages: ConversationMessage[]): NlTurn[] {
  const orderedTurns: NlTurn[] = [];
  let lastAnsweredClarification: ConversationMessage | null = null;
  for (const msg of messages) {
    if (msg.message_type === 'clarification') {
      const clarifyBase = `conv-clarify-${msg.query_id}`;
      const previous = orderedTurns.at(-1);
      const duplicatesPreviousPrompt = previous?.role === 'assistant'
        && previous.question === msg.question
        && previous.clarificationQuestion === (msg.clarification_question ?? '');
      if (!duplicatesPreviousPrompt) {
        orderedTurns.push({
          id: `${clarifyBase}-assistant`,
          role: 'assistant',
          question: msg.question,
          clarificationQuestion: msg.clarification_question ?? '',
          clarificationTurn: msg.clarification_turn ?? null,
          clarificationConfirmedRequirements: msg.confirmed_requirements ?? null,
          clarificationMissingRequirements: msg.missing_requirements ?? null,
        });
      }
      if (msg.clarification_answer) {
        orderedTurns.push({
          id: `${clarifyBase}-user`,
          role: 'user',
          question: msg.clarification_answer,
        });
        lastAnsweredClarification = msg;
      }
      continue;
    }
    const baseId = `conv-${msg.query_id}`;
    const clarificationQuestion = lastAnsweredClarification?.question.trim();
    const clarificationAnswer = lastAnsweredClarification?.clarification_answer?.trim();
    const normalizedQuery = msg.question.trim();
    const queryReplaysClarification = !!clarificationQuestion
      && !!clarificationAnswer
      && normalizedQuery.includes(clarificationQuestion)
      && normalizedQuery.includes(clarificationAnswer);
    if (!queryReplaysClarification) {
      orderedTurns.push({
        id: baseId,
        role: 'user',
        question: msg.question,
        dataSourceId: msg.data_source_id ?? undefined,
      });
    }
    if (msg.generated_sql) {
      orderedTurns.push({
        id: `${baseId}-sql`,
        role: 'assistant',
        question: msg.question,
        sql: msg.generated_sql,
        queryId: msg.query_id,
        dataSourceId: msg.data_source_id ?? undefined,
        appliedRules: msg.applied_rules ?? undefined,
        usedReferences: msg.used_references ?? undefined,
        resultSource: msg.data_source_id ? 'query' : 'agent',
        result: msg.rows_returned != null ? {
          columns: [],
          rows: [],
          row_count: msg.rows_returned,
          execution_time_ms: msg.execution_ms ?? undefined,
        } : undefined,
      });
    }
    lastAnsweredClarification = null;
  }
  return orderedTurns;
}

export function fallbackGranularityLabel(mode?: string | null): string {
  switch (mode) {
    case 'weekly_granularity':
      return '按周（weekly）';
    case 'monthly_granularity':
      return '按月（monthly）';
    case 'quarterly_granularity':
      return '按季度（quarterly）';
    case 'yearly_granularity':
      return '按年（yearly）';
    default:
      return '按天（daily）';
  }
}

function normalizeRuleKey(value?: string | null): string {
  return String(value ?? '').trim().toLowerCase();
}

export function mergeAppliedRules(
  base?: AppliedRuleHit[] | null,
  next?: AppliedRuleHit[] | null,
): AppliedRuleHit[] {
  const merged: AppliedRuleHit[] = [];
  const seen = new Set<string>();
  for (const item of [...(base ?? []), ...(next ?? [])]) {
    if (!item) continue;
    const key = normalizeRuleKey(item.ruleKey) || normalizeRuleKey(item.ruleName);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    merged.push(item);
  }
  return merged;
}

export function resolveTurnQueryId(turn?: Partial<NlTurn> | null): string | null {
  if (!turn) return null;
  const direct = (turn.queryId ?? '').trim();
  if (direct) return direct;
  const id = String(turn.id ?? '');
  if (id.startsWith('conv-')) return id.slice('conv-'.length);
  if (id.startsWith('view-sql-')) return id.slice('view-sql-'.length);
  return null;
}

export function sanitizeHtml(html: string): string {
  return html
    .replace(/<script\b[^<]*(?:(?!<\/script>)<[^<]*)*<\/script>/gi, '')
    .replace(/<iframe\b[^<]*(?:(?!<\/iframe>)<[^<]*)*<\/iframe>/gi, '')
    .replace(/<object\b[^<]*(?:(?!<\/object>)<[^<]*)*<\/object>/gi, '')
    .replace(/<embed\b[^>]*>/gi, '')
    .replace(/<link\b[^>]*>/gi, '')
    .replace(/\s*on\w+\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]*)/gi, '')
    .replace(/javascript:/gi, '')
    .replace(/data:/gi, '');
}

export function formatTime(ts: string | number): string {
  return dayjs(ts).fromNow();
}

export function formatDuration(ms?: number): string {
  if (!ms) return '';
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}
