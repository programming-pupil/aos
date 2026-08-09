import { useState, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from '@/router';
import {
  Row,
  Col,
  Card,
  Statistic,
  Table,
  DatePicker,
  Typography,
  Button,
  Space,
  Segmented,
  Alert,
  Tooltip,
  Tag,
} from 'antd';
import {
  ArrowUpOutlined,
  ThunderboltOutlined,
  CheckCircleOutlined,
  ReloadOutlined,
  InfoCircleOutlined,
  AlertOutlined,
  ClockCircleOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import dayjs from 'dayjs';
import { dashboardApi, agentOpsApi, type AgentOpsTask } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { PageSkeleton } from '@/components/Skeleton';
import type {
  DashboardConfigOverviewStats,
  ModelUsageStats,
  ModuleTokenUsageStats,
  DailyTokenStats,
} from '@/types';
import { usePermissions } from '@/store/permissions';

const { RangePicker } = DatePicker;
const { Title, Text, Paragraph } = Typography;

function statusColor(status?: string | null) {
  switch (status) {
    case 'completed':
      return 'green';
    case 'failed':
    case 'timed_out':
    case 'stale':
      return 'red';
    case 'cancelled':
      return 'default';
    case 'waiting_input':
    case 'waiting_approval':
    case 'blocked':
    case 'cancelling':
      return 'orange';
    case 'running':
    case 'queued':
    case 'claimed':
    case 'retrying':
      return 'blue';
    default:
      return 'default';
  }
}

function queueStatusColor(status?: string | null) {
  switch (status) {
    case 'queued':
      return 'cyan';
    case 'claimed':
    case 'running':
      return 'blue';
    case 'waiting_input':
    case 'cancelling':
      return 'orange';
    case 'succeeded':
      return 'green';
    case 'dead':
      return 'red';
    default:
      return 'default';
  }
}

function shortTime(value?: string | null) {
  if (!value) return '-';
  return dayjs(value).format('MM-DD HH:mm');
}

function capabilityName(key: string, t: ReturnType<typeof useTranslation>['t']) {
  const map: Record<string, string> = {
    ai_chat: t('watchdog.capabilities.aiChat'),
    super_adversarial: t('watchdog.capabilities.superAdversarial'),
    watchdog: t('watchdog.capabilities.watchdog'),
    pm_assistant: t('watchdog.capabilities.pmAssistant'),
    rd_agent: t('watchdog.capabilities.rdAgent'),
    nl2sql: t('watchdog.capabilities.nl2sql'),
    aos_router: t('watchdog.capabilities.aosRouter'),
    generic_ai: t('watchdog.capabilities.genericAi'),
  };
  return map[key] ?? key;
}

function formatNumber(n: number): string {
  if (!n && n !== 0) return '0';
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(2)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return n.toLocaleString();
}

function formatCost(cost: number): string {
  if (!cost && cost !== 0) return '$0.00';
  return `$${cost.toFixed(2)}`;
}

function formatPercent(pct: number): string {
  if (!pct && pct !== 0) return '0%';
  return `${pct.toFixed(1)}%`;
}

function SvgBarChart({ data }: { data: DailyTokenStats[] }) {
  const { t } = useTranslation();
  if (!data?.length) return null;

  const maxInput = Math.max(...data.map((d) => d.input_tokens || 0), 1);
  const maxOutput = Math.max(...data.map((d) => d.output_tokens || 0), 1);
  const maxVal = Math.max(maxInput, maxOutput);
  const W = 600, H = 220, PL = 50, PR = 16, PT = 16, PB = 36;
  const chartW = W - PL - PR;
  const chartH = H - PT - PB;
  const barW = (chartW / data.length) * 0.35;
  const gap = (chartW / data.length) * 0.05;

  return (
    <div style={{ overflowX: 'auto' }}>
      <svg viewBox={`0 0 ${W} ${H}`} style={{ width: '100%', minWidth: 400, height: 'auto', display: 'block' }}>
        {[0, 0.25, 0.5, 0.75, 1].map((pct, i) => {
          const y = PT + chartH * (1 - pct);
          return (
            <g key={i}>
              <line x1={PL} y1={y} x2={W - PR} y2={y} stroke="var(--border-default)" strokeWidth={1} strokeDasharray="4 4" />
              <text x={PL - 6} y={y + 4} textAnchor="end" fontSize={10} fill="var(--text-muted)">
                {formatNumber(Math.round(maxVal * pct))}
              </text>
            </g>
          );
        })}
        {data.map((d, i) => {
          const inputH = ((d.input_tokens || 0) / maxVal) * chartH;
          const outputH = ((d.output_tokens || 0) / maxVal) * chartH;
          const x = PL + (chartW / data.length) * i + gap;
          const baseY = PT + chartH;
          return (
            <g key={i}>
              <rect x={x} y={baseY - inputH} width={barW} height={inputH} fill="#58a6ff" rx={2} opacity={0.9} />
              <rect x={x + barW + 2} y={baseY - outputH} width={barW} height={outputH} fill="#3fb950" rx={2} opacity={0.9} />
              <text x={x + barW} y={H - 6} textAnchor="middle" fontSize={9} fill="var(--text-muted)">
                {d.date?.slice(5) ?? ''}
              </text>
            </g>
          );
        })}
        <g transform={`translate(${W - PR - 80}, ${PT})`}>
          <g>
            <rect x={0} y={0} width={10} height={10} rx={2} fill="#58a6ff" />
            <text x={14} y={9} fontSize={10} fill="var(--text-secondary)">{t('dashboard.inputToken')}</text>
          </g>
          <g transform="translate(0, 16)">
            <rect x={0} y={0} width={10} height={10} rx={2} fill="#3fb950" />
            <text x={14} y={9} fontSize={10} fill="var(--text-secondary)">{t('dashboard.outputToken')}</text>
          </g>
        </g>
      </svg>
    </div>
  );
}

function SvgPieChart({ data }: { data: ModelUsageStats[] }) {
  const { t } = useTranslation();
  if (!data?.length) return null;

  const top = data.slice(0, 5);
  const total = top.reduce((s, d) => s + (d.request_count || 0), 0);
  const COLORS = ['#58a6ff', '#3fb950', '#d29922', '#7c3aed', '#13c2c2', '#eb2f96'];
  const CX = 80, CY = 80, R = 65;
  const LEG_W = 120, LEG_H = 24;
  const W = CX * 2 + LEG_W + 10;
  const H = Math.max(CY * 2 + 10, top.length * LEG_H + 10);

  let angle = -Math.PI / 2;
  const slices = top.map((d, i) => {
    const cnt = d.request_count || 0;
    const pct = total > 0 ? cnt / total : 0;
    const startA = angle;
    angle += pct * Math.PI * 2;
    const endA = angle;
    const large = pct > 0.5 ? 1 : 0;
    const x1 = CX + R * Math.cos(startA);
    const y1 = CY + R * Math.sin(startA);
    const x2 = CX + R * Math.cos(endA);
    const y2 = CY + R * Math.sin(endA);
    const path = `M${CX},${CY} L${x1},${y1} A${R},${R} 0 ${large},1 ${x2},${y2} Z`;
    return { path, color: COLORS[i], model: d.model || '—', pct, count: cnt };
  });

  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 16, overflow: 'hidden' }}>
      <svg viewBox={`0 0 ${W} ${H}`} style={{ width: 180, height: 'auto', flexShrink: 0 }}>
        {slices.map((s, i) => (
          <path
            key={i}
            d={s.path}
            fill={s.color}
            opacity={0.85}
            stroke="var(--bg-surface)"
            strokeWidth={2}
          />
        ))}
        <text x={CX} y={CY - 4} textAnchor="middle" fontSize={20} fill="var(--text-primary)" fontWeight={700} fontFamily="var(--font-ui)">
          {formatNumber(total)}
        </text>
        <text x={CX} y={CY + 16} textAnchor="middle" fontSize={14} fill="var(--text-muted)" fontFamily="var(--font-ui)">
          {t('dashboard.totalRequests')}
        </text>
      </svg>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6, minWidth: 0, overflow: 'hidden', flex: 1 }}>
        {slices.map((s, i) => (
          <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0 }}>
            <div style={{ width: 12, height: 12, borderRadius: 3, background: s.color, flexShrink: 0 }} />
            <Text style={{ fontSize: 13, color: 'var(--text-primary)', flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontWeight: 500 }}>
              {s.model}
            </Text>
            <Text style={{ fontSize: 13, color: 'var(--text-secondary)', flexShrink: 0, fontVariantNumeric: 'tabular-nums', fontWeight: 500 }}>
              {(s.pct * 100).toFixed(0)}%
            </Text>
          </div>
        ))}
      </div>
    </div>
  );
}

export default function Dashboard() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { hasPermission } = usePermissions();
  const canReadWatchdog = hasPermission('tasks:read') || hasPermission('watchdog:read');

  const [quickRange, setQuickRange] = useState(7);
  const [dateRange, setDateRange] = useState<[dayjs.Dayjs | null, dayjs.Dayjs | null] | null>(null);

  const { data: configStats } = useQuery({
    queryKey: queryKeys.dashboard.configOverviewStats(),
    queryFn: () => dashboardApi.getConfigOverviewStats(),
    retry: false,
    throwOnError: false,
  });

  const agentOpsSummaryQuery = useQuery({
    queryKey: queryKeys.agentOps.summary(),
    queryFn: agentOpsApi.summary,
    retry: false,
    throwOnError: false,
    refetchInterval: 10_000,
    enabled: canReadWatchdog,
  });

  const agentHealthQuery = useQuery({
    queryKey: queryKeys.agentOps.agents(),
    queryFn: agentOpsApi.agents,
    retry: false,
    throwOnError: false,
    refetchInterval: 10_000,
    enabled: canReadWatchdog,
  });

  const attentionTasksQuery = useQuery({
    queryKey: queryKeys.agentOps.tasks({ attention_only: true, page: 1, per_page: 100 }),
    queryFn: () => agentOpsApi.tasks({ attention_only: true, page: 1, per_page: 100 }),
    retry: false,
    throwOnError: false,
    refetchInterval: 8_000,
    enabled: canReadWatchdog,
  });

  const staleQueueQuery = useQuery({
    queryKey: queryKeys.agentOps.queue({ staleOnly: true, leaseTimeoutSecs: 600, page: 1, per_page: 5 }),
    queryFn: () => agentOpsApi.queue({ staleOnly: true, leaseTimeoutSecs: 600, page: 1, per_page: 5 }),
    retry: false,
    throwOnError: false,
    refetchInterval: 10_000,
    enabled: canReadWatchdog,
  });

  const QUICK_RANGES = [
    { label: t('dashboard.last7days'), days: 7 },
    { label: t('dashboard.last30days'), days: 30 },
    { label: t('dashboard.last90days'), days: 90 },
  ];

  const queryParams = useMemo(() => {
    if (dateRange && dateRange[0] && dateRange[1]) {
      return {
        start_date: dateRange[0].format('YYYY-MM-DD'),
        end_date: dateRange[1].format('YYYY-MM-DD'),
      };
    }
    const end = dayjs();
    const start = end.subtract(Math.max(quickRange - 1, 0), 'day');
    return {
      start_date: start.format('YYYY-MM-DD'),
      end_date: end.format('YYYY-MM-DD'),
    };
  }, [dateRange, quickRange]);

  const {
    data: overview,
    isLoading,
    isError,
    isRefetching,
  } = useQuery({
    queryKey: queryKeys.dashboard.overview(queryParams),
    queryFn: () => dashboardApi.getOverview(queryParams),
    retry: false,
    throwOnError: false,
  });

  const { data: moduleUsage } = useQuery({
    queryKey: queryKeys.dashboard.moduleUsage(queryParams),
    queryFn: () => dashboardApi.getModuleUsage(queryParams),
    retry: false,
    throwOnError: false,
  });

  const effectiveOverview = overview ?? {
    token_stats: {
      total_input_tokens: 0,
      total_output_tokens: 0,
      total_cache_creation_tokens: 0,
      total_cache_read_tokens: 0,
      estimated_cost_usd: 0,
      session_count: 0,
      total_requests: 0,
      active_model_count: 0,
    },
    cache_stats: { total_cache_creation_tokens: 0, total_cache_read_tokens: 0, estimated_savings_usd: 0, cache_hit_rate: 0 },
    top_models: [],
    daily_trend: [],
  };

  const modelColumns: ColumnsType<ModelUsageStats> = [
    {
      title: t('dashboard.modelDistribution') || '模型',
      dataIndex: 'model',
      key: 'model',
      render: (v) => <Text code>{v || '—'}</Text>,
    },
    {
      title: t('dashboard.requestCount'),
      dataIndex: 'request_count',
      key: 'request_count',
      align: 'right',
      render: (v) => formatNumber(v || 0),
    },
    {
      title: t('dashboard.inputTokens'),
      dataIndex: 'input_tokens',
      key: 'input_tokens',
      align: 'right',
      render: (v) => formatNumber(v || 0),
    },
    {
      title: t('dashboard.outputTokens'),
      dataIndex: 'output_tokens',
      key: 'output_tokens',
      align: 'right',
      render: (v) => formatNumber(v || 0),
    },
    {
      title: t('dashboard.estimatedCost'),
      dataIndex: 'estimated_cost_usd',
      key: 'estimated_cost_usd',
      align: 'right',
      render: (v) => <Text type="warning">{formatCost(v || 0)}</Text>,
    },
  ];

  const moduleColumns: ColumnsType<ModuleTokenUsageStats> = [
    {
      title: t('dashboard.moduleUsage.columns.module'),
      dataIndex: 'module',
      key: 'module',
      render: (v: string) => {
        if (v === 'chat') return t('dashboard.module.chat');
        if (v === 'adversarial') return t('dashboard.module.adversarial');
        if (v === 'analytics') return t('dashboard.module.analytics');
        if (v === 'engineering') return t('dashboard.module.engineering');
        if (v === 'operations') return t('dashboard.module.operations');
        if (v === 'agent') return t('dashboard.module.agent');
        return v;
      },
    },
    {
      title: t('dashboard.requestCount'),
      dataIndex: 'request_count',
      key: 'request_count',
      align: 'right',
      render: (v) => formatNumber(v || 0),
    },
    {
      title: t('dashboard.inputTokens'),
      dataIndex: 'input_tokens',
      key: 'input_tokens',
      align: 'right',
      render: (v) => formatNumber(v || 0),
    },
    {
      title: t('dashboard.outputTokens'),
      dataIndex: 'output_tokens',
      key: 'output_tokens',
      align: 'right',
      render: (v) => formatNumber(v || 0),
    },
    {
      title: t('dashboard.totalTokens'),
      dataIndex: 'total_tokens',
      key: 'total_tokens',
      align: 'right',
      render: (v) => formatNumber(v || 0),
    },
    {
      title: t('dashboard.moduleUsage.columns.share'),
      dataIndex: 'token_share_pct',
      key: 'token_share_pct',
      align: 'right',
      render: (v) => formatPercent(v || 0),
    },
    {
      title: t('dashboard.estimatedCost'),
      dataIndex: 'estimated_cost_usd',
      key: 'estimated_cost_usd',
      align: 'right',
      render: (v) => <Text type="warning">{formatCost(v || 0)}</Text>,
    },
  ];

  if (isLoading) return <PageSkeleton rows={10} />;

  if (isError) {
    return (
      <div style={{ padding: 48, textAlign: 'center' }}>
        <Text type="danger" style={{ fontSize: 16 }}>{t('errors.serverError')}</Text>
        <br />
        <Button
          icon={<ReloadOutlined />}
          onClick={() => qc.refetchQueries({ queryKey: queryKeys.dashboard.all })}
          style={{ marginTop: 16 }}
        >
          {t('common.retry')}
        </Button>
      </div>
    );
  }

  const s = effectiveOverview.token_stats;
  const c = effectiveOverview.cache_stats;
  const totalTokens = (s?.total_input_tokens ?? 0) + (s?.total_output_tokens ?? 0);
  const totalRequests =
    (s?.total_requests ?? 0) > 0
      ? (s?.total_requests ?? 0)
      : (effectiveOverview.top_models ?? []).reduce(
          (sum, item) => sum + (item.request_count ?? 0),
          0,
        );
  const activeModelCount =
    (s?.active_model_count ?? 0) > 0
      ? (s?.active_model_count ?? 0)
      : (effectiveOverview.top_models ?? []).length;
  const avgTokensPerRequest =
    totalRequests > 0 ? totalTokens / totalRequests : 0;
  const avgCostPerRequest =
    totalRequests > 0 ? (s?.estimated_cost_usd ?? 0) / totalRequests : 0;
  const stats: DashboardConfigOverviewStats = configStats ?? {
    enabled_api_key_count: 0,
    enabled_hook_count: 0,
    enabled_mcp_server_count: 0,
    active_user_count: 0,
  };
  const configSummary = {
    hookCount: stats.enabled_hook_count,
    activeMcpServers: stats.enabled_mcp_server_count,
    tenantUsers: stats.active_user_count,
    apiKeyCount: stats.enabled_api_key_count,
  };
  const attentionTasks = attentionTasksQuery.data?.items ?? [];
  const agentHealth = agentHealthQuery.data?.items ?? [];
  const statusCounts = (agentOpsSummaryQuery.data?.byStatus ?? []).reduce<Record<string, number>>((acc, item) => {
    acc[item.status] = item.count;
    return acc;
  }, {});
  const waitingInputCount = (statusCounts.waiting_input ?? 0) + (statusCounts.waiting_approval ?? 0) + (statusCounts.blocked ?? 0);
  const staleQueueCount = staleQueueQuery.data?.total ?? 0;
  const staleAgentCount = agentOpsSummaryQuery.data?.stale ?? 0;
  const activeTotal = agentOpsSummaryQuery.data?.running ?? 0;
  const healthIssueCount = attentionTasksQuery.data?.total ?? 0;
  const systemHealth = !canReadWatchdog || agentOpsSummaryQuery.isError || attentionTasksQuery.isError
    ? 'unknown'
    : healthIssueCount > 0
      ? 'attention'
      : activeTotal > 0
        ? 'busy'
        : 'healthy';
  const healthColor = systemHealth === 'attention'
    ? '#cf222e'
    : systemHealth === 'busy'
      ? '#0969da'
      : systemHealth === 'unknown'
        ? '#6e7781'
        : '#1a7f37';
  const healthIcon = systemHealth === 'attention'
    ? <AlertOutlined />
    : systemHealth === 'busy'
      ? <ClockCircleOutlined />
      : <SafetyCertificateOutlined />;
  const healthTitle = t(`dashboard.agentOps.health.${systemHealth}.title`);
  const healthDescription = t(`dashboard.agentOps.health.${systemHealth}.description`);
  const openTask = (task: AgentOpsTask) => {
    navigate(`/tasks?task=${encodeURIComponent(task.id)}`);
  };
  const focusActionRequired = () => {
    document.getElementById('dashboard-action-required')?.scrollIntoView({ behavior: 'smooth', block: 'center' });
  };

  const attentionColumns: ColumnsType<AgentOpsTask> = [
    {
      title: t('dashboard.agentOps.columns.task'),
      dataIndex: 'title',
      key: 'title',
      width: 280,
      render: (value: string, row) => (
        <Space direction="vertical" size={1} style={{ width: '100%' }}>
          <Paragraph
            strong
            ellipsis={{ rows: 1, tooltip: value }}
            style={{ margin: 0, maxWidth: 260, overflowWrap: 'anywhere' }}
          >
            {value || row.id}
          </Paragraph>
          <Text type="secondary" style={{ fontSize: 12 }} ellipsis>
            {capabilityName(row.capabilityKey, t)} · {row.phase || '-'}
          </Text>
        </Space>
      ),
    },
    {
      title: t('common.status'),
      dataIndex: 'status',
      key: 'status',
      width: 120,
      render: (value: string) => <Tag color={statusColor(value)}>{t(`tasks.status.${value}`, value)}</Tag>,
    },
    {
      title: t('dashboard.agentOps.columns.reason'),
      key: 'reason',
      width: 320,
      render: (_: unknown, row) => {
        const reason = row.queue?.deadReason || row.queue?.lastError || row.errorMessage || row.lastEvent || row.runtimeSession?.currentCommand || '-';
        return (
          <Paragraph
            ellipsis={{ rows: 2, tooltip: reason }}
            style={{ margin: 0, maxWidth: 300, overflowWrap: 'anywhere' }}
          >
            {reason}
          </Paragraph>
        );
      },
    },
    {
      title: t('dashboard.agentOps.columns.queue'),
      key: 'queue',
      width: 120,
      render: (_: unknown, row) => <Tag color={queueStatusColor(row.queue?.status)}>{row.queue?.status || '-'}</Tag>,
    },
    {
      title: t('common.updatedAt'),
      dataIndex: 'updatedAt',
      key: 'updatedAt',
      width: 120,
      render: shortTime,
    },
  ];

  return (
    <div style={{ padding: '24px 24px 0', overflowY: 'auto', height: '100%', minWidth: 0 }}>
      <Space align="center" style={{ justifyContent: 'space-between', width: '100%', marginBottom: 16 }} wrap>
        <div>
          <Title level={3} style={{ margin: 0 }}>{t('dashboard.title')}</Title>
          <Text type="secondary" style={{ fontSize: 13 }}>{t('dashboard.subtitle')}</Text>
        </div>
        <Space wrap>
          <Button
            icon={<ReloadOutlined spin={attentionTasksQuery.isFetching || agentOpsSummaryQuery.isFetching || isRefetching} />}
            onClick={() => {
              qc.invalidateQueries({ queryKey: queryKeys.dashboard.all });
              qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
            }}
          >
            {t('common.refresh')}
          </Button>
        </Space>
      </Space>

      <Row gutter={[16, 16]} style={{ marginBottom: 16 }}>
        <Col xs={24} lg={8}>
          <Card size="small" style={{ height: '100%' }} styles={{ body: { minHeight: 168 } }}>
            <Space direction="vertical" size={12} style={{ width: '100%' }}>
              <Space align="start" style={{ justifyContent: 'space-between', width: '100%' }}>
                <Space align="start">
                  <div
                    style={{
                      width: 40,
                      height: 40,
                      borderRadius: 8,
                      display: 'flex',
                      alignItems: 'center',
                      justifyContent: 'center',
                      color: healthColor,
                      background: `${healthColor}14`,
                      fontSize: 20,
                    }}
                  >
                    {healthIcon}
                  </div>
                  <Space direction="vertical" size={1}>
                    <Text strong style={{ fontSize: 15 }}>{healthTitle}</Text>
                    <Text type="secondary" style={{ fontSize: 12 }}>{healthDescription}</Text>
                  </Space>
                </Space>
                <Tag color={systemHealth === 'attention' ? 'red' : systemHealth === 'busy' ? 'blue' : systemHealth === 'unknown' ? 'default' : 'green'}>
                  {t(`dashboard.agentOps.health.${systemHealth}.tag`)}
                </Tag>
              </Space>
              {healthIssueCount > 0 ? (
                <Alert
                  type="warning"
                  showIcon
                  message={t('dashboard.agentOps.needsAttentionHint', { count: healthIssueCount })}
                  action={canReadWatchdog ? <Button size="small" onClick={focusActionRequired}>{t('dashboard.agentOps.inspect')}</Button> : undefined}
                />
              ) : (
                <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.agentOps.noAttentionHint')}</Text>
              )}
            </Space>
          </Card>
        </Col>

        <Col xs={12} md={6} lg={4}>
          <Card size="small" style={{ height: '100%' }}>
            <Statistic
              title={t('dashboard.agentOps.activeTasks')}
              value={activeTotal}
              formatter={(v) => formatNumber(Number(v))}
              valueStyle={{ color: activeTotal > 0 ? '#0969da' : undefined, fontSize: 22 }}
            />
            <Text type="secondary" style={{ fontSize: 12 }}>
              {t('dashboard.agentOps.activeTasksHint', {
                queued: statusCounts.queued ?? 0,
                running: statusCounts.running ?? 0,
              })}
            </Text>
          </Card>
        </Col>
        <Col xs={12} md={6} lg={4}>
          <Card size="small" style={{ height: '100%' }}>
            <Statistic
              title={t('dashboard.agentOps.waitingInput')}
              value={waitingInputCount}
              formatter={(v) => formatNumber(Number(v))}
              valueStyle={{ color: waitingInputCount > 0 ? '#d29922' : undefined, fontSize: 22 }}
            />
            <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.agentOps.waitingInputHint')}</Text>
          </Card>
        </Col>
        <Col xs={12} md={6} lg={4}>
          <Card size="small" style={{ height: '100%' }}>
            <Statistic
              title={t('dashboard.agentOps.stale')}
              value={staleAgentCount}
              formatter={(v) => formatNumber(Number(v))}
              valueStyle={{ color: staleAgentCount > 0 ? '#d29922' : undefined, fontSize: 22 }}
            />
            <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.agentOps.staleHint')}</Text>
          </Card>
        </Col>
        <Col xs={12} md={6} lg={4}>
          <Card size="small" style={{ height: '100%' }}>
            <Statistic
              title={t('dashboard.agentOps.staleQueue')}
              value={staleQueueCount}
              formatter={(v) => formatNumber(Number(v))}
              valueStyle={{ color: staleQueueCount > 0 ? '#d29922' : undefined, fontSize: 22 }}
            />
            <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.agentOps.staleQueueHint')}</Text>
          </Card>
        </Col>
      </Row>

      <Row id="dashboard-action-required" gutter={[16, 16]} style={{ marginBottom: 16 }}>
        <Col xs={24} xl={15}>
          <Card
            size="small"
            title={t('dashboard.agentOps.actionRequired')}
            extra={<Tag>{t('common.total', { count: healthIssueCount })}</Tag>}
            styles={{ body: { padding: 0 } }}
          >
            <Table
              columns={attentionColumns}
              dataSource={attentionTasks}
              rowKey="id"
              loading={attentionTasksQuery.isLoading}
              pagination={attentionTasks.length > 6 ? { pageSize: 6, size: 'small' } : false}
              size="small"
              scroll={{ x: 'max-content' }}
              onRow={(record) => ({ onClick: () => openTask(record), style: { cursor: 'pointer' } })}
              locale={{ emptyText: t('dashboard.agentOps.noActionRequired') }}
            />
          </Card>
        </Col>
        <Col xs={24} xl={9}>
          <Card size="small" title={t('dashboard.agentOps.activeAgents')} style={{ height: '100%' }}>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
              {(agentHealth.length ? agentHealth : []).slice(0, 6).map((agent) => {
                return (
                  <div key={agent.capabilityKey} style={{ minWidth: 0 }}>
                    <Space style={{ justifyContent: 'space-between', width: '100%' }}>
                      <Text strong ellipsis style={{ maxWidth: 180 }}>{capabilityName(agent.capabilityKey, t)}</Text>
                      <Space size={4}>
                        <Tag color={agent.active ? 'blue' : 'default'}>{t('dashboard.agentOps.activeShort', { count: agent.active })}</Tag>
                        {agent.failed24h > 0 && <Tag color="red">{t('dashboard.agentOps.failed24hShort', { count: agent.failed24h })}</Tag>}
                      </Space>
                    </Space>
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      {t('dashboard.agentOps.agentLine', { total: agent.total, updated: shortTime(agent.lastUpdatedAt) })}
                    </Text>
                  </div>
                );
              })}
              {!agentHealth.length && (
                <Text type="secondary">{t('dashboard.agentOps.noAgents')}</Text>
              )}
            </div>
          </Card>
        </Col>
      </Row>

      {/* Config overview card */}
      <Row gutter={[16, 16]} style={{ marginBottom: 16 }}>
        <Col xs={24}>
          <Card
            size="small"
            title={t('dashboard.configOverview') || 'Config Overview'}
            style={{ marginBottom: 0 }}
          >
            <div style={{ display: 'flex', gap: 28, overflowX: 'auto', paddingBottom: 2 }}>
              <div style={{ minWidth: 96 }}>
                <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.enabledHookCount')}</Text>
                <div style={{ fontSize: 13, fontWeight: 600 }}>{configSummary.hookCount}</div>
              </div>
              <div style={{ minWidth: 112 }}>
                <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.enabledMcpServers')}</Text>
                <div style={{ fontSize: 13, fontWeight: 600 }}>{configSummary.activeMcpServers}</div>
              </div>
              <div style={{ minWidth: 112 }}>
                <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.activeTenantUsers')}</Text>
                <div style={{ fontSize: 13, fontWeight: 600 }}>{configSummary.tenantUsers}</div>
              </div>
              <div style={{ minWidth: 104 }}>
                <Text type="secondary" style={{ fontSize: 12 }}>{t('dashboard.enabledApiKeyCount')}</Text>
                <div style={{ fontSize: 13, fontWeight: 600 }}>{configSummary.apiKeyCount}</div>
              </div>
            </div>
          </Card>
        </Col>
      </Row>

      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 24, flexWrap: 'wrap', gap: 12 }}>
        <div>
          <Title level={4} style={{ margin: 0 }}>{t('dashboard.usageTitle')}</Title>
          <Text type="secondary" style={{ fontSize: 13 }}>{t('dashboard.usageSubtitle')}</Text>
        </div>
        <Space size={12} wrap>
          <Segmented
            options={QUICK_RANGES.map((r) => ({ label: r.label, value: r.days }))}
            value={quickRange}
            onChange={(v) => {
              setQuickRange(v as number);
              setDateRange(null);
            }}
          />
          <RangePicker
            value={dateRange}
            onChange={(dates) => {
              setDateRange(dates as [dayjs.Dayjs | null, dayjs.Dayjs | null] | null);
              if (dates) setQuickRange(0);
            }}
            allowClear
          />
          <Button
            icon={<ReloadOutlined spin={isRefetching} />}
            onClick={() => qc.refetchQueries({ queryKey: queryKeys.dashboard.all })}
          >
            {t('common.refresh')}
          </Button>
        </Space>
      </div>

      {/* Stat cards */}
      <Row gutter={[16, 16]} style={{ marginBottom: 16 }}>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={t('dashboard.totalRequests')}
              value={totalRequests}
              formatter={(v) => formatNumber(Number(v))}
              prefix={<ThunderboltOutlined style={{ color: '#0969da' }} />}
              valueStyle={{ fontSize: 20 }}
            />
          </Card>
        </Col>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={t('dashboard.inputTokens')}
              value={s?.total_input_tokens ?? 0}
              formatter={(v) => formatNumber(Number(v))}
              prefix={<ArrowUpOutlined style={{ color: '#58a6ff' }} />}
              valueStyle={{ color: '#58a6ff', fontSize: 20 }}
            />
          </Card>
        </Col>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={t('dashboard.outputTokens')}
              value={s?.total_output_tokens ?? 0}
              formatter={(v) => formatNumber(Number(v))}
              prefix={<ArrowUpOutlined style={{ color: '#3fb950' }} />}
              valueStyle={{ color: '#3fb950', fontSize: 20 }}
            />
          </Card>
        </Col>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={(
                <Space size={4}>
                  {t('dashboard.estimatedTotalCost')}
                  <Tooltip title={t('dashboard.estimatedCostTip')}>
                    <InfoCircleOutlined style={{ color: 'var(--text-muted)' }} />
                  </Tooltip>
                </Space>
              )}
              value={s?.estimated_cost_usd ?? 0}
              formatter={(v) => formatCost(Number(v))}
              valueStyle={{ fontSize: 20, color: '#d29922' }}
            />
          </Card>
        </Col>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={
                <Space size={4}>
                  {t('dashboard.cacheHitRate')}
                  <Tooltip title={t('dashboard.cacheHitRateTip')}>
                    <InfoCircleOutlined style={{ color: 'var(--text-muted)' }} />
                  </Tooltip>
                </Space>
              }
              value={c?.cache_hit_rate ?? 0}
              formatter={(v) => formatPercent(Number(v))}
              prefix={<CheckCircleOutlined style={{ color: '#7c3aed' }} />}
              valueStyle={{ color: '#7c3aed', fontSize: 20 }}
            />
          </Card>
        </Col>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={t('dashboard.activeModels')}
              value={activeModelCount}
              formatter={(v) => formatNumber(Number(v))}
              valueStyle={{ fontSize: 20 }}
            />
          </Card>
        </Col>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={t('dashboard.avgTokensPerRequest')}
              value={avgTokensPerRequest}
              formatter={(v) => formatNumber(Number(v))}
              valueStyle={{ fontSize: 20 }}
            />
          </Card>
        </Col>
        <Col xs={12} sm={8} lg={3}>
          <Card size="small">
            <Statistic
              title={t('dashboard.avgCostPerRequest')}
              value={avgCostPerRequest}
              formatter={(v) => formatCost(Number(v))}
              valueStyle={{ fontSize: 20, color: '#d29922' }}
            />
          </Card>
        </Col>
      </Row>

      {/* Charts */}
      <Row gutter={[16, 16]} style={{ marginBottom: 16 }}>
        <Col xs={24} xl={16}>
          <Card title={t('dashboard.tokenTrend')}>
            {effectiveOverview?.daily_trend && effectiveOverview.daily_trend.length > 0 ? (
              <SvgBarChart data={effectiveOverview.daily_trend} />
            ) : (
              <div style={{ textAlign: 'center', padding: 48 }}>
                <Text type="secondary">{t('common.noData')}</Text>
              </div>
            )}
          </Card>
        </Col>
        <Col xs={24} xl={8}>
          <Card title={t('dashboard.modelDistribution')}>
            {effectiveOverview?.top_models && effectiveOverview.top_models.length > 0 ? (
              <SvgPieChart data={effectiveOverview.top_models} />
            ) : (
              <div style={{ textAlign: 'center', padding: 48 }}>
                <Text type="secondary">{t('common.noData')}</Text>
              </div>
            )}
          </Card>
        </Col>
      </Row>

      {/* Module token usage */}
      <Row gutter={[16, 16]} style={{ marginBottom: 16 }}>
        <Col xs={24}>
          <Card title={t('dashboard.moduleUsage.title')}>
            <Table
              columns={moduleColumns}
              dataSource={moduleUsage ?? []}
              rowKey="module"
              pagination={false}
              size="small"
              scroll={{ x: 'max-content' }}
            />
          </Card>
        </Col>
      </Row>

      {/* Model detail table */}
      <Row gutter={[16, 16]} style={{ marginBottom: 16 }}>
        <Col xs={24}>
          <Card title={t('dashboard.modelUsageDetail')}>
            <Table
              columns={modelColumns}
              dataSource={effectiveOverview?.top_models ?? []}
              rowKey="model"
              pagination={{ pageSize: 10, size: 'small' }}
              size="small"
              scroll={{ x: 'max-content' }}
            />
          </Card>
        </Col>
      </Row>

    </div>
  );
}
