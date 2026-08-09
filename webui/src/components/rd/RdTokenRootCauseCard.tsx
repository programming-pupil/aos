import { Alert, Card, Empty, Space, Spin, Tag, Typography } from 'antd';
import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import type { RdTaskEvent } from '@/types';

const { Text } = Typography;

type RdTokenRootCauseCardProps = {
  events: RdTaskEvent[];
  loading?: boolean;
  embedded?: boolean;
};

type TokenUsageRow = {
  stage: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  totalTokens: number;
};

type RootCauseLevel = 'ok' | 'watch' | 'warning';

type RootCauseDiagnostic = {
  level: RootCauseLevel;
  title: string;
  description: string;
  recommendations: string[];
  cacheEvent?: RdTaskEvent;
  cacheHitCount: number;
  embeddingHits: number;
  summaryHits: number;
  symbolHits: number;
  importHits: number;
  dependencyGraphHits: number;
  taskMemoryHits: number;
  lexicalHits: number;
  selectedFiles: number;
  mergedCandidates: number;
  staleFiles: number;
  cacheReusedChunks: number;
  cacheRegeneratedChunks: number;
  estimatedTokensSaved: number;
  cacheMissReasons: string[];
  cacheSources: string[];
  readFileCount: number;
  searchCount: number;
  grepSearchCount: number;
  globSearchCount: number;
  repeatedInputsCount: number;
  repeatedTargetsCount: number;
  failedTargetsCount: number;
  emptyResultsCount: number;
  slowToolsCount: number;
  toolCallCount: number;
  suggestedReadFileCount: number;
  suggestedSearchCount: number;
  softReadThreshold: number;
  softSearchThreshold: number;
  overSuggestedRead: boolean;
  overSuggestedSearch: boolean;
  deepModeRecommended: boolean;
  totalTokens: number;
  localEstimatedSavingsRate: number;
  runtimeInputTokens: number;
  contextPlanInputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  providerCacheReadRate: number;
  providerCacheHit: boolean;
};

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function numberValue(record: Record<string, unknown> | null | undefined, keys: string[]): number {
  if (!record) return 0;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return 0;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
    : [];
}

function recordArray(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((item): item is Record<string, unknown> => !!item && typeof item === 'object' && !Array.isArray(item))
    : [];
}

function formatPercent(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0%';
  if (value >= 0.995) return '99%+';
  return `${Math.round(value * 100)}%`;
}

function latestEvent(events: RdTaskEvent[], stages: string[]): RdTaskEvent | undefined {
  const stageSet = new Set(stages);
  return events
    .filter((event) => stageSet.has(event.stage))
    .sort((a, b) => b.id - a.id)[0];
}

function tokenUsageRowFromEvent(event: RdTaskEvent): TokenUsageRow | null {
  const detail = asRecord(event.detailJson);
  if (!detail) return null;
  const usage = asRecord(detail.usage) ?? detail;
  const inputTokens = numberValue(usage, ['inputTokens', 'input_tokens']);
  const outputTokens = numberValue(usage, ['outputTokens', 'output_tokens']);
  const cacheCreationTokens = numberValue(usage, [
    'cacheCreationTokens',
    'cache_creation_tokens',
    'cacheCreationInputTokens',
    'cache_creation_input_tokens',
  ]);
  const cacheReadTokens = numberValue(usage, [
    'cacheReadTokens',
    'cache_read_tokens',
    'cacheReadInputTokens',
    'cache_read_input_tokens',
  ]);
  const totalTokens = numberValue(usage, ['totalTokens', 'total_tokens'])
    || inputTokens + outputTokens + cacheCreationTokens + cacheReadTokens;
  if (totalTokens <= 0) return null;
  return {
    stage: event.stage,
    inputTokens,
    outputTokens,
    cacheCreationTokens,
    cacheReadTokens,
    totalTokens,
  };
}

function buildUsageRows(events: RdTaskEvent[]): TokenUsageRow[] {
  const rows = events
    .map(tokenUsageRowFromEvent)
    .filter((row): row is TokenUsageRow => !!row);
  const hasRuntimeUsage = rows.some((row) => row.stage === 'runtime_usage');
  const hasRuntimeCompleted = rows.some((row) => row.stage === 'runtime');
  return rows.filter((row) => {
    if (row.stage === 'runtime' && hasRuntimeUsage) return false;
    if (row.stage === 'summary' && (hasRuntimeUsage || hasRuntimeCompleted)) return false;
    return ['context_plan_llm', 'runtime_usage', 'runtime', 'summary'].includes(row.stage);
  });
}

function latestRuntimeToolGovernance(events: RdTaskEvent[]): Record<string, unknown> | null {
  const direct = latestEvent(events, ['runtime_tool_governance']);
  const directDetail = asRecord(direct?.detailJson);
  if (directDetail) return directDetail;
  const runtime = latestEvent(events, ['runtime', 'candidate_worktree']);
  const runtimeDetail = asRecord(runtime?.detailJson);
  return asRecord(runtimeDetail?.toolGovernance);
}

function buildDiagnostic(events: RdTaskEvent[], t: ReturnType<typeof useTranslation>['t']): RootCauseDiagnostic {
  const cacheEvent = latestEvent(events, ['context_cache_usage']) ?? latestEvent(events, ['context_retrieval_evidence']);
  const cacheDetail = asRecord(cacheEvent?.detailJson);
  const governance = latestRuntimeToolGovernance(events);
  const governancePlan = asRecord(governance?.plan) ?? asRecord(governance?.toolGovernancePlan);
  const usageRows = buildUsageRows(events);
  const runtimeRows = usageRows.filter((row) => row.stage === 'runtime_usage' || row.stage === 'runtime' || row.stage === 'summary');
  const contextPlanRows = usageRows.filter((row) => row.stage === 'context_plan_llm');
  const totalTokens = usageRows.reduce((sum, row) => sum + row.totalTokens, 0);
  const runtimeInputTokens = runtimeRows.reduce((sum, row) => sum + row.inputTokens, 0);
  const contextPlanInputTokens = contextPlanRows.reduce((sum, row) => sum + row.inputTokens, 0);
  const outputTokens = usageRows.reduce((sum, row) => sum + row.outputTokens, 0);
  const cacheCreationTokens = usageRows.reduce((sum, row) => sum + row.cacheCreationTokens, 0);
  const cacheReadTokens = usageRows.reduce((sum, row) => sum + row.cacheReadTokens, 0);

  const embeddingHits = numberValue(cacheDetail, ['embeddingHits', 'embeddingHitCount']);
  const summaryHits = numberValue(cacheDetail, ['summaryHits', 'summaryHitCount']);
  const symbolHits = numberValue(cacheDetail, ['symbolHits', 'symbolHitCount']);
  const importHits = numberValue(cacheDetail, ['importHits', 'importHitCount']);
  const dependencyGraphHits = numberValue(cacheDetail, ['dependencyGraphHits', 'dependencyGraphHitCount']);
  const taskMemoryHits = numberValue(cacheDetail, ['taskMemoryHits', 'taskMemoryHitCount']);
  const lexicalHits = numberValue(cacheDetail, ['lexicalHits', 'lexicalHitCount']);
  const cacheReusedChunks = numberValue(cacheDetail, ['cacheReusedChunks']);
  const cacheRegeneratedChunks = numberValue(cacheDetail, ['cacheRegeneratedChunks']);
  const estimatedTokensSaved = numberValue(cacheDetail, ['estimatedTokensSaved']);
  const cacheHitCount = embeddingHits
    + summaryHits
    + symbolHits
    + importHits
    + dependencyGraphHits
    + taskMemoryHits
    + lexicalHits
    + cacheReusedChunks;

  const readFileCount = numberValue(governance, ['readFileCount']);
  const grepSearchCount = numberValue(governance, ['grepSearchCount']);
  const globSearchCount = numberValue(governance, ['globSearchCount']);
  const searchCount = numberValue(governance, ['searchCount']) || grepSearchCount + globSearchCount;
  const suggestedReadFileCount = numberValue(governancePlan, ['suggestedReadFileCount']) || numberValue(governance, ['suggestedReadFileCount']);
  const suggestedSearchCount = numberValue(governancePlan, ['suggestedSearchCount']) || numberValue(governance, ['suggestedSearchCount']);
  const softReadThreshold = numberValue(governancePlan, ['softReadThreshold']) || numberValue(governance, ['softReadThreshold']);
  const softSearchThreshold = numberValue(governancePlan, ['softSearchThreshold']) || numberValue(governance, ['softSearchThreshold']);
  const overSuggestedRead = Boolean(governance?.overSuggestedReadFile) || (softReadThreshold > 0 && readFileCount > softReadThreshold);
  const overSuggestedSearch = Boolean(governance?.overSuggestedSearch) || (softSearchThreshold > 0 && searchCount > softSearchThreshold);
  const deepModeRecommended = Boolean(governance?.deepModeRecommended);
  const repeatedInputsCount = recordArray(governance?.repeatedInputs).length;
  const repeatedTargetsCount = recordArray(governance?.repeatedTargets).length;
  const failedTargetsCount = recordArray(governance?.failedTargets).length;
  const emptyResultsCount = recordArray(governance?.emptyResults).length;
  const slowToolsCount = recordArray(governance?.slowTools).length;
  const toolCallCount = numberValue(governance, ['toolCallCount']);
  const selectedFiles = numberValue(cacheDetail, ['selectedFiles', 'fileCount']);
  const mergedCandidates = numberValue(cacheDetail, ['mergedCandidates']);
  const staleFiles = numberValue(cacheDetail, ['staleFiles']);
  const cacheMissReasons = stringArray(cacheDetail?.cacheMissReasons).slice(0, 5);
  const cacheSources = stringArray(cacheDetail?.cacheSources ?? cacheDetail?.sources).slice(0, 8);
  const providerCacheHit = cacheReadTokens > 0;
  const localEstimatedSavingsRate = estimatedTokensSaved > 0
    ? estimatedTokensSaved / Math.max(1, estimatedTokensSaved + totalTokens)
    : 0;
  const providerCacheReadRate = cacheReadTokens > 0
    ? cacheReadTokens / Math.max(1, runtimeInputTokens + contextPlanInputTokens + cacheCreationTokens + cacheReadTokens)
    : 0;

  let level: RootCauseLevel = 'ok';
  let title = t('rd.tokenRootCauseHealthy', '未见明显 Token 异常');
  let description = t('rd.tokenRootCauseHealthyDesc', '本轮已有可观测数据，未发现缓存完全未命中或 runtime 明显过度读取。');
  const recommendations: string[] = [];

  if (!cacheDetail) {
    level = 'watch';
    title = t('rd.tokenRootCauseNoCacheEvent', '缺少缓存诊断事件');
    description = t('rd.tokenRootCauseNoCacheEventDesc', '当前已加载事件里没有 context_cache_usage/context_retrieval_evidence，无法判断本地缓存是否参与。');
    recommendations.push(t('rd.tokenRootCauseNoCacheEventTip', '如果任务事件很多，等待诊断摘要接口返回；若仍为空，检查任务是否在上下文构建前失败。'));
  } else if (cacheHitCount <= 0) {
    level = 'warning';
    title = t('rd.tokenRootCauseLocalCacheMiss', '本地缓存/索引基本未命中');
    description = t('rd.tokenRootCauseLocalCacheMissDesc', 'embedding、摘要、symbol/import、依赖图和历史任务记忆都没有形成有效命中，runtime 更容易扩大真实文件读取。');
    recommendations.push(t('rd.tokenRootCauseLocalCacheMissTip', '检查仓库索引是否完成、rd 场景 embedding 模型是否配置、文件 hash/mtime 是否导致首次重建。'));
  } else if (runtimeInputTokens >= 300_000 && (overSuggestedRead || readFileCount >= Math.max(8, suggestedReadFileCount * 2))) {
    level = 'warning';
    title = t('rd.tokenRootCauseRuntimeReadHeavy', '主要瓶颈：Runtime 真实文件读取偏多');
    description = t('rd.tokenRootCauseRuntimeReadHeavyDesc', '本地缓存已经参与定位，但后续 read_file/search 仍把大量真实文件内容送入模型，所以总输入 token 仍然很高。');
    recommendations.push(t('rd.tokenRootCauseRuntimeReadHeavyTip', '优先检查 Runtime 页签里的读取目标；简单需求应先读入口、相关 Controller/路由、统一返回体，除非证据不足再扩大。'));
  } else if (totalTokens >= 300_000 && !providerCacheHit) {
    level = 'watch';
    title = t('rd.tokenRootCauseProviderCacheMiss', '模型侧 Prompt Cache 未体现');
    description = t('rd.tokenRootCauseProviderCacheMissDesc', 'AOS 本地缓存可能已命中，但 token_usage 里的 cache_read_tokens 为 0，说明供应商侧缓存未命中或兼容接口未回传缓存用量。');
    recommendations.push(t('rd.tokenRootCauseProviderCacheMissTip', '确认当前模型/兼容接口是否支持并回传 prompt cache usage；本地 embedding 命中不等于账单侧 cache_read_tokens。'));
  } else if (contextPlanInputTokens > runtimeInputTokens && contextPlanInputTokens >= 100_000) {
    level = 'watch';
    title = t('rd.tokenRootCausePlannerHeavy', '上下文规划阶段消耗偏高');
    description = t('rd.tokenRootCausePlannerHeavyDesc', '主要 token 可能花在 LLM Context Planner，而不是最终 runtime 编码循环。');
    recommendations.push(t('rd.tokenRootCausePlannerHeavyTip', '检查 context_plan_llm 输入是否包含过多候选摘要；保留效果优先，但需要让 Planner 只看高价值候选。'));
  } else if (cacheRegeneratedChunks > cacheReusedChunks && cacheRegeneratedChunks > 0) {
    level = 'watch';
    title = t('rd.tokenRootCauseIndexRebuild', '本轮索引重建占比偏高');
    description = t('rd.tokenRootCauseIndexRebuildDesc', '文件变化、切换 embedding 模型或首次构建会导致重新摘要/embedding，重复提问后应逐步转为复用。');
    recommendations.push(t('rd.tokenRootCauseIndexRebuildTip', '连续相同问题仍大量重建时，重点排查 content_hash、mtime、git_blob_sha 是否稳定。'));
  } else if (totalTokens >= 300_000) {
    level = 'watch';
    title = t('rd.tokenRootCauseLargeContext', '总 Token 偏高，需要继续看 Runtime 证据');
    description = t('rd.tokenRootCauseLargeContextDesc', '缓存有命中，但总消耗仍高；通常要结合 read_file 目标、工具输出和模型侧缓存读取一起判断。');
    recommendations.push(t('rd.tokenRootCauseLargeContextTip', '打开 Runtime 页签检查是否有重复读取、失败重试、空搜索和大输出工具结果。'));
  }

  if (taskMemoryHits === 0 && cacheDetail) {
    recommendations.push(t('rd.tokenRootCauseTaskMemoryMissTip', '历史任务记忆未命中：连续问相同需求时，应优先召回上次完成任务的 Diff/ touched files，再决定是否扩大搜索。'));
  }
  if (repeatedInputsCount > 0 || repeatedTargetsCount > 0) {
    recommendations.push(t('rd.tokenRootCauseRepeatedToolTip', '检测到重复工具输入/目标：优先复用已有证据或调整搜索词，避免原样重试。'));
  }
  if (failedTargetsCount > 0 || emptyResultsCount > 0) {
    recommendations.push(t('rd.tokenRootCauseFailedToolTip', '存在失败/空结果工具调用：下一轮应根据失败原因收敛路径，而不是继续盲扫。'));
  }

  return {
    level,
    title,
    description,
    recommendations: Array.from(new Set(recommendations)).slice(0, 5),
    cacheEvent,
    cacheHitCount,
    embeddingHits,
    summaryHits,
    symbolHits,
    importHits,
    dependencyGraphHits,
    taskMemoryHits,
    lexicalHits,
    selectedFiles,
    mergedCandidates,
    staleFiles,
    cacheReusedChunks,
    cacheRegeneratedChunks,
    estimatedTokensSaved,
    cacheMissReasons,
    cacheSources,
    readFileCount,
    searchCount,
    grepSearchCount,
    globSearchCount,
    repeatedInputsCount,
    repeatedTargetsCount,
    failedTargetsCount,
    emptyResultsCount,
    slowToolsCount,
    toolCallCount,
    suggestedReadFileCount,
    suggestedSearchCount,
    softReadThreshold,
    softSearchThreshold,
    overSuggestedRead,
    overSuggestedSearch,
    deepModeRecommended,
    totalTokens,
    localEstimatedSavingsRate,
    runtimeInputTokens,
    contextPlanInputTokens,
    outputTokens,
    cacheCreationTokens,
    cacheReadTokens,
    providerCacheReadRate,
    providerCacheHit,
  };
}

export function mergeRdTaskDiagnosticEvents(
  events: RdTaskEvent[],
  diagnosticEvents: RdTaskEvent[] = [],
): RdTaskEvent[] {
  const merged = new Map<number, RdTaskEvent>();
  for (const event of [...events, ...diagnosticEvents]) {
    merged.set(event.id, event);
  }
  return Array.from(merged.values()).sort((a, b) => b.id - a.id);
}

export function RdTokenRootCauseCard({ events, loading, embedded = false }: RdTokenRootCauseCardProps) {
  const { t } = useTranslation();
  const diagnostic = useMemo(() => buildDiagnostic(events, t), [events, t]);
  const alertType = diagnostic.level === 'warning' ? 'warning' : diagnostic.level === 'watch' ? 'info' : 'success';

  const content = (
    <Space direction="vertical" size={10} style={{ width: '100%' }}>
      <Alert
        type={alertType}
        showIcon
        message={diagnostic.title}
        description={diagnostic.description}
      />
      {loading ? (
        <Space>
          <Spin size="small" />
          <Text type="secondary">{t('rd.tokenRootCauseLoading', '正在加载诊断摘要...')}</Text>
        </Space>
      ) : null}
      {events.length === 0 && !loading ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={<span style={{ color: '#94a3b8' }}>{t('rd.tokenRootCauseNoEvents', '暂无任务事件，任务开始后会生成诊断。')}</span>}
        />
      ) : (
        <>
          <Space size={[6, 6]} wrap>
            <Tag color={diagnostic.cacheHitCount > 0 ? 'green' : 'default'}>
              {t('rd.localCacheHits', '本地缓存命中')}: {diagnostic.cacheHitCount.toLocaleString()}
            </Tag>
            <Tag color={diagnostic.providerCacheHit ? 'green' : 'default'}>
              {t('rd.providerCacheRead', '模型缓存读取')}: {diagnostic.cacheReadTokens.toLocaleString()}
            </Tag>
            <Tag color={diagnostic.estimatedTokensSaved > 0 ? 'lime' : 'default'}>
              {t('rd.estimatedTokensSaved', '估算节省 Token')}: {diagnostic.estimatedTokensSaved.toLocaleString()}
            </Tag>
            <Tag color={diagnostic.localEstimatedSavingsRate > 0 ? 'lime' : 'default'}>
              {t('rd.localEstimatedSavingsRate', '本地估算节省率')}: {formatPercent(diagnostic.localEstimatedSavingsRate)}
            </Tag>
            <Tag color={diagnostic.runtimeInputTokens >= 300_000 ? 'volcano' : 'blue'}>
              {t('rd.runtimeInputTokens', 'Runtime 输入')}: {diagnostic.runtimeInputTokens.toLocaleString()}
            </Tag>
            <Tag color={diagnostic.totalTokens >= 300_000 ? 'volcano' : 'geekblue'}>
              {t('rd.totalTokens', '总 Token')}: {diagnostic.totalTokens.toLocaleString()}
            </Tag>
            <Tag color={diagnostic.readFileCount > diagnostic.softReadThreshold && diagnostic.softReadThreshold > 0 ? 'orange' : 'blue'}>
              read_file: {diagnostic.readFileCount}
              {diagnostic.softReadThreshold > 0 ? ` / ${diagnostic.softReadThreshold}` : ''}
            </Tag>
            <Tag color={diagnostic.searchCount > diagnostic.softSearchThreshold && diagnostic.softSearchThreshold > 0 ? 'orange' : 'blue'}>
              search: {diagnostic.searchCount}
              {diagnostic.softSearchThreshold > 0 ? ` / ${diagnostic.softSearchThreshold}` : ''}
            </Tag>
          </Space>

          <Space size={[6, 6]} wrap>
            <Tag color={diagnostic.embeddingHits > 0 ? 'green' : 'default'}>Embedding: {diagnostic.embeddingHits}</Tag>
            <Tag color={diagnostic.summaryHits > 0 ? 'lime' : 'default'}>{t('rd.summaryHits', '摘要命中')}: {diagnostic.summaryHits}</Tag>
            <Tag color={diagnostic.symbolHits > 0 ? 'purple' : 'default'}>Symbol: {diagnostic.symbolHits}</Tag>
            <Tag color={diagnostic.importHits > 0 ? 'magenta' : 'default'}>Import: {diagnostic.importHits}</Tag>
            <Tag color={diagnostic.dependencyGraphHits > 0 ? 'orange' : 'default'}>{t('rd.dependencyGraphHits', '依赖图命中')}: {diagnostic.dependencyGraphHits}</Tag>
            <Tag color={diagnostic.taskMemoryHits > 0 ? 'gold' : 'default'}>{t('rd.taskMemoryHits', '历史任务命中')}: {diagnostic.taskMemoryHits}</Tag>
            <Tag>{t('rd.selectedCandidateFiles', '候选文件')}: {diagnostic.selectedFiles}</Tag>
            <Tag>{t('rd.mergedCandidates', '合并候选')}: {diagnostic.mergedCandidates}</Tag>
            <Tag color={diagnostic.cacheRegeneratedChunks > 0 ? 'volcano' : 'default'}>{t('rd.cacheRegeneratedChunks', '重建 Chunk')}: {diagnostic.cacheRegeneratedChunks}</Tag>
            <Tag color="green">{t('rd.cacheReusedChunks', '复用 Chunk')}: {diagnostic.cacheReusedChunks}</Tag>
          </Space>

          <Space size={[6, 6]} wrap>
            <Tag>{t('rd.contextPlanInputTokens', '规划输入')}: {diagnostic.contextPlanInputTokens.toLocaleString()}</Tag>
            <Tag>{t('rd.outputTokens', '输出')}: {diagnostic.outputTokens.toLocaleString()}</Tag>
            <Tag color={diagnostic.cacheCreationTokens > 0 ? 'gold' : 'default'}>{t('rd.cacheWriteTokens', '缓存写入')}: {diagnostic.cacheCreationTokens.toLocaleString()}</Tag>
            <Tag color={diagnostic.providerCacheReadRate > 0 ? 'green' : 'default'}>
              {t('rd.providerCacheReadRate', '模型缓存读取占比')}: {formatPercent(diagnostic.providerCacheReadRate)}
            </Tag>
            <Tag>{t('rd.toolCallCount', '工具调用')}: {diagnostic.toolCallCount}</Tag>
            <Tag color={diagnostic.repeatedInputsCount > 0 || diagnostic.repeatedTargetsCount > 0 ? 'orange' : 'default'}>
              {t('rd.repeatedToolSignals', '重复读取/输入')}: {diagnostic.repeatedTargetsCount + diagnostic.repeatedInputsCount}
            </Tag>
            <Tag color={diagnostic.failedTargetsCount > 0 || diagnostic.emptyResultsCount > 0 ? 'red' : 'default'}>
              {t('rd.failedOrEmptyToolSignals', '失败/空结果')}: {diagnostic.failedTargetsCount + diagnostic.emptyResultsCount}
            </Tag>
            {diagnostic.deepModeRecommended ? <Tag color="volcano">{t('rd.deepModeRecommended', '建议深度模式')}</Tag> : null}
          </Space>

          {diagnostic.cacheSources.length > 0 ? (
            <Text style={{ color: '#64748b', fontSize: 12 }}>
              {t('rd.cacheSources', '来源')}: {diagnostic.cacheSources.join(', ')}
            </Text>
          ) : null}

          {diagnostic.recommendations.length > 0 ? (
            <Space direction="vertical" size={2}>
              <Text style={{ color: '#94a3b8', fontSize: 12 }}>{t('rd.tokenRootCauseRecommendations', '建议排查')}</Text>
              {diagnostic.recommendations.map((item) => (
                <Text key={item} style={{ color: '#64748b', fontSize: 12 }}>- {item}</Text>
              ))}
            </Space>
          ) : null}

          {diagnostic.cacheMissReasons.length > 0 ? (
            <Space direction="vertical" size={2}>
              <Text style={{ color: '#94a3b8', fontSize: 12 }}>{t('rd.cacheMissReasons', '缓存未命中原因')}</Text>
              {diagnostic.cacheMissReasons.map((reason) => (
                <Text key={reason} style={{ color: '#64748b', fontSize: 12 }}>- {reason}</Text>
              ))}
            </Space>
          ) : null}

          <Text style={{ color: '#64748b', fontSize: 12 }}>
            {t('rd.tokenRootCauseEffectFirstNote', '说明：这张卡只做根因诊断，不会限制 Agent；写代码和 Review 仍会读取必要真实文件来保障效果。')}
          </Text>
        </>
      )}
    </Space>
  );

  if (embedded) return content;

  return (
    <Card
      size="small"
      style={{ background: '#07111f', borderColor: 'rgba(56, 189, 248, 0.28)' }}
      title={<span style={{ color: '#e2e8f0' }}>{t('rd.tokenRootCauseTitle', 'Token 根因诊断')}</span>}
    >
      {content}
    </Card>
  );
}
