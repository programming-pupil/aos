// ── NL2SQL Analytics — quality metrics dashboard ──────────────────────────────────
// Shows overview stats, routing performance, semantic coverage, and daily trends.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import dayjs from 'dayjs';
import {
  Row,
  Col,
  Card,
  Statistic,
  Table,
  DatePicker,
  Typography,
  Space,
  Button,
  Spin,
  Empty,
  Tag,
  Tooltip,
} from 'antd';
import {
  ThunderboltOutlined,
  CheckCircleOutlined,
  RiseOutlined,
  DatabaseOutlined,
  BarChartOutlined,
  LineChartOutlined,
  ClockCircleOutlined,
} from '@ant-design/icons';
import ReactECharts from 'echarts-for-react';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import { nl2sqlApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { ErrorBoundary } from '@/components/ErrorBoundary';
import type {
  AnalyticsDatasourceHealthRow,
  AnalyticsOverview,
  AnalyticsRouting,
  AnalyticsRuleHits,
  AnalyticsSemanticCoverage,
  AnalyticsTrends,
  SlowQueryItem,
} from '@/types';

const { Text, Title } = Typography;
const { RangePicker } = DatePicker;

type Granularity = '7d' | '30d' | '90d';

function StatCard({
  title,
  value,
  suffix,
  icon,
  color,
  tooltip,
}: {
  title: string;
  value: string | number;
  suffix?: string;
  icon: React.ReactNode;
  color: string;
  tooltip?: string;
}) {
  return (
    <Card size="small" style={{ borderRadius: 12 }}>
      <Tooltip title={tooltip}>
        <Space direction="vertical" size={4} style={{ width: '100%' }}>
          <Space>
            <span style={{ color, fontSize: 16 }}>{icon}</span>
            <Text type="secondary" style={{ fontSize: 12 }}>{title}</Text>
          </Space>
          <div style={{ display: 'flex', alignItems: 'baseline', gap: 4 }}>
            <Text strong style={{ fontSize: 22, color: 'var(--text-primary)' }}>{value}</Text>
            {suffix && <Text type="secondary" style={{ fontSize: 12 }}>{suffix}</Text>}
          </div>
        </Space>
      </Tooltip>
    </Card>
  );
}

function TrendChart({ data }: { data: AnalyticsTrends['daily'] }) {
  const { t } = useTranslation();
  if (!data || data.length === 0) {
    return <Empty description={t('analytics.noTrendData')} image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }
  const safeDaily = data.map((d) => ({
    ...d,
    queries: Number.isFinite(d?.queries as number) ? Number(d.queries) : 0,
    success_rate: Number.isFinite(d?.success_rate as number) ? Number(d.success_rate) : 0,
    avg_confidence: Number.isFinite(d?.avg_confidence as number) ? Number(d.avg_confidence) : 0,
  }));
  const safeRate = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
  const option = {
    tooltip: { trigger: 'axis' },
    legend: { data: [t('analytics.queryVolume'), t('analytics.successRate')], bottom: 0 },
    grid: { left: 72, right: 64, top: 24, bottom: 56, containLabel: true },
    xAxis: {
      type: 'category',
      data: safeDaily.map((d) => d.date),
      axisLabel: { fontSize: 11 },
    },
    yAxis: [
      { type: 'value', name: t('analytics.queryVolume'), nameGap: 24, axisLabel: { fontSize: 11 } },
      { type: 'value', name: t('analytics.successRatePct'), nameGap: 24, max: 100, axisLabel: { fontSize: 11 } },
    ],
    series: [
      {
        name: t('analytics.queryVolume'),
        type: 'bar',
        data: safeDaily.map((d) => d.queries),
        itemStyle: { color: '#7c3aed' },
      },
      {
        name: t('analytics.successRate'),
        type: 'line',
        yAxisIndex: 1,
        data: safeDaily.map((d) => parseFloat(safeRate(d.success_rate).toFixed(1))),
        smooth: true,
        itemStyle: { color: '#10b981' },
        lineStyle: { width: 2 },
      },
    ],
  };
  return <ReactECharts option={option} style={{ height: 280 }} />;
}

function ConfidenceChart({ data }: { data: AnalyticsRouting['confidence_distribution'] }) {
  const { t } = useTranslation();
  if (!data || data.length === 0) {
    return <Empty description={t('analytics.noConfidenceData')} image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }
  const option = {
    tooltip: { trigger: 'item' },
    xAxis: { type: 'category', data: data.map((d) => d.range) },
    yAxis: { type: 'value', name: t('analytics.queryCount') },
    series: [{
      type: 'bar',
      data: data.map((d) => d.count),
      itemStyle: {
        color: (p: { dataIndex: number }) =>
          ['#10b981', '#34d399', '#fbbf24', '#f97316', '#ef4444'][p.dataIndex],
      },
    }],
  };
  return <ReactECharts option={option} style={{ height: 220 }} />;
}

function MethodChart({ data }: { data: AnalyticsRouting['method_distribution'] }) {
  const { t } = useTranslation();
  if (!data || data.length === 0) {
    return <Empty description={t('analytics.noMethodData')} image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }
  const option = {
    tooltip: { trigger: 'item', formatter: '{b}: {c} ({d}%)' },
    legend: { orient: 'vertical', right: 10, top: 'center', textStyle: { fontSize: 11 } },
    series: [{
      type: 'pie',
      radius: ['35%', '65%'],
      center: ['40%', '50%'],
      label: { show: false },
      data: data.map((d) => ({ name: d.method, value: d.count })),
      itemStyle: { borderRadius: 6, borderColor: '#fff', borderWidth: 2 },
    }],
  };
  return <ReactECharts option={option} style={{ height: 220 }} />;
}

function CoverageBar({ data }: { data: AnalyticsSemanticCoverage['datasources'] }) {
  const { t } = useTranslation();
  if (!data || data.length === 0) {
    return <Empty description={t('analytics.noCoverageData')} image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }
  const option = {
    tooltip: { trigger: 'axis', axisPointer: { type: 'shadow' } },
    legend: { data: [t('analytics.tableCoverage'), t('analytics.columnCoverage')], bottom: 0 },
    grid: { left: 120, right: 30, top: 10, bottom: 40 },
    xAxis: { type: 'value', max: 100, name: '%', axisLabel: { fontSize: 11 } },
    yAxis: {
      type: 'category',
      data: data.map((d) => d.datasource_name),
      axisLabel: { fontSize: 11 },
    },
    series: [
      {
        name: t('analytics.tableCoverage'),
        type: 'bar',
        data: data.map((d) => parseFloat(((d.indexed_tables / Math.max(d.total_tables, 1)) * 100).toFixed(1))),
        itemStyle: { color: '#7c3aed' },
      },
      {
        name: t('analytics.columnCoverage'),
        type: 'bar',
        data: data.map((d) => parseFloat(((d.indexed_columns / Math.max(d.total_columns, 1)) * 100).toFixed(1))),
        itemStyle: { color: '#06b6d4' },
      },
    ],
  };
  return <ReactECharts option={option} style={{ height: Math.max(200, data.length * 50 + 30) }} />;
}

function RuleHitCoverageChart({ data }: { data: AnalyticsRuleHits['daily'] }) {
  const { t } = useTranslation();
  if (!data || data.length === 0) {
    return <Empty description={t('analytics.noRuleHitData')} image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }
  const option = {
    tooltip: { trigger: 'axis' },
    legend: { data: [t('analytics.ruleCoverageRate'), t('analytics.ruleHitCount')], bottom: 0 },
    grid: { left: 45, right: 20, top: 20, bottom: 45 },
    xAxis: {
      type: 'category',
      data: data.map((d) => d.date),
      axisLabel: { fontSize: 11 },
    },
    yAxis: [
      { type: 'value', name: '%', max: 100, axisLabel: { fontSize: 11 } },
      { type: 'value', name: t('analytics.ruleHitCount'), axisLabel: { fontSize: 11 } },
    ],
    series: [
      {
        name: t('analytics.ruleCoverageRate'),
        type: 'line',
        smooth: true,
        data: data.map((d) => Number((d.coverage_rate ?? 0).toFixed(1))),
        itemStyle: { color: '#0ea5e9' },
        lineStyle: { width: 2 },
      },
      {
        name: t('analytics.ruleHitCount'),
        type: 'bar',
        yAxisIndex: 1,
        data: data.map((d) => d.total_hits ?? 0),
        itemStyle: { color: '#f59e0b' },
      },
    ],
  };
  return <ReactECharts option={option} style={{ height: 220 }} />;
}

export default function Nl2sqlAnalytics() {
  const { t } = useTranslation();
  const [granularity, setGranularity] = useState<Granularity>('30d');
  const [customRange, setCustomRange] = useState<[dayjs.Dayjs, dayjs.Dayjs] | null>(null);
  const [refreshToken, setRefreshToken] = useState(0);

  const { startDate, endDate, subtitleDays } = useMemo(() => {
    if (customRange) {
      const [start, end] = customRange;
      return {
        startDate: start.format('YYYY-MM-DD'),
        endDate: end.format('YYYY-MM-DD'),
        subtitleDays: Math.max(end.startOf('day').diff(start.startOf('day'), 'day') + 1, 1),
      };
    }
    const now = dayjs();
    const days = granularity === '7d' ? 7 : granularity === '30d' ? 30 : 90;
    return {
      startDate: now.subtract(days - 1, 'day').format('YYYY-MM-DD'),
      endDate: now.format('YYYY-MM-DD'),
      subtitleDays: days,
    };
  }, [granularity, customRange]);

  const { data: overview, isLoading: overviewLoading } = useQuery({
    queryKey: [
      ...queryKeys.nl2sql.analytics.overview({ start_date: startDate, end_date: endDate }),
      { refreshToken },
    ],
    queryFn: () => nl2sqlApi.analyticsOverview({ start_date: startDate, end_date: endDate }),
  });

  const { data: routing, isLoading: routingLoading } = useQuery({
    queryKey: [
      ...queryKeys.nl2sql.analytics.routing({ start_date: startDate, end_date: endDate }),
      { refreshToken },
    ],
    queryFn: () => nl2sqlApi.analyticsRouting({ start_date: startDate, end_date: endDate }),
  });

  const { data: coverage, isLoading: coverageLoading } = useQuery({
    queryKey: queryKeys.nl2sql.analytics.semanticCoverage(),
    queryFn: () => nl2sqlApi.analyticsSemanticCoverage(),
  });

  const { data: trends, isLoading: trendsLoading } = useQuery({
    queryKey: [
      ...queryKeys.nl2sql.analytics.trends({ start_date: startDate, end_date: endDate }),
      { refreshToken },
    ],
    queryFn: () => nl2sqlApi.analyticsTrends({ start_date: startDate, end_date: endDate }),
  });

  const { data: ruleHits, isLoading: ruleHitsLoading } = useQuery({
    queryKey: [
      ...queryKeys.nl2sql.analytics.ruleHits({ start_date: startDate, end_date: endDate }),
      { refreshToken },
    ],
    queryFn: () => nl2sqlApi.analyticsRuleHits({ start_date: startDate, end_date: endDate }),
  });

  const { data: datasourceHealth, isLoading: datasourceHealthLoading } = useQuery({
    queryKey: [
      ...queryKeys.nl2sql.analytics.datasourceHealth({ start_date: startDate, end_date: endDate }),
      { refreshToken },
    ],
    queryFn: () => nl2sqlApi.analyticsDatasourceHealth({ start_date: startDate, end_date: endDate }),
  });

  // F-11: Slow query analysis
  const slowPageSize = 20;
  const {
    data: slowQueriesPages,
    isLoading: slowLoading,
    isFetchingNextPage: slowLoadingMore,
    hasNextPage: slowHasNextPage,
    fetchNextPage: fetchSlowNextPage,
  } = useInfiniteQuery({
    queryKey: [
      ...queryKeys.nl2sql.analytics.slowQueries({
        start_date: startDate,
        end_date: endDate,
        per_page: slowPageSize,
      }),
      { refreshToken },
    ],
    queryFn: ({ pageParam = 1 }) =>
      nl2sqlApi.slowQueries({
        page: Number(pageParam),
        per_page: slowPageSize,
        start_date: startDate,
        end_date: endDate,
      }),
    initialPageParam: 1,
    getNextPageParam: (lastPage, allPages) => {
      const loaded = allPages.reduce((sum, page) => sum + (page.items?.length ?? 0), 0);
      const total = lastPage.total ?? 0;
      if (loaded >= total) return undefined;
      return allPages.length + 1;
    },
  });

  useEffect(() => {
    const onRefresh = () => setRefreshToken((prev) => prev + 1);
    window.addEventListener('nl2sql-analytics-refresh', onRefresh);
    return () => window.removeEventListener('nl2sql-analytics-refresh', onRefresh);
  }, []);

  const isLoading = overviewLoading || routingLoading || coverageLoading || trendsLoading || ruleHitsLoading || datasourceHealthLoading;
  const topTables = routing?.top_routed_tables?.slice(0, 8) ?? [];
  const safeNum = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
  const slowQueriesData = useMemo(
    () => slowQueriesPages?.pages.flatMap((p) => p.items ?? []) ?? [],
    [slowQueriesPages]
  );
  const slowQueriesTotal = slowQueriesPages?.pages?.[0]?.total ?? 0;
  const slowP50 = slowQueriesPages?.pages?.[0]?.p50Ms ?? null;
  const slowP95 = slowQueriesPages?.pages?.[0]?.p95Ms ?? null;
  const slowP99 = slowQueriesPages?.pages?.[0]?.p99Ms ?? null;

  if (isLoading) {
    return (
      <div style={{ display: 'flex', justifyContent: 'center', alignItems: 'center', minHeight: 400 }}>
        <Spin size="large" />
      </div>
    );
  }

  return (
    <ErrorBoundary>
    <div style={{ padding: '14px 24px 24px', height: '100%', overflowY: 'auto', overflowX: 'hidden' }}>
      {/* Header */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 24 }}>
        <div>
          <Title level={4} style={{ margin: 0 }}>{t('analytics.title')}</Title>
          <Text type="secondary" style={{ fontSize: 12 }}>
            {t('analytics.subtitle', { days: subtitleDays })}
          </Text>
        </div>
        <Space>
          <Space.Compact>
            {([
              { label: t('analytics.last7Days'), value: '7d' as Granularity },
              { label: t('analytics.last30Days'), value: '30d' as Granularity },
              { label: t('analytics.last90Days'), value: '90d' as Granularity },
            ]).map((item) => {
              const selected = !customRange && granularity === item.value;
              return (
                <Button
                  key={item.value}
                  type={selected ? 'primary' : 'default'}
                  onClick={() => {
                    setCustomRange(null);
                    setGranularity(item.value);
                    setRefreshToken((prev) => prev + 1);
                  }}
                >
                  {item.label}
                </Button>
              );
            })}
          </Space.Compact>
          <RangePicker
            value={customRange}
            allowClear
            onChange={(values) => {
              if (!values || !values[0] || !values[1]) {
                setCustomRange(null);
                setRefreshToken((prev) => prev + 1);
                return;
              }
              const next: [dayjs.Dayjs, dayjs.Dayjs] = [
                values[0].startOf('day'),
                values[1].endOf('day'),
              ];
              setCustomRange(next);
              setRefreshToken((prev) => prev + 1);
            }}
          />
        </Space>
      </div>

      {/* Overview Stats */}
      <Row gutter={[16, 16]} style={{ marginBottom: 20 }}>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.totalQueries')}
            value={overview?.total_queries?.toLocaleString() ?? '0'}
            icon={<ThunderboltOutlined />}
            color="#7c3aed"
            tooltip={t('analytics.totalQueriesTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.successRate')}
            value={overview?.success_rate != null ? overview.success_rate.toFixed(1) : '0'}
            suffix="%"
            icon={<CheckCircleOutlined />}
            color="#10b981"
            tooltip={t('analytics.successRateTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.avgConfidence')}
            value={overview?.avg_route_confidence != null ? overview.avg_route_confidence.toFixed(2) : '0'}
            icon={<RiseOutlined />}
            color="#06b6d4"
            tooltip={t('analytics.avgConfidenceTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.avgPlanningTime')}
            value={overview?.avg_planning_ms != null ? (overview.avg_planning_ms / 1000).toFixed(2) : '0'}
            suffix={t('analytics.secondsUnit')}
            icon={<ClockCircleOutlined />}
            color="#0ea5e9"
            tooltip={t('analytics.avgPlanningTimeTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.avgExecutionTime')}
            value={overview?.avg_execution_ms != null ? (overview.avg_execution_ms / 1000).toFixed(2) : '0'}
            suffix={t('analytics.secondsUnit')}
            icon={<LineChartOutlined />}
            color="#f97316"
            tooltip={t('analytics.avgExecutionTimeTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.planningExecutionRatio')}
            value={overview?.planning_execution_ratio != null ? overview.planning_execution_ratio.toFixed(2) : '0'}
            suffix="x"
            icon={<BarChartOutlined />}
            color="#22c55e"
            tooltip={t('analytics.planningExecutionRatioTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.cacheHitRate')}
            value={overview?.cache_hit_rate != null ? overview.cache_hit_rate.toFixed(1) : '0'}
            suffix="%"
            icon={<CheckCircleOutlined />}
            color="#a855f7"
            tooltip={t('analytics.cacheHitRateTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.datasourceCount')}
            value={overview?.total_datasources?.toLocaleString() ?? '0'}
            icon={<DatabaseOutlined />}
            color="#2563eb"
            tooltip={t('analytics.datasourceCountTip')}
          />
        </Col>
        <Col xs={12} sm={8} md={6} lg={6}>
          <StatCard
            title={t('analytics.conversationCount')}
            value={overview?.total_conversations?.toLocaleString() ?? '0'}
            icon={<BarChartOutlined />}
            color="#ec4899"
            tooltip={t('analytics.conversationCountTip')}
          />
        </Col>
      </Row>

      {/* Coverage + Confidence */}
      <Row gutter={16} style={{ marginBottom: 20 }}>
        {/* Semantic Coverage */}
        <Col span={14}>
          <Card
            title={
              <Space>
                <DatabaseOutlined style={{ color: '#7c3aed' }} />
                <span>{t('analytics.semanticCoverage')}</span>
              </Space>
            }
            size="small"
            extra={
              <Space>
                <Text type="secondary" style={{ fontSize: 11 }}>
                  {t('analytics.tablesIndexed')}: {overview?.total_tables_indexed ?? 0} | {t('analytics.avgColumnCoverage')}:{' '}
                  {overview?.avg_semantic_coverage != null ? overview.avg_semantic_coverage.toFixed(1) : '0'}%
                </Text>
              </Space>
            }
          >
            <CoverageBar data={coverage?.datasources ?? []} />
          </Card>
        </Col>

        {/* Routing Method Distribution */}
        <Col span={10}>
          <Card
            title={
              <Space>
                <ThunderboltOutlined style={{ color: '#06b6d4' }} />
                <span>{t('analytics.routingMethodDist')}</span>
              </Space>
            }
            size="small"
            extra={
              <Text type="secondary" style={{ fontSize: 11 }}>
                {t('analytics.clarificationRate', { rate: routing?.clarification_rate?.toFixed(1) ?? '0' })}
              </Text>
            }
          >
            <Row gutter={16}>
              <Col span={12}>
                <ConfidenceChart data={routing?.confidence_distribution ?? []} />
              </Col>
              <Col span={12}>
                <MethodChart data={routing?.method_distribution ?? []} />
              </Col>
            </Row>
          </Card>
        </Col>
      </Row>

      {/* Trend Chart */}
      <Row gutter={16} style={{ marginBottom: 20 }}>
        <Col span={24}>
          <Card
            title={
              <Space>
                <LineChartOutlined style={{ color: '#7c3aed' }} />
                <span>{t('analytics.queryTrends')}</span>
              </Space>
            }
            size="small"
          >
            <TrendChart data={trends?.daily ?? []} />
          </Card>
        </Col>
      </Row>

      {/* Top Tables */}
      <Row gutter={16}>
        <Col span={12}>
          <Card
            title={
              <Space>
                <BarChartOutlined style={{ color: '#2563eb' }} />
                <span>{t('analytics.highFreqTables')}</span>
              </Space>
            }
            size="small"
          >
            <Table
              dataSource={topTables.map((t, i) => ({ rank: i + 1, ...t }))}
              columns={[
                { title: '#', dataIndex: 'rank', width: 40 },
                { title: t('analytics.tableName'), dataIndex: 'table', render: (v: string) => <Tag>{v}</Tag> },
                {
                  title: t('analytics.queryCount'),
                  dataIndex: 'count',
                  width: 100,
                  render: (v: number) => <Text strong>{v}</Text>,
                },
              ]}
              rowKey="table"
              size="small"
              pagination={false}
            />
          </Card>
        </Col>
        <Col span={12}>
          <Card
            title={
              <Space>
                <CheckCircleOutlined style={{ color: '#10b981' }} />
                <span>{t('analytics.confidence')}</span>
              </Space>
            }
            size="small"
          >
            <Table
              dataSource={routing?.confidence_distribution ?? []}
              columns={[
                { title: t('analytics.confidenceRange'), dataIndex: 'range', width: 120 },
                {
                  title: t('analytics.queryCount'),
                  dataIndex: 'count',
                  width: 100,
                  render: (v: number) => <Text strong>{v}</Text>,
                },
                {
                  title: t('analytics.percentage'),
                  key: 'pct',
                  width: 80,
                  render: (_: unknown, r: { range: string; count: number }) => {
                    const total = (routing?.confidence_distribution ?? []).reduce((s, d) => s + d.count, 0);
                    const pct = total > 0 ? ((r.count / total) * 100).toFixed(1) : '0';
                    return <Text type="secondary">{pct}%</Text>;
                  },
                },
              ]}
              rowKey="range"
              size="small"
              pagination={false}
            />
          </Card>
        </Col>
      </Row>

      {/* Rule Hit Analytics */}
      <Row gutter={16} style={{ marginTop: 20, marginBottom: 20 }}>
        <Col span={24} style={{ marginBottom: 16 }}>
          <Card
            title={
              <Space>
                <DatabaseOutlined style={{ color: '#2563eb' }} />
                <span>{t('analytics.datasourceHealth')}</span>
              </Space>
            }
            size="small"
            extra={
              <Text type="secondary" style={{ fontSize: 12 }}>
                {t('analytics.datasourceHealthRows', { count: datasourceHealth?.total ?? 0 })}
              </Text>
            }
          >
            <Table
              dataSource={datasourceHealth?.rows ?? []}
              rowKey={(r: AnalyticsDatasourceHealthRow) => r.datasource_id || r.datasource_name}
              size="small"
              pagination={false}
              locale={{ emptyText: t('common.noData') }}
              scroll={{ x: 920 }}
              columns={[
                {
                  title: t('analytics.datasourceName'),
                  dataIndex: 'datasource_name',
                  width: 220,
                  render: (v: string) => <Text strong>{v || '-'}</Text>,
                },
                {
                  title: t('analytics.queryCount'),
                  dataIndex: 'total_queries',
                  width: 110,
                  render: (v: number) => v?.toLocaleString() ?? '0',
                },
                {
                  title: t('analytics.successRate'),
                  dataIndex: 'success_rate',
                  width: 110,
                  render: (v: number) => `${safeNum(v).toFixed(1)}%`,
                },
                {
                  title: t('analytics.failedQueries'),
                  dataIndex: 'failed_queries',
                  width: 110,
                  render: (v: number) => v?.toLocaleString() ?? '0',
                },
                {
                  title: t('analytics.avgExecutionTime'),
                  dataIndex: 'avg_execution_ms',
                  width: 130,
                  render: (v: number) => `${(safeNum(v) / 1000).toFixed(2)}s`,
                },
                {
                  title: t('analytics.p95ExecutionTime'),
                  dataIndex: 'p95_execution_ms',
                  width: 130,
                  render: (v: number | null) => (v != null ? `${(safeNum(v) / 1000).toFixed(2)}s` : '-'),
                },
              ]}
            />
          </Card>
        </Col>
        <Col span={24}>
          <Card
            title={
              <Space>
                <CheckCircleOutlined style={{ color: '#0ea5e9' }} />
                <span>{t('analytics.ruleHitAnalytics')}</span>
              </Space>
            }
            size="small"
            extra={
              <Space>
                <Tag color="blue">
                  {t('analytics.ruleCoverageRate')}: {ruleHits?.coverage_rate?.toFixed(1) ?? '0'}%
                </Tag>
                <Tag color="gold">
                  {t('analytics.ruleHitCount')}: {ruleHits?.total_rule_hits?.toLocaleString() ?? '0'}
                </Tag>
              </Space>
            }
          >
            <Row gutter={16}>
              <Col span={14}>
                <RuleHitCoverageChart data={ruleHits?.daily ?? []} />
              </Col>
              <Col span={10}>
                <Table
                  dataSource={(ruleHits?.top_rules ?? []).slice(0, 8)}
                  rowKey="rule_key"
                  size="small"
                  pagination={false}
                  columns={[
                    {
                      title: t('analytics.ruleName'),
                      dataIndex: 'rule_name',
                      ellipsis: true,
                      render: (v: string) => <Text>{v || '-'}</Text>,
                    },
                    {
                      title: t('analytics.hitQueries'),
                      dataIndex: 'queries',
                      width: 90,
                      render: (v: number) => <Text strong>{v}</Text>,
                    },
                    {
                      title: t('analytics.percentage'),
                      width: 90,
                      render: (_: unknown, r: { query_hit_rate: number }) => (
                        <Text type="secondary">{(r.query_hit_rate ?? 0).toFixed(1)}%</Text>
                      ),
                    },
                  ]}
                />
              </Col>
            </Row>
          </Card>
        </Col>
      </Row>

      {/* F-11: Slow Query Analysis */}
      <Row gutter={16} style={{ marginBottom: 20 }}>
        <Col span={24}>
          <Card
            title={
              <Space>
                <ClockCircleOutlined style={{ color: '#f97316' }} />
                <span>{t('analytics.slowQueries')}</span>
                <Tag color="orange">{slowQueriesTotal} total</Tag>
              </Space>
            }
            extra={
              <Space>
                {slowP50 != null && (
                  <Tag color="green">P50: {(safeNum(slowP50) / 1000).toFixed(2)}s</Tag>
                )}
                {slowP95 != null && (
                  <Tag color="orange">P95: {(safeNum(slowP95) / 1000).toFixed(2)}s</Tag>
                )}
                {slowP99 != null && (
                  <Tag color="red">P99: {(safeNum(slowP99) / 1000).toFixed(2)}s</Tag>
                )}
              </Space>
            }
            size="small"
          >
            {slowLoading ? (
              <Spin />
            ) : (
              <>
                <Table
                  dataSource={slowQueriesData}
                  rowKey="id"
                  size="small"
                  pagination={false}
                  scroll={{ y: 460 }}
                  onScroll={(evt) => {
                    const target = evt.currentTarget as HTMLDivElement;
                    const nearBottom = target.scrollTop + target.clientHeight >= target.scrollHeight - 32;
                    if (nearBottom && slowHasNextPage && !slowLoadingMore) {
                      void fetchSlowNextPage();
                    }
                  }}
                  columns={[
                    {
                      title: t('analytics.question'),
                      dataIndex: 'question',
                      ellipsis: true,
                      width: '40%',
                      render: (q: string) => <Text type="secondary">{q}</Text>,
                    },
                    {
                      title: t('analytics.executionTime'),
                      dataIndex: 'executionMs',
                      width: 140,
                      sorter: (a: SlowQueryItem, b: SlowQueryItem) => a.executionMs - b.executionMs,
                      defaultSortOrder: 'descend' as const,
                      render: (ms: number) => {
                        if (ms >= 5000) return <Tag color="red">{(ms / 1000).toFixed(1)}s</Tag>;
                        if (ms >= 1000) return <Tag color="orange">{(ms / 1000).toFixed(1)}s</Tag>;
                        return <Tag color="green">{(ms / 1000).toFixed(1)}s</Tag>;
                      },
                    },
                    {
                      title: t('analytics.rowsReturned'),
                      dataIndex: 'rowsReturned',
                      width: 100,
                      render: (r: number | null) => r?.toLocaleString() ?? '-',
                    },
                    {
                      title: t('analytics.generatedSql'),
                      dataIndex: 'generatedSql',
                      ellipsis: true,
                      render: (sql: string | null) =>
                        sql ? (
                          <Tooltip title={<pre style={{ margin: 0, fontSize: 11 }}>{sql}</pre>}>
                            <Text code style={{ fontSize: 11, cursor: 'pointer' }}>
                              {sql.substring(0, 60)}...
                            </Text>
                          </Tooltip>
                        ) : (
                          '-'
                        ),
                    },
                    {
                      title: t('analytics.createdAt'),
                      dataIndex: 'createdAt',
                      width: 160,
                      render: (ts: string) => <Text type="secondary" style={{ fontSize: 11 }}>{ts}</Text>,
                    },
                  ]}
                />
                {slowLoadingMore && (
                  <div style={{ textAlign: 'center', paddingTop: 12 }}>
                    <Spin size="small" />
                  </div>
                )}
              </>
            )}
          </Card>
        </Col>
      </Row>
    </div>
    </ErrorBoundary>
  );
}
