import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Alert,
  Button,
  Card,
  Col,
  List,
  Row,
  Space,
  Statistic,
  Table,
  Tag,
  Tabs,
  Typography,
  Tooltip,
  message,
} from 'antd';
import type { ColumnsType } from 'antd/es/table';
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import {
  agentApi,
  pmApi,
  type PmBudgetProfileRow,
  type PmFailureTaxonomyRow,
  type PmKnowledgeCoverageWarningRow,
  type PmProviderHealthRow,
  type PmQualityGateWindowSummary,
  type PmRouteLearningFeatureRow,
  type PmRuntimeCostByModelRow,
  type PmFailureDrilldownRow,
  type PmRuntimeInsightsDailyRunRow,
  type PmRuntimeInsightsDailySourceQuotaRow,
  type PmRuntimeInsightsSummary,
  type PmSearchLayerAvailability,
  type PmSearchLayerStatus,
  type PmSearchProviderHealth,
  type PmSloWindowSummary,
} from '@/api';
import { usePermissions } from '@/store/permissions';
import { useNavigate } from '@/router';

const { Text } = Typography;

function pct(value?: number | null): string {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-';
  return `${(Math.max(0, Math.min(1, value)) * 100).toFixed(1)}%`;
}

function formatNumber(value?: number | null): string {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-';
  return value.toLocaleString();
}

function formatUsd(value?: number | null): string {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-';
  return `$${value.toFixed(value >= 10 ? 2 : 4)}`;
}

function formatDuration(seconds?: number | null): string {
  if (typeof seconds !== 'number' || Number.isNaN(seconds)) return '-';
  if (seconds <= 0) return '0s';
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

function healthColor(kind: 'ok' | 'warn' | 'bad'): string {
  if (kind === 'ok') return '#3f8600';
  if (kind === 'warn') return '#d48806';
  return '#cf1322';
}

function layerHealthKind(layer?: PmSearchLayerStatus | null): 'ok' | 'warn' | 'bad' {
  if (!layer) return 'warn';
  if (layer.available) return 'ok';
  const status = (layer.status || '').toLowerCase();
  if (status.includes('fail') || status.includes('error')) return 'bad';
  return 'warn';
}

function providerHealthKind(provider?: PmSearchProviderHealth | null): 'ok' | 'warn' | 'bad' {
  if (!provider?.enabled) return 'warn';
  const status = (provider.healthStatus || '').toLowerCase();
  if (status === 'healthy' || status === 'ok' || status === 'available') return 'ok';
  if (status.includes('fail') || status.includes('error') || status.includes('unhealthy')) return 'bad';
  return 'warn';
}

function healthTag(kind: 'ok' | 'warn' | 'bad', label: string) {
  const color = kind === 'ok' ? 'green' : kind === 'warn' ? 'gold' : 'red';
  return <Tag color={color}>{label}</Tag>;
}

function timestampText(value?: string | null): string {
  if (!value) return '-';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

export default function OperationsGovernance() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [activeTab, setActiveTab] = useState('overview');
  const [coveragePage, setCoveragePage] = useState(1);
  const [coveragePageSize, setCoveragePageSize] = useState(20);
  const canWriteGovernance = usePermissions((state) =>
    state.hasPermission('operations_governance:write'),
  );

  const budgetQ = useQuery({
    queryKey: ['pm', 'governance', 'budget-profiles'],
    queryFn: () => agentApi.listPmBudgetProfiles(),
  });

  const sloQ = useQuery({
    queryKey: ['pm', 'governance', 'slo'],
    queryFn: () => agentApi.listPmSloSummary(),
  });

  const failureQ = useQuery({
    queryKey: ['pm', 'governance', 'failure-taxonomy'],
    queryFn: () => agentApi.listPmFailureTaxonomy({ days: 30, limit: 20 }),
  });

  const providerQ = useQuery({
    queryKey: ['pm', 'governance', 'provider-health'],
    queryFn: () => agentApi.listPmProviderHealth(20),
  });

  const routeLearningQ = useInfiniteQuery({
    queryKey: ['pm', 'governance', 'route-learning'],
    initialPageParam: 1,
    queryFn: ({ pageParam }) =>
      agentApi.listPmRouteLearningFeatures({
        page: Number(pageParam) || 1,
        per_page: 20,
      }),
    getNextPageParam: (lastPage, pages) => {
      const loaded = pages.reduce((sum, page) => sum + (page.rows?.length ?? 0), 0);
      const total = Number(lastPage.total ?? loaded);
      return loaded < total ? pages.length + 1 : undefined;
    },
  });

  const qualityGateQ = useQuery({
    queryKey: ['pm', 'governance', 'quality-gate-summary'],
    queryFn: () => agentApi.listPmQualityGateSummary(),
  });

  const coverageWarnQ = useQuery({
    queryKey: ['pm', 'governance', 'knowledge-coverage-warnings', coveragePage, coveragePageSize],
    queryFn: () => agentApi.listPmKnowledgeCoverageWarnings({
      days: 30,
      page: coveragePage,
      per_page: coveragePageSize,
    }),
  });

  const runtimeInsightsQ = useQuery({
    queryKey: ['pm', 'governance', 'runtime-insights'],
    queryFn: () => agentApi.listPmRuntimeInsights({ days: 30 }),
  });

  const searchDoctorQ = useQuery({
    queryKey: ['pm', 'governance', 'search-doctor'],
    queryFn: () => pmApi.getSearchDoctor(),
    staleTime: 30_000,
  });

  const activateMut = useMutation({
    mutationFn: (profileKey: string) => agentApi.activatePmBudgetProfile(profileKey),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      qc.invalidateQueries({ queryKey: ['pm', 'governance', 'budget-profiles'] });
      qc.invalidateQueries({ queryKey: ['pm', 'governance', 'slo'] });
      qc.invalidateQueries({ queryKey: ['pm', 'governance', 'failure-taxonomy'] });
      qc.invalidateQueries({ queryKey: ['pm', 'governance', 'quality-gate-summary'] });
    },
    onError: (err: Error) => {
      message.error(err.message || t('common.operateFailed'));
    },
  });

  const budgetRows = budgetQ.data?.rows ?? [];
  const activeProfile = useMemo(
    () => budgetRows.find((row) => row.isDefault)?.profileKey ?? '',
    [budgetRows],
  );

  const budgetColumns: ColumnsType<PmBudgetProfileRow> = [
    {
      title: t('operations.govProfile', '策略档位'),
      dataIndex: 'displayName',
      render: (_v: string | undefined, row) => (
        <Space size={8} wrap>
          <Text strong>{row.displayName || row.profileKey}</Text>
          <Tag>{row.profileKey}</Tag>
          {row.isDefault ? <Tag color="green">{t('operations.govActive', '当前生效')}</Tag> : null}
        </Space>
      ),
    },
    {
      title: t('operations.govPipelineBudget', '总预算'),
      dataIndex: 'pipelineTimeoutSecs',
      width: 120,
      render: (v: number) => `${v}s`,
    },
    {
      title: t('operations.govSourceBudget', '来源预算'),
      width: 220,
      render: (_v, row) =>
        `search ${row.sourceSlotSearchSecs}s / browser ${row.sourceSlotBrowserSecs}s / fetch ${row.sourceSlotApiFetchSecs}s`,
    },
    {
      title: t('operations.govAttempts', '重试上限'),
      dataIndex: 'maxAttempts',
      width: 110,
    },
    {
      title: t('operations.govToolBudget', '工具上限'),
      dataIndex: 'retrieveMaxToolCalls',
      width: 110,
    },
    {
      title: t('common.actions'),
      width: 140,
      render: (_v, row) => (
        <Button
          size="small"
          type={row.profileKey === activeProfile ? 'default' : 'primary'}
          disabled={row.profileKey === activeProfile || !canWriteGovernance}
          loading={activateMut.isPending && activateMut.variables === row.profileKey}
          onClick={() => activateMut.mutate(row.profileKey)}
        >
          {row.profileKey === activeProfile
            ? t('operations.govActive', '当前生效')
            : t('operations.govActivate', '设为生效')}
        </Button>
      ),
    },
  ];

  const sloColumns: ColumnsType<PmSloWindowSummary> = [
    { title: t('operations.govWindow', '窗口'), dataIndex: 'windowDays', width: 100, render: (v: number) => `${v}d` },
    { title: t('operations.govTotalRuns', '总运行'), dataIndex: 'totalRuns', width: 110 },
    {
      title: t('operations.govTerminalRate', '终态率'),
      dataIndex: 'terminalRate',
      width: 120,
      render: (v?: number) => pct(v),
    },
    {
      title: t('operations.govAnswerDeliveryRate', '有效答案交付率'),
      dataIndex: 'answerDeliveryRate',
      width: 150,
      render: (v?: number) => pct(v),
    },
    { title: t('operations.govSuccessRate', '完成率'), dataIndex: 'successRate', width: 120, render: (v?: number) => pct(v) },
    {
      title: t('operations.govQualitySampleRuns', '质量样本'),
      dataIndex: 'qualitySampleRuns',
      width: 150,
      render: (v: number | undefined, row) => `${formatNumber(v)} (${pct(row.qualitySampleCoverage)})`,
    },
    {
      title: t('operations.govQualityPassRate', '质量达标率'),
      dataIndex: 'qualityPassRate',
      width: 140,
      render: (v?: number) => pct(v),
    },
    { title: 'P50(ms)', dataIndex: 'latencyP50Ms', width: 110, render: (v?: number) => v ?? '-' },
    { title: 'P95(ms)', dataIndex: 'latencyP95Ms', width: 110, render: (v?: number) => v ?? '-' },
    { title: 'P99(ms)', dataIndex: 'latencyP99Ms', width: 110, render: (v?: number) => v ?? '-' },
    { title: t('operations.govLatencySamples', '延迟样本数'), dataIndex: 'latencySampleCount', width: 120 },
  ];

  const failureColumns: ColumnsType<PmFailureTaxonomyRow> = [
    { title: t('operations.govErrorCode', '错误码'), dataIndex: 'errorCode', width: 220 },
    { title: t('operations.govFailureObjects', '失败对象'), dataIndex: 'objectCount', width: 100 },
    { title: t('operations.govRunFailures', '运行'), dataIndex: 'runFailureCount', width: 80 },
    { title: t('operations.govSubtaskFailures', '子任务'), dataIndex: 'subtaskFailureCount', width: 80 },
    { title: t('operations.govToolFailures', '工具调用'), dataIndex: 'toolFailureCount', width: 90 },
    {
      title: t('operations.govAvgElapsed', '平均耗时(ms)'),
      dataIndex: 'avgElapsedMs',
      width: 130,
      render: (v?: number) => v ?? '-',
    },
    { title: t('operations.govLastSeen', '最近出现'), dataIndex: 'lastSeenAt', render: (v?: string) => v || '-' },
  ];

  const qualityGateColumns: ColumnsType<PmQualityGateWindowSummary> = [
    { title: t('operations.govWindow', '窗口'), dataIndex: 'windowDays', width: 100, render: (v: number) => `${v}d` },
    { title: t('operations.govTotalRuns', '总运行'), dataIndex: 'totalRuns', width: 110 },
    { title: t('operations.govQualityPassRate', '质量达标率'), dataIndex: 'passRate', width: 130, render: (v?: number) => pct(v) },
    { title: t('operations.govAvgQualityScore', '平均质量分'), dataIndex: 'avgQualityScore', width: 120, render: (v?: number) => typeof v === 'number' ? v.toFixed(3) : '-' },
    { title: t('operations.govTriadCoverage', '核心结构覆盖率'), dataIndex: 'avgTriadCoverage', width: 130, render: (v?: number) => pct(v) },
    { title: t('operations.govClaimAlignRate', '结论对齐率'), dataIndex: 'claimAlignmentRate', width: 130, render: (v?: number) => pct(v) },
    { title: t('operations.govConflictAdjRate', '冲突裁决率'), dataIndex: 'conflictAdjudicatedRate', width: 130, render: (v?: number) => pct(v) },
    { title: t('operations.govAvgCitationCount', '平均引用数'), dataIndex: 'avgCitationCount', width: 120, render: (v?: number) => typeof v === 'number' ? v.toFixed(2) : '-' },
    { title: t('operations.govAvgDomainCount', '平均域名数'), dataIndex: 'avgDomainCount', width: 120, render: (v?: number) => typeof v === 'number' ? v.toFixed(2) : '-' },
  ];

  const coverageWarnColumns: ColumnsType<PmKnowledgeCoverageWarningRow> = [
    { title: t('operations.govRunId', 'Run ID'), dataIndex: 'runId', width: 200, render: (v?: string) => v || '-' },
    { title: t('operations.govCoverageRatio', '知识覆盖率'), dataIndex: 'coverageRatio', width: 120, render: (v?: number) => pct(v) },
    { title: t('operations.govPlannedSubtasks', '规划Subtask'), dataIndex: 'plannedSubtasks', width: 110 },
    { title: t('operations.govExecutedSubtasks', '执行Subtask'), dataIndex: 'executedSubtasks', width: 110 },
    { title: t('operations.govQueuedSubtasks', '排队Subtask'), dataIndex: 'queuedSubtasks', width: 110 },
    { title: t('operations.govSubtaskGapCount', 'Subtask缺口'), dataIndex: 'subtaskGapCount', width: 110 },
    { title: t('operations.govDimensionGapCount', '维度缺口'), dataIndex: 'dimensionGapCount', width: 110 },
    { title: t('common.createdAt'), dataIndex: 'createdAt', width: 180, render: (v?: string) => v || '-' },
    {
      title: t('common.actions'),
      width: 110,
      fixed: 'right',
      render: (_v, row) => (
        <Button
          type="link"
          size="small"
          onClick={() => row.taskId
            ? navigate(`/tasks?task=${encodeURIComponent(row.taskId)}`)
            : navigate('/tasks')}
        >
          {t('operations.govGoHandle', '去处理')}
        </Button>
      ),
    },
  ];

  const providerColumns: ColumnsType<PmProviderHealthRow> = [
    { title: t('operations.govProvider', 'Provider'), dataIndex: 'providerKey', width: 180 },
    { title: t('operations.govChannel', '通道'), dataIndex: 'channel', width: 100 },
    { title: t('operations.govRunCount', '触发次数'), dataIndex: 'runCount', width: 100 },
    {
      title: t('operations.govSuccessRate', '成功率'),
      width: 110,
      render: (_v, row) => row.runCount > 0 ? pct(row.successCount / row.runCount) : '-',
    },
    { title: t('operations.govFailedCount', '失败次数'), dataIndex: 'failureCount', width: 100 },
    { title: t('operations.govAvgElapsed', '平均耗时(ms)'), dataIndex: 'avgLatencyMs', width: 120, render: (v?: number) => v ?? '-' },
    { title: t('operations.govErrorCode', '错误码'), dataIndex: 'lastErrorCode', width: 180, render: (v?: string) => v || '-' },
    { title: t('common.status'), dataIndex: 'lastStatus', width: 100 },
    { title: t('operations.govLastSeen', '最近出现'), dataIndex: 'lastCheckedAt', width: 180, render: (v?: string) => v || '-' },
  ];

  const routeLearningColumns: ColumnsType<PmRouteLearningFeatureRow> = [
    { title: t('operations.govRouteKey', 'Route'), dataIndex: 'route', width: 220 },
    { title: t('operations.govChannel', '通道'), dataIndex: 'channel', width: 100, render: (v?: string) => v || '-' },
    { title: t('operations.govTotalRuns', '总运行'), dataIndex: 'totalRuns', width: 100 },
    { title: t('operations.govSuccessRate', '成功率'), dataIndex: 'emaSuccessRate', width: 110, render: (v?: number) => pct(v) },
    { title: t('operations.govAvgQualityScore', '平均质量分'), dataIndex: 'emaQuality', width: 120, render: (v?: number) => typeof v === 'number' ? v.toFixed(3) : '-' },
    { title: t('operations.govAvgElapsed', '平均耗时(ms)'), dataIndex: 'emaLatencyMs', width: 120, render: (v?: number) => typeof v === 'number' ? v.toFixed(0) : '-' },
    { title: t('operations.govAvgCostUsd', '平均成本($)'), dataIndex: 'emaCostUsd', width: 120, render: (v?: number) => typeof v === 'number' ? v.toFixed(4) : '-' },
    { title: t('operations.govLastSeen', '最近出现'), dataIndex: 'lastRunAt', width: 180, render: (v?: string) => v || '-' },
  ];

  const runtimeSummaryRow: PmRuntimeInsightsSummary = runtimeInsightsQ.data?.summary ?? {
    totalRuns: 0,
    queuedRuns: 0,
    runningRuns: 0,
    completedRuns: 0,
    failedRuns: 0,
    cancelledRuns: 0,
    terminalRuns: 0,
    terminalRate: undefined,
    answerDeliveryRate: undefined,
    retriedRuns: 0,
    recoveredRuns: 0,
    retryRecoveryRate: 0,
    firstPassSuccessRate: 0,
    failureRate: undefined,
    manualInterruptionRate: undefined,
    currentQueuedTasks: 0,
    currentRunningTasks: 0,
    currentQueuedSubtasks: 0,
    currentRunningSubtasks: 0,
    retryRepairAttempts: 0,
    sourceQuotaExhaustedAttempts: 0,
    sourceQuotaExhaustedRate: 0,
    degradedSynthesisRate: undefined,
  };

  const slo30d = useMemo(
    () => sloQ.data?.rows?.find((row) => row.windowDays === 30) ?? sloQ.data?.rows?.[0],
    [sloQ.data],
  );
  const sloWindowLabel = slo30d?.windowDays ? `${slo30d.windowDays}天` : '30天';
  const quality30d = useMemo(
    () => qualityGateQ.data?.rows?.find((row) => row.windowDays === 30) ?? qualityGateQ.data?.rows?.[0],
    [qualityGateQ.data],
  );
  const qualityWindowLabel = quality30d?.windowDays ? `${quality30d.windowDays}天` : '30天';
  const searchDoctor = searchDoctorQ.data;
  const searchLayers = searchDoctor?.orchestrator?.layers ?? [];
  const searchAvailableCount = [
    searchDoctor?.builtinWebSearch,
    searchDoctor?.nativeSearch,
    searchDoctor?.mcpSearch,
    searchDoctor?.ragLocal,
  ].filter((layer) => layer?.available).length + (searchDoctor?.configuredProviders ?? []).filter((provider) => provider.enabled && providerHealthKind(provider) === 'ok').length;
  const searchIssueCount = (searchDoctor?.degradedReason ? 1 : 0)
    + (searchDoctor?.configuredProviders ?? []).filter((provider) => provider.enabled && providerHealthKind(provider) !== 'ok').length;
  const searchHealthKind: 'ok' | 'warn' | 'bad' =
    searchIssueCount === 0 && searchAvailableCount > 0 ? 'ok' : searchAvailableCount > 0 ? 'warn' : 'bad';
  const deepResearch = runtimeInsightsQ.data?.deepResearch;
  const queueHealth = runtimeInsightsQ.data?.queueHealth;
  const costSummary = runtimeInsightsQ.data?.cost;
  const activeQueuedCount =
    (queueHealth?.queuedCount ?? runtimeSummaryRow.currentQueuedTasks ?? 0);
  const activeRunningCount =
    (queueHealth?.runningCount ?? runtimeSummaryRow.currentRunningTasks ?? 0);
  const activeTaskCount = activeQueuedCount + activeRunningCount;
  const oldestQueuedText =
    activeQueuedCount > 0
      ? formatDuration(queueHealth?.oldestQueuedObject?.ageSecs ?? queueHealth?.oldestQueuedTaskAgeSecs)
      : '-';
  const longestRunningText =
    activeRunningCount > 0 ? formatDuration(queueHealth?.longestRunningTaskAgeSecs) : '-';
  const longestHeartbeatText =
    activeRunningCount > 0 ? formatDuration(queueHealth?.longestRunningHeartbeatAgeSecs) : '-';
  const avgQueueWaitText =
    typeof queueHealth?.avgQueueWaitSecs === 'number'
      ? formatDuration(queueHealth.avgQueueWaitSecs)
      : '-';
  const totalRuns30d = slo30d?.totalRuns ?? runtimeSummaryRow.totalRuns ?? 0;
  const deliveredRuns30d = slo30d?.completedRuns ?? runtimeSummaryRow.completedRuns ?? 0;
  const failedRuns30d = slo30d?.failedRuns ?? runtimeSummaryRow.failedRuns ?? 0;
  const deepDecisionReadiness =
    deepResearch && (deepResearch.scoreSampleCount ?? 0) > 0
      ? deepResearch.avgDecisionReadiness
      : null;
  const deepActionability =
    deepResearch && (deepResearch.scoreSampleCount ?? 0) > 0 ? deepResearch.avgActionability : null;
  const degradedSynthesisRate =
    runtimeSummaryRow.degradedSynthesisRate ?? null;
  const avgCostPerRunUsd = costSummary?.avgCostPerRunUsd ?? null;
  const avgCostDisplay = (costSummary?.unpricedUsageRecordCount ?? 0) > 0
    ? t('operations.govPricingUnavailable', '价格未配置')
    : formatUsd(avgCostPerRunUsd);
  const totalCostDisplay = (costSummary?.unpricedUsageRecordCount ?? 0) > 0
    ? (costSummary?.pricedUsageRecordCount ?? 0) > 0
      ? `${t('operations.govPricedSubtotal', '已定价小计')} ${formatUsd(costSummary?.estimatedCostUsd)}`
      : t('operations.govPricingUnavailable', '价格未配置')
    : formatUsd(costSummary?.estimatedCostUsd);
  const unhealthyProviders = useMemo(
    () => (providerQ.data?.rows ?? []).filter((row) => {
      if ((row.runCount || 0) <= 0) return false;
      const status = (row.lastStatus || '').toLowerCase();
      const runCount = row.runCount || 0;
      const successRate = (row.successCount || 0) / runCount;
      return status === 'failed' || status === 'error' || successRate < 0.8;
    }).length,
    [providerQ.data],
  );
  const riskItems = useMemo(() => {
    const items: Array<{
      key: string;
      level: 'ok' | 'warn' | 'bad';
      text: string;
      destination?: { kind: 'path' | 'tab'; value: string };
    }> = [];
    if ((queueHealth?.staleRunningTasks ?? 0) > 0) {
      items.push({
        key: 'stale-running',
        level: 'bad',
        text: t('operations.govRiskStaleTasks', '疑似卡住任务 {{count}} 个，最长未心跳 {{age}}', {
          count: queueHealth?.staleRunningTasks ?? 0,
          age: longestHeartbeatText,
        }),
        destination: { kind: 'path', value: '/tasks' },
      });
    }
    if (activeQueuedCount > 0) {
      items.push({
        key: 'queued',
        level: 'warn',
        text: t('operations.govRiskQueuedTasks', '当前排队对象 {{count}} 个，最老等待 {{age}}', {
          count: activeQueuedCount,
          age: oldestQueuedText,
        }),
        destination: { kind: 'path', value: '/tasks' },
      });
    }
    if (searchHealthKind !== 'ok') {
      items.push({
        key: 'search',
        level: searchHealthKind,
        text: searchDoctor?.degradedReason || t('operations.govRiskSearch', '联网能力有 {{count}} 个不可用层，请检查联网 / Provider 配置。', { count: searchIssueCount }),
        destination: { kind: 'path', value: '/search-providers' },
      });
    }
    if ((coverageWarnQ.data?.summary?.warningCount ?? 0) > 0) {
      items.push({
        key: 'coverage',
        level: 'warn',
        text: t('operations.govRiskCoverage', '知识覆盖告警 {{count}} 条，最低覆盖率 {{ratio}}', {
          count: coverageWarnQ.data?.summary?.warningCount ?? 0,
          ratio: pct(coverageWarnQ.data?.summary?.minCoverageRatio),
        }),
        destination: { kind: 'tab', value: 'quality' },
      });
    }
    if ((degradedSynthesisRate ?? 0) > 0.15) {
      items.push({
        key: 'degraded',
        level: 'warn',
        text: t('operations.govRiskDegraded', '降级综合占比 {{ratio}}，需要关注检索或质量门稳定性。', { ratio: pct(degradedSynthesisRate) }),
        destination: { kind: 'tab', value: 'quality' },
      });
    }
    if ((runtimeSummaryRow.failureRate ?? 0) > 0.1) {
      items.push({
        key: 'failure',
        level: 'bad',
        text: t('operations.govRiskFailures', '失败率 {{ratio}}，建议查看运行和失败钻取。', { ratio: pct(runtimeSummaryRow.failureRate) }),
        destination: { kind: 'tab', value: 'runtime' },
      });
    }
    if (items.length === 0) {
      items.push({ key: 'ok', level: 'ok', text: t('operations.govRiskNone', '暂无需要立即处理的运行风险') });
    }
    return items;
  }, [
    coverageWarnQ.data,
    queueHealth,
    activeQueuedCount,
    longestHeartbeatText,
    oldestQueuedText,
    degradedSynthesisRate,
    runtimeSummaryRow.failureRate,
    searchDoctor?.degradedReason,
    searchHealthKind,
    searchIssueCount,
    t,
  ]);

  const runtimeSummaryColumns: ColumnsType<PmRuntimeInsightsSummary> = [
    { title: t('operations.govTotalRuns', '总运行'), dataIndex: 'totalRuns', width: 90 },
    { title: t('operations.govCurrentQueuedTasks', '当前排队任务'), dataIndex: 'currentQueuedTasks', width: 120 },
    { title: t('operations.govCurrentRunningTasks', '当前运行任务'), dataIndex: 'currentRunningTasks', width: 120 },
    {
      title: t('operations.govAnswerDeliveryRate', '有效答案交付率'),
      dataIndex: 'answerDeliveryRate',
      width: 140,
      render: (v?: number) => pct(v),
    },
    {
      title: t('operations.govFailureRate', '失败率'),
      dataIndex: 'failureRate',
      width: 100,
      render: (v?: number) => pct(v),
    },
    {
      title: t('operations.govDegradedSynthesisRate', '降级率'),
      dataIndex: 'degradedSynthesisRate',
      width: 100,
      render: (v?: number) => pct(v),
    },
    { title: t('operations.govCurrentQueuedSubtasks', '当前排队子任务'), dataIndex: 'currentQueuedSubtasks', width: 130 },
    { title: t('operations.govCurrentRunningSubtasks', '当前运行子任务'), dataIndex: 'currentRunningSubtasks', width: 130 },
    { title: t('operations.govRetriedRuns', '重试运行数'), dataIndex: 'retriedRuns', width: 110 },
    { title: t('operations.govRecoveredRuns', '重试恢复成功数'), dataIndex: 'recoveredRuns', width: 140 },
    {
      title: t('operations.govRetryRecoveryRate', '重试恢复率'),
      dataIndex: 'retryRecoveryRate',
      width: 110,
      render: (v: number) => pct(v),
    },
    {
      title: t('operations.govFirstPassSuccessRate', '首轮成功率'),
      dataIndex: 'firstPassSuccessRate',
      width: 110,
      render: (v: number) => pct(v),
    },
    { title: t('operations.govRetryRepairAttempts', '重试修复尝试数'), dataIndex: 'retryRepairAttempts', width: 130 },
    { title: t('operations.govSourceQuotaExhaustedAttempts', '配额耗尽次数'), dataIndex: 'sourceQuotaExhaustedAttempts', width: 120 },
    {
      title: t('operations.govSourceQuotaExhaustedRate', '配额耗尽占比'),
      dataIndex: 'sourceQuotaExhaustedRate',
      width: 120,
      render: (v: number) => pct(v),
    },
  ];

  const runtimeDailyRunColumns: ColumnsType<PmRuntimeInsightsDailyRunRow> = [
    { title: t('operations.govDate', '日期'), dataIndex: 'date', width: 120 },
    { title: t('operations.govTotalRuns', '总运行'), dataIndex: 'totalRuns', width: 90 },
    { title: t('operations.govCompletedCount', '成功次数'), dataIndex: 'completedRuns', width: 100 },
    { title: t('operations.govFailedCount', '失败次数'), dataIndex: 'failedRuns', width: 100 },
    { title: t('operations.govCancelledRuns', '取消次数'), dataIndex: 'cancelledRuns', width: 100 },
    { title: t('operations.govRetriedRuns', '重试运行数'), dataIndex: 'retriedRuns', width: 110 },
  ];

  const runtimeDailySourceQuotaColumns: ColumnsType<PmRuntimeInsightsDailySourceQuotaRow> = [
    { title: t('operations.govDate', '日期'), dataIndex: 'date', width: 120 },
    { title: t('operations.govRetryRepairAttempts', '重试修复尝试数'), dataIndex: 'retryRepairAttempts', width: 130 },
    { title: t('operations.govSourceQuotaExhaustedAttempts', '配额耗尽次数'), dataIndex: 'sourceQuotaExhaustedAttempts', width: 120 },
    {
      title: t('operations.govSourceQuotaExhaustedRate', '配额耗尽占比'),
      dataIndex: 'sourceQuotaExhaustedRate',
      width: 120,
      render: (v: number) => pct(v),
    },
  ];

  const costByModelColumns: ColumnsType<PmRuntimeCostByModelRow> = [
    { title: t('operations.govModel', '模型'), dataIndex: 'model', width: 180 },
    { title: t('operations.govProvider', 'Provider'), dataIndex: 'provider', width: 140 },
    { title: t('operations.govUsageRecords', '用量记录'), dataIndex: 'usageRecordCount', width: 100, render: (v: number | undefined, row) => formatNumber(v ?? row.requestCount) },
    { title: t('operations.govTotalTokens', 'Token'), dataIndex: 'totalTokens', width: 120, render: (v: number) => formatNumber(v) },
    { title: t('operations.govPricingSource', '价格来源'), dataIndex: 'pricingSource', width: 110, render: (v?: string) => v === 'custom' ? t('operations.govCustomPricing', '自定义') : v === 'built_in' ? t('operations.govBuiltInPricing', '内置') : t('operations.govPricingUnavailable', '未配置') },
    { title: t('operations.govEstimatedCost', '预估成本'), dataIndex: 'estimatedCostUsd', width: 120, render: (v: number, row) => row.pricingSource === 'unknown' ? '-' : formatUsd(v) },
  ];

  const failureDrilldownColumns: ColumnsType<PmFailureDrilldownRow> = [
    { title: t('operations.govFailureBucket', '失败类型'), dataIndex: 'bucket', width: 220 },
    { title: t('operations.govEventCount', '事件数'), dataIndex: 'count', width: 120, render: (v: number) => formatNumber(v) },
  ];

  const searchLayerColumns: ColumnsType<PmSearchLayerAvailability> = [
    {
      title: t('operations.govSearchLayer', '联网层'),
      dataIndex: 'label',
      width: 180,
      render: (v: string, row) => (
        <Space size={6} wrap>
          <Text strong>{v || row.layer}</Text>
          <Tag>{row.adapter}</Tag>
        </Space>
      ),
    },
    {
      title: t('common.status', '状态'),
      width: 120,
      render: (_v, row) => healthTag(row.available ? 'ok' : 'warn', row.status || (row.available ? 'available' : 'not configured')),
    },
    { title: t('operations.govSearchDetail', '说明'), dataIndex: 'detail', render: (v?: string) => v || '-' },
  ];

  const configuredSearchProviderColumns: ColumnsType<PmSearchProviderHealth> = [
    { title: t('operations.govProviderName', '名称'), dataIndex: 'name', width: 180 },
    { title: t('operations.govProviderType', '类型'), dataIndex: 'providerType', width: 120, render: (v: string) => <Tag>{v}</Tag> },
    { title: t('operations.govPriority', '优先级'), dataIndex: 'priority', width: 90 },
    {
      title: t('common.status', '状态'),
      width: 120,
      render: (_v, row) => healthTag(providerHealthKind(row), row.enabled ? row.healthStatus || 'unknown' : t('common.disabled', '已禁用')),
    },
    { title: t('operations.govHasSecret', '密钥'), dataIndex: 'hasSecret', width: 90, render: (v: boolean) => (v ? t('common.yes', '是') : t('common.no', '否')) },
    { title: t('operations.govLastError', '最近错误'), dataIndex: 'lastError', render: (v?: string | null) => v || '-' },
  ];

  const routeLearningRows = useMemo(
    () => routeLearningQ.data?.pages.flatMap((page) => page.rows ?? []) ?? [],
    [routeLearningQ.data],
  );
  const routeLearningTotal = routeLearningQ.data?.pages?.[0]?.total ?? routeLearningRows.length;
  const routeLearningHasMore = routeLearningRows.length < routeLearningTotal;

  const queryErrorMessages = [
    budgetQ.error,
    sloQ.error,
    failureQ.error,
    qualityGateQ.error,
    coverageWarnQ.error,
    runtimeInsightsQ.error,
    providerQ.error,
    routeLearningQ.error,
    searchDoctorQ.error,
  ]
    .filter(Boolean)
    .map((err) => (err instanceof Error ? err.message : String(err)));

  return (
    <div style={{ padding: '24px 24px 0' }}>
      <Row justify="space-between" align="middle" style={{ marginBottom: 16 }}>
        <Col>
          <Space direction="vertical" size={2}>
            <Text strong style={{ fontSize: 20 }}>
              {t('operations.governanceTitle', '治理中心')}
            </Text>
            <Text type="secondary">
              {t('operations.governanceDesc', '面向 PMO 的运行、质量、联网与成本健康看板。')}
            </Text>
          </Space>
        </Col>
        <Col>
          <Space size={8} wrap>
            <Tooltip title={t('operations.govGeneratedAtTip', '各卡片来自不同统计接口，时间用于判断数据新鲜度。')}>
              <Text type="secondary" style={{ fontSize: 12 }}>
                {t('operations.govGeneratedAt', '更新时间')}: {timestampText(runtimeInsightsQ.data?.generatedAt ?? sloQ.data?.generatedAt)}
              </Text>
            </Tooltip>
            <Button
              onClick={() => {
                qc.invalidateQueries({ queryKey: ['pm', 'governance'] });
              }}
            >
              {t('operations.ui.buttons.refresh', '刷新')}
            </Button>
          </Space>
        </Col>
      </Row>

      {queryErrorMessages.length > 0 ? (
        <Alert
          type="warning"
          showIcon
          style={{ marginBottom: 12 }}
          message={t('operations.govDataWarning', '部分治理数据加载失败，请检查接口或数据库。')}
          description={queryErrorMessages.map((msg, idx) => (
            <div key={`${idx}-${msg}`}>{msg}</div>
          ))}
        />
      ) : null}

      <Tabs
        activeKey={activeTab}
        onChange={setActiveTab}
        items={[
          {
            key: 'overview',
            label: t('operations.govTabOverview', '概览'),
            children: (
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Row gutter={[12, 12]}>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govTotalRunsWithWindow', '{{window}}总运行', { window: sloWindowLabel })}
                        value={formatNumber(totalRuns30d)}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{ fontSize: 22 }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govDeliveredAndFailed', '交付 {{delivered}} / 失败 {{failed}}', {
                          delivered: formatNumber(deliveredRuns30d),
                          failed: formatNumber(failedRuns30d),
                        })}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govAnswerDeliveryRateWithWindow', '{{window}}有效答案交付率', { window: sloWindowLabel })}
                        value={pct(slo30d?.answerDeliveryRate ?? runtimeSummaryRow.answerDeliveryRate)}
                        loading={sloQ.isLoading || runtimeInsightsQ.isLoading}
                        valueStyle={{ fontSize: 22, color: healthColor((slo30d?.answerDeliveryRate ?? 0) >= 0.8 ? 'ok' : 'warn') }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govTerminalRate', '终态率')}: {pct(slo30d?.terminalRate ?? runtimeSummaryRow.terminalRate)}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govQualityWithWindow', '{{window}}质量达标率', { window: qualityWindowLabel })}
                        value={pct(quality30d?.passRate)}
                        loading={qualityGateQ.isLoading}
                        valueStyle={{ fontSize: 22, color: healthColor((quality30d?.passRate ?? 0) >= 0.75 ? 'ok' : 'warn') }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govQualitySampleRuns', '质量样本')}: {formatNumber(quality30d?.totalRuns)}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govDeepDecisionReadiness', '决策准备度')}
                        value={deepDecisionReadiness == null ? '-' : pct(deepDecisionReadiness)}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{ fontSize: 22, color: healthColor((deepDecisionReadiness ?? 0) >= 0.75 ? 'ok' : 'warn') }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govActionability', '可执行度')}: {deepActionability == null ? '-' : pct(deepActionability)}
                      </Text>
                    </Card>
                  </Col>
                </Row>

                <Row gutter={[12, 12]}>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govHealthActiveTasks', '活跃任务')}
                        value={activeTaskCount}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{
                          fontSize: 22,
                          color: activeRunningCount > 0 ? '#1677ff' : undefined,
                        }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govQueuedAndRunning', '排队 {{queued}} / 运行 {{running}}', {
                          queued: activeQueuedCount,
                          running: activeRunningCount,
                        })}
                        {queueHealth ? ` · ${t('operations.govQueueObjectBreakdown', '任务/运行/子任务')} ${queueHealth.queuedTasks ?? 0}/${queueHealth.queuedRuns ?? 0}/${queueHealth.queuedSubtasks ?? 0}` : ''}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govOldestQueuedTask', '最老排队任务')}
                        value={oldestQueuedText}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{
                          fontSize: 22,
                          color: activeQueuedCount > 0 ? healthColor('warn') : undefined,
                        }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govLongestRunningTask', '最长运行任务')}: {longestRunningText}
                      </Text>
                      {queueHealth?.oldestQueuedObject && (
                        <Tooltip title={queueHealth.oldestQueuedObject.title}>
                          <Text type="secondary" ellipsis style={{ display: 'block', maxWidth: '100%', fontSize: 12 }}>
                            {queueHealth.oldestQueuedObject.objectType} #{queueHealth.oldestQueuedObject.objectId}
                          </Text>
                        </Tooltip>
                      )}
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govSearchHealth', '联网健康')}
                        value={searchHealthKind === 'ok' ? t('common.normal', '正常') : searchHealthKind === 'warn' ? t('common.warning', '注意') : t('common.abnormal', '异常')}
                        loading={searchDoctorQ.isLoading}
                        valueStyle={{ fontSize: 22, color: healthColor(searchHealthKind) }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govSearchAvailableLayers', '可用层')}: {formatNumber(searchAvailableCount)}
                        {searchIssueCount > 0 ? ` / ${t('common.warning', '注意')} ${formatNumber(searchIssueCount)}` : ''}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govAvgCostPerRun', '单次平均成本')}
                        value={avgCostDisplay}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{ fontSize: 22 }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govTotalTokens', 'Token')}: {formatNumber(costSummary?.totalTokens)}
                        {costSummary ? ` / ${t('operations.govCostRunCoverage', '运行样本覆盖')}: ${pct(costSummary.costRunCoverage)}` : ''}
                        {costSummary ? ` / ${t('operations.govPricingCoverage', '定价覆盖')}: ${pct(costSummary.pricingCoverage)}` : ''}
                        {degradedSynthesisRate != null ? ` / ${t('operations.govDegradedSynthesisRate', '降级率')}: ${pct(degradedSynthesisRate)}` : ''}
                      </Text>
                    </Card>
                  </Col>
                </Row>

                <Row gutter={[12, 12]}>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govFailureRate', '失败率')}
                        value={pct(runtimeSummaryRow.failureRate)}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{ fontSize: 22, color: healthColor((runtimeSummaryRow.failureRate ?? 0) <= 0.1 ? 'ok' : 'bad') }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govManualInterruptionRate', '人工中断率')}: {pct(runtimeSummaryRow.manualInterruptionRate)}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govDegradedSynthesisRate', '降级综合率')}
                        value={degradedSynthesisRate == null ? '-' : pct(degradedSynthesisRate)}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{ fontSize: 22, color: healthColor((degradedSynthesisRate ?? 0) <= 0.15 ? 'ok' : 'warn') }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govDegradedSynthesisCount', '降级综合')}: {formatNumber(deepResearch?.degradedSynthesisCount)}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govAvgQueueWait', '平均等待')}
                        value={avgQueueWaitText}
                        loading={runtimeInsightsQ.isLoading}
                        valueStyle={{ fontSize: 22 }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govOldestQueuedTask', '最老排队任务')}: {oldestQueuedText}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic
                        title={t('operations.govProviderHealthTitle', 'Provider 历史健康')}
                        value={unhealthyProviders > 0 ? t('common.warning', '注意') : t('common.normal', '正常')}
                        loading={providerQ.isLoading}
                        valueStyle={{ fontSize: 22, color: healthColor(unhealthyProviders > 0 ? 'warn' : 'ok') }}
                      />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govAbnormalProviderCount', '异常历史 Provider')}: {formatNumber(unhealthyProviders)}
                      </Text>
                    </Card>
                  </Col>
                </Row>

                <Card size="small" title={t('operations.govRiskQueue', '待处理风险')}>
                  <List
                    size="small"
                    dataSource={riskItems}
                    pagination={riskItems.length > 5 ? { pageSize: 5, size: 'small' } : false}
                    renderItem={(item) => (
                      <List.Item
                        actions={item.destination ? [
                          <Button
                            key="handle"
                            type="link"
                            size="small"
                            onClick={() => {
                              if (item.destination?.kind === 'path') navigate(item.destination.value);
                              if (item.destination?.kind === 'tab') setActiveTab(item.destination.value);
                            }}
                          >
                            {t('operations.govGoHandle', '去处理')}
                          </Button>,
                        ] : undefined}
                      >
                        <Space size={8} wrap>
                          {healthTag(item.level, item.level === 'ok' ? t('common.normal', '正常') : item.level === 'warn' ? t('common.warning', '注意') : t('common.abnormal', '异常'))}
                          <Text>{item.text}</Text>
                        </Space>
                      </List.Item>
                    )}
                  />
                </Card>
              </Space>
            ),
          },
          {
            key: 'runtime',
            label: t('operations.govTabRuntime', '运行'),
            children: (
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Row gutter={[12, 12]}>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small"><Statistic title={t('operations.govOldestQueuedTask', '最老排队任务')} value={oldestQueuedText} loading={runtimeInsightsQ.isLoading} /></Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small"><Statistic title={t('operations.govLongestRunningTask', '最长运行任务')} value={longestRunningText} loading={runtimeInsightsQ.isLoading} /></Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small">
                      <Statistic title={t('operations.govStaleRunningTasks', '疑似卡死任务')} value={queueHealth?.staleRunningTasks ?? 0} loading={runtimeInsightsQ.isLoading} valueStyle={{ color: (queueHealth?.staleRunningTasks ?? 0) > 0 ? '#cf1322' : '#3f8600' }} />
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('operations.govLongestHeartbeatAge', '最长未心跳')}: {longestHeartbeatText}
                      </Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small"><Statistic title={t('operations.govAvgQueueWait', '平均等待')} value={avgQueueWaitText} loading={runtimeInsightsQ.isLoading} /></Card>
                  </Col>
                </Row>

                <Card size="small" title={t('operations.govSloTitle', 'SLO 摘要')}>
                  <Table
                    rowKey={(row) => `${row.windowDays}`}
                    loading={sloQ.isLoading}
                    columns={sloColumns}
                    dataSource={sloQ.data?.rows ?? []}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                    scroll={{ x: 1150 }}
                  />
                </Card>

                <Card size="small" title={t('operations.govRuntimeInsightsTitle', '运行压力与重试收益（近 30 天）')}>
                  <Table
                    rowKey={() => 'runtime-summary'}
                    loading={runtimeInsightsQ.isLoading}
                    columns={runtimeSummaryColumns}
                    dataSource={[runtimeSummaryRow]}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                    scroll={{ x: 1500 }}
                  />
                </Card>

                <Row gutter={[12, 12]}>
                  <Col xs={24} lg={12}>
                    <Card size="small" title={t('operations.govRuntimeDailyTitle', '运行状态日趋势')}>
                      <Table
                        rowKey={(row) => row.date}
                        loading={runtimeInsightsQ.isLoading}
                        columns={runtimeDailyRunColumns}
                        dataSource={runtimeInsightsQ.data?.dailyRuns ?? []}
                        pagination={false}
                        locale={{ emptyText: t('common.noData') }}
                        scroll={{ x: 700 }}
                      />
                    </Card>
                  </Col>
                  <Col xs={24} lg={12}>
                    <Card size="small" title={t('operations.govSourceQuotaDailyTitle', '来源配额耗尽日趋势')}>
                      <Table
                        rowKey={(row) => row.date}
                        loading={runtimeInsightsQ.isLoading}
                        columns={runtimeDailySourceQuotaColumns}
                        dataSource={runtimeInsightsQ.data?.dailySourceQuota ?? []}
                        pagination={false}
                        locale={{ emptyText: t('common.noData') }}
                        scroll={{ x: 680 }}
                      />
                    </Card>
                  </Col>
                </Row>
              </Space>
            ),
          },
          {
            key: 'quality',
            label: t('operations.govTabQuality', '质量'),
            children: (
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Row gutter={[12, 12]}>
                  <Col xs={24} sm={12} lg={6}><Card size="small"><Statistic title={t('operations.govDeepDecisionReadiness', '决策准备度')} value={(deepResearch?.scoreSampleCount ?? 0) > 0 ? pct(deepResearch?.avgDecisionReadiness) : '-'} loading={runtimeInsightsQ.isLoading} /></Card></Col>
                  <Col xs={24} sm={12} lg={6}><Card size="small"><Statistic title={t('operations.govActionability', '可执行度')} value={(deepResearch?.scoreSampleCount ?? 0) > 0 ? pct(deepResearch?.avgActionability) : '-'} loading={runtimeInsightsQ.isLoading} /></Card></Col>
                  <Col xs={24} sm={12} lg={6}><Card size="small"><Statistic title={t('operations.govFirstPartyAlignment', '一手数据对齐')} value={(deepResearch?.scoreSampleCount ?? 0) > 0 ? pct(deepResearch?.avgFirstPartyAlignment) : '-'} loading={runtimeInsightsQ.isLoading} /></Card></Col>
                  <Col xs={24} sm={12} lg={6}><Card size="small"><Statistic title={t('operations.govEvidenceCoverage', '证据覆盖')} value={(deepResearch?.scoreSampleCount ?? 0) > 0 ? pct(deepResearch?.avgEvidenceCoverage) : '-'} loading={runtimeInsightsQ.isLoading} /></Card></Col>
                </Row>

                <Row gutter={[12, 12]}>
                  <Col xs={24} lg={12}>
                    <Card size="small" title={t('operations.govDeepResearchEvents', 'Deep Research 事件')}>
                      <Row gutter={[12, 12]}>
                        <Col span={12}><Statistic title={t('operations.govFinalizedCount', '完成')} value={deepResearch?.finalizedCount ?? 0} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={12}><Statistic title={t('operations.govDegradedSynthesisCount', '降级综合')} value={deepResearch?.degradedSynthesisCount ?? 0} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={12}><Statistic title={t('operations.govFollowupPlannedCount', '继续检索')} value={deepResearch?.followupPlannedCount ?? 0} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={12}><Statistic title={t('operations.govScoreSamples', '评分样本/事件')} value={`${deepResearch?.scoreSampleCount ?? 0}/${deepResearch?.eventCount ?? 0}`} loading={runtimeInsightsQ.isLoading} /></Col>
                      </Row>
                    </Card>
                  </Col>
                  <Col xs={24} lg={12}>
                    <Card size="small" title={t('operations.govRuntimeBehavior', '运行行为（系统派生）')}>
                      <Row gutter={[12, 12]}>
                        <Col span={12}><Statistic title={t('operations.govRetryRate', '重试率')} value={pct(runtimeInsightsQ.data?.userOutcome?.retryRate)} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={12}><Statistic title={t('operations.govManualInterruptionRate', '人工中断率')} value={pct(runtimeInsightsQ.data?.userOutcome?.manualInterruptionRate)} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={12}><Statistic title={t('operations.govFollowupPlannedCount', '继续检索计划')} value={runtimeInsightsQ.data?.userOutcome?.followupRepairCount ?? 0} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={12}><Statistic title={t('operations.govDerivedSignal', '口径')} value={t('operations.govDerived', '系统事件派生')} loading={runtimeInsightsQ.isLoading} /></Col>
                      </Row>
                    </Card>
                  </Col>
                </Row>

                <Card size="small" title={t('operations.govQualityGateTitle', '质量门汇总')}>
                  <Table
                    rowKey={(row) => `${row.windowDays}`}
                    loading={qualityGateQ.isLoading}
                    columns={qualityGateColumns}
                    dataSource={qualityGateQ.data?.rows ?? []}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                    scroll={{ x: 1150 }}
                  />
                </Card>

                <Card
                  size="small"
                  title={t('operations.govKnowledgeCoverageTitle', '知识覆盖告警（近 30 天）')}
                  extra={
                    <Space size={12} wrap>
                      <Text type="secondary">{t('operations.govCoverageAvg', '平均覆盖率')}: {pct(coverageWarnQ.data?.summary?.avgCoverageRatio)}</Text>
                      <Text type="secondary">{t('operations.govCoverageMin', '最低覆盖率')}: {pct(coverageWarnQ.data?.summary?.minCoverageRatio)}</Text>
                      <Text type="secondary">{t('operations.govCoverageWarnCount', '告警数')}: {coverageWarnQ.data?.summary?.warningCount ?? 0}</Text>
                    </Space>
                  }
                >
                  <Table
                    rowKey={(row) => `${row.runId ?? ''}-${row.createdAt ?? ''}`}
                    loading={coverageWarnQ.isLoading}
                    columns={coverageWarnColumns}
                    dataSource={coverageWarnQ.data?.rows ?? []}
                    pagination={{
                      current: coveragePage,
                      pageSize: coveragePageSize,
                      total: coverageWarnQ.data?.total ?? 0,
                      showSizeChanger: true,
                      onChange: (page, pageSize) => {
                        setCoveragePage(pageSize === coveragePageSize ? page : 1);
                        setCoveragePageSize(pageSize);
                      },
                    }}
                    locale={{ emptyText: t('common.noData') }}
                    scroll={{ x: 1000 }}
                  />
                </Card>

                <Card size="small" title={t('operations.govFailureDrilldownTitle', '失败钻取')}>
                  <Table
                    rowKey={(row) => row.bucket}
                    loading={runtimeInsightsQ.isLoading}
                    columns={failureDrilldownColumns}
                    dataSource={runtimeInsightsQ.data?.failureDrilldown ?? []}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                  />
                </Card>
              </Space>
            ),
          },
          {
            key: 'search',
            label: t('operations.govTabSearch', '联网 / Provider'),
            children: (
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Row gutter={[12, 12]}>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small" title={t('operations.govNativeSearch', '模型原生搜索')} extra={healthTag(layerHealthKind(searchDoctor?.nativeSearch), searchDoctor?.nativeSearch?.status ?? '-')}>
                      <Text type="secondary">{searchDoctor?.nativeSearch?.detail ?? '-'}</Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small" title="MCP Search" extra={healthTag(layerHealthKind(searchDoctor?.mcpSearch), searchDoctor?.mcpSearch?.status ?? '-')}>
                      <Text type="secondary">{searchDoctor?.mcpSearch?.detail ?? '-'}</Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small" title={t('operations.govConfiguredSearchProviders', 'Search 扩展')} extra={healthTag((searchDoctor?.configuredProviders ?? []).some((p) => p.enabled && providerHealthKind(p) === 'ok') ? 'ok' : 'warn', `${(searchDoctor?.configuredProviders ?? []).filter((p) => p.enabled && providerHealthKind(p) === 'ok').length}`)}>
                      <Text type="secondary">{t('operations.govConfiguredSearchProvidersDesc', '来自页面/表配置的联网搜索 Provider。')}</Text>
                    </Card>
                  </Col>
                  <Col xs={24} sm={12} lg={6}>
                    <Card size="small" title="RAG / Local" extra={healthTag(layerHealthKind(searchDoctor?.ragLocal), searchDoctor?.ragLocal?.status ?? '-')}>
                      <Text type="secondary">{searchDoctor?.ragLocal?.detail ?? '-'}</Text>
                    </Card>
                  </Col>
                </Row>

                {searchDoctor?.degradedReason ? (
                  <Alert
                    type="warning"
                    showIcon
                    message={t('operations.govSearchDegradedReason', '当前联网降级原因')}
                    description={searchDoctor.degradedReason}
                  />
                ) : null}

                <Card
                  size="small"
                  title={t('operations.govSearchOrchestratorHealth', '统一联网能力健康')}
                  extra={<Text type="secondary">{t('operations.govEffectiveOrder', '生效顺序')}: {(searchDoctor?.effectiveOrder ?? []).join(' -> ') || '-'}</Text>}
                >
                  <Table
                    rowKey={(row) => row.key}
                    loading={searchDoctorQ.isLoading}
                    columns={searchLayerColumns}
                    dataSource={searchLayers}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                  />
                </Card>

                <Card size="small" title={t('operations.govConfiguredSearchProviders', 'Search 扩展')}>
                  <Table
                    rowKey={(row) => row.id}
                    loading={searchDoctorQ.isLoading}
                    columns={configuredSearchProviderColumns}
                    dataSource={searchDoctor?.configuredProviders ?? []}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                  />
                </Card>

                <Card size="small" title={t('operations.govProviderHealthTitle', '运行时 Provider 历史健康（生命周期累计）')}>
                  <Table
                    rowKey={(row) => `${row.providerKey}-${row.channel}`}
                    loading={providerQ.isLoading}
                    columns={providerColumns}
                    dataSource={providerQ.data?.rows ?? []}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                    scroll={{ x: 1050 }}
                  />
                </Card>
              </Space>
            ),
          },
          {
            key: 'advanced',
            label: t('operations.govTabAdvanced', '高级诊断'),
            children: (
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Card
                  size="small"
                  title={t('operations.govBudgetTitle', '预算策略')}
                  extra={
                    !canWriteGovernance ? (
                      <Text type="secondary">{t('operations.govWritePermissionHint', '只读账号不能切换预算档位')}</Text>
                    ) : null
                  }
                >
                  <Table
                    rowKey={(row) => row.profileKey}
                    loading={budgetQ.isLoading}
                    columns={budgetColumns}
                    dataSource={budgetRows}
                    pagination={false}
                    locale={{ emptyText: t('common.noData') }}
                    scroll={{ x: 1000 }}
                  />
                </Card>

                <Row gutter={[12, 12]}>
                  <Col xs={24} lg={12}>
                    <Card size="small" title={t('operations.govCostTitle', '成本治理（近 30 天）')}>
                      <Row gutter={[12, 12]} style={{ marginBottom: 12 }}>
                        <Col span={8}><Statistic title={t('operations.govTotalTokens', 'Token')} value={formatNumber(costSummary?.totalTokens)} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={8}><Statistic title={t('operations.govUsageRecords', '用量记录')} value={formatNumber(costSummary?.usageRecordCount ?? costSummary?.requestCount)} loading={runtimeInsightsQ.isLoading} /></Col>
                        <Col span={8}><Statistic title={t('operations.govEstimatedCost', '预估成本')} value={totalCostDisplay} loading={runtimeInsightsQ.isLoading} /></Col>
                      </Row>
                      <Text type="secondary" style={{ display: 'block', marginBottom: 12, fontSize: 12 }}>
                        {t('operations.govCostSampleRuns', '可信成本样本')}: {formatNumber(costSummary?.costSampleRunCount)} / {formatNumber(runtimeSummaryRow.terminalRuns)}
                        {costSummary ? ` · ${t('operations.govCostRunCoverage', '运行样本覆盖')}: ${pct(costSummary.costRunCoverage)}` : ''}
                        {costSummary ? ` · ${t('operations.govPricingCoverage', '定价覆盖')}: ${pct(costSummary.pricingCoverage)}` : ''}
                      </Text>
                      <Table
                        rowKey={(row) => `${row.provider}-${row.model}-${row.pricingSource ?? 'unknown'}`}
                        loading={runtimeInsightsQ.isLoading}
                        columns={costByModelColumns}
                        dataSource={costSummary?.byModel ?? []}
                        pagination={false}
                        locale={{ emptyText: t('common.noData') }}
                      />
                    </Card>
                  </Col>
                  <Col xs={24} lg={12}>
                    <Card size="small" title={t('operations.govFailureTitle', '失败分类（近 30 天）')}>
                      <Table
                        rowKey={(row) => row.errorCode}
                        loading={failureQ.isLoading}
                        columns={failureColumns}
                        dataSource={failureQ.data?.rows ?? []}
                        pagination={false}
                        locale={{ emptyText: t('common.noData') }}
                        scroll={{ x: 850 }}
                      />
                    </Card>
                  </Col>
                </Row>

                <Card
                  size="small"
                  title={t('operations.govRouteLearningTitle', '路由学习诊断')}
                  extra={<Text type="secondary">{routeLearningRows.length}/{routeLearningTotal}</Text>}
                >
                  <div
                    style={{ maxHeight: 460, overflowY: 'auto' }}
                    onScroll={(event) => {
                      if (routeLearningQ.isFetchingNextPage || !routeLearningQ.hasNextPage) return;
                      const target = event.currentTarget;
                      const nearBottom = target.scrollTop + target.clientHeight >= target.scrollHeight - 24;
                      if (nearBottom) {
                        void routeLearningQ.fetchNextPage();
                      }
                    }}
                  >
                    <Table
                      rowKey={(row, index) => `${row.route}-${row.channel ?? ''}-${index ?? 0}`}
                      loading={routeLearningQ.isLoading}
                      columns={routeLearningColumns}
                      dataSource={routeLearningRows}
                      pagination={false}
                      locale={{ emptyText: t('common.noData') }}
                      scroll={{ x: 1100 }}
                    />
                  </div>
                  <div style={{ marginTop: 8 }}>
                    {routeLearningQ.isFetchingNextPage ? (
                      <Text type="secondary">{t('common.loading', '加载中...')}</Text>
                    ) : routeLearningHasMore ? (
                      <Text type="secondary">{t('operations.scrollToLoadMore', '下滑可加载更多')}</Text>
                    ) : (
                      <Text type="secondary">{t('operations.allLoaded', '已加载全部')}</Text>
                    )}
                  </div>
                </Card>
              </Space>
            ),
          },
        ]}
      />
    </div>
  );
}
