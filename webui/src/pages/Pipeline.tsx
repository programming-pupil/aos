import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import {
  Alert,
  Button,
  Card,
  Drawer,
  Empty,
  Input,
  Modal,
  Select,
  Space,
  Spin,
  Table,
  Tag,
  Timeline,
  Typography,
  message,
} from 'antd';
import {
  CheckCircleOutlined,
  CodeOutlined,
  ExperimentOutlined,
  FileTextOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
  RollbackOutlined,
  RobotOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@/router';
import { useTranslation } from 'react-i18next';
import dayjs from 'dayjs';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { Markdown } from '@/components/chat';
import { RdTokenRootCauseCard, mergeRdTaskDiagnosticEvents } from '@/components/rd/RdTokenRootCauseCard';
import { usePermissions } from '@/store/permissions';
import type { RdFileChange, RdTask, RdTaskEvent } from '@/types';
import { isRdFileChangeApplicable, rdFileChangeNotApplicableReason } from '@/utils/rdChanges';
import { cleanRdPromptForDisplay } from '@/utils/rdDisplay';

const { Text, Title } = Typography;
const RD_TASK_EVENT_PAGE_SIZE = 20;
const DIFF_COLLAPSE_LINE_LIMIT = 520;
const DIFF_PREVIEW_HEAD_LINES = 300;
const DIFF_PREVIEW_TAIL_LINES = 120;

const STATUS_COLORS: Record<string, string> = {
  queued: 'default',
  running: 'processing',
  waiting_approval: 'warning',
  completed: 'success',
  failed: 'error',
  cancelled: 'default',
  passed: 'success',
  timeout: 'error',
};

const TASK_STATUS_VALUES = ['queued', 'running', 'waiting_approval', 'completed', 'failed', 'cancelled'] as const;

type RdTokenUsageRow = {
  key: string;
  stage: string;
  label: string;
  model?: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  totalTokens: number;
};

const RD_TOKEN_STAGE_FALLBACKS: Record<string, string> = {
  context_plan_llm: 'LLM 上下文规划',
  runtime_usage: 'Runtime 主循环',
  runtime: 'Runtime 主循环',
  summary: '主生成',
};

function statusTag(status?: string | null, label?: string) {
  if (!status) return null;
  return <Tag color={STATUS_COLORS[status] ?? 'blue'}>{label ?? status}</Tag>;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function runtimeTokenNumber(record: Record<string, unknown> | null, keys: string[]): number {
  for (const key of keys) {
    const value = record?.[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return 0;
}

function tokenUsageRowFromEvent(event: RdTaskEvent, t: ReturnType<typeof useTranslation>['t']): RdTokenUsageRow | null {
  const detail = asRecord(event.detailJson);
  if (!detail) return null;
  const usage = asRecord(detail.usage) ?? detail;
  const inputTokens = runtimeTokenNumber(usage, ['inputTokens', 'input_tokens']);
  const outputTokens = runtimeTokenNumber(usage, ['outputTokens', 'output_tokens']);
  const cacheCreationTokens = runtimeTokenNumber(usage, ['cacheCreationTokens', 'cache_creation_tokens', 'cacheCreationInputTokens', 'cache_creation_input_tokens']);
  const cacheReadTokens = runtimeTokenNumber(usage, ['cacheReadTokens', 'cache_read_tokens', 'cacheReadInputTokens', 'cache_read_input_tokens']);
  const totalTokens = runtimeTokenNumber(usage, ['totalTokens', 'total_tokens'])
    || inputTokens + outputTokens + cacheCreationTokens + cacheReadTokens;
  if (totalTokens <= 0) return null;
  const model = typeof usage.model === 'string'
    ? usage.model
    : typeof detail.model === 'string'
      ? detail.model
      : undefined;
  return {
    key: `${event.stage}-${event.id}`,
    stage: event.stage,
    label: t(`rd.tokenStages.${event.stage}`, RD_TOKEN_STAGE_FALLBACKS[event.stage] || event.stage),
    model,
    inputTokens,
    outputTokens,
    cacheCreationTokens,
    cacheReadTokens,
    totalTokens,
  };
}

function buildRdStageTokenUsageRows(events: RdTaskEvent[], t: ReturnType<typeof useTranslation>['t']): RdTokenUsageRow[] {
  const rows = events
    .map((event) => tokenUsageRowFromEvent(event, t))
    .filter((row): row is RdTokenUsageRow => !!row);
  const hasRuntimeUsage = rows.some((row) => row.stage === 'runtime_usage');
  const hasRuntimeCompleted = rows.some((row) => row.stage === 'runtime');
  return rows
    .filter((row) => {
      if (row.stage === 'runtime' && hasRuntimeUsage) return false;
      if (row.stage === 'summary' && (hasRuntimeUsage || hasRuntimeCompleted)) return false;
      return ['context_plan_llm', 'runtime_usage', 'runtime', 'summary'].includes(row.stage);
    })
    .sort((a, b) => {
      const order = ['context_plan_llm', 'runtime_usage', 'runtime', 'summary'];
      return order.indexOf(a.stage) - order.indexOf(b.stage);
    });
}

function DiffBlock({ change }: { change: RdFileChange }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const lines = useMemo(() => change.diffPatch.split('\n'), [change.diffPatch]);
  const shouldCollapse = lines.length > DIFF_COLLAPSE_LINE_LIMIT;
  const visibleLines = useMemo(() => {
    if (!shouldCollapse || expanded) return lines;
    return [
      ...lines.slice(0, DIFF_PREVIEW_HEAD_LINES),
      `... ${t('rd.diffHiddenLines', '已折叠 {{count}} 行 Diff，点击展开查看完整内容', {
        count: Math.max(0, lines.length - DIFF_PREVIEW_HEAD_LINES - DIFF_PREVIEW_TAIL_LINES),
      })} ...`,
      ...lines.slice(-DIFF_PREVIEW_TAIL_LINES),
    ];
  }, [expanded, lines, shouldCollapse, t]);
  return (
    <Space direction="vertical" size={8} style={{ width: '100%' }}>
      {shouldCollapse ? (
        <Alert
          type="info"
          showIcon
          message={t('rd.largeDiffPreview', '大 Diff 已启用预览渲染')}
          description={t('rd.largeDiffPreviewDesc', '为避免详情抽屉卡顿，默认只渲染头尾关键行；需要审查完整内容时再展开。')}
          action={(
            <Button size="small" onClick={() => setExpanded((value) => !value)}>
              {expanded ? t('common.collapse', '收起') : t('rd.expandFullDiff', '展开完整 Diff')}
            </Button>
          )}
        />
      ) : null}
      <pre
        style={{
          maxHeight: 420,
          overflow: 'auto',
          margin: 0,
          padding: 14,
          borderRadius: 12,
          background: '#07111f',
          color: '#dbeafe',
          fontFamily: 'var(--font-code, "JetBrains Mono", monospace)',
          fontSize: 12,
          lineHeight: 1.6,
        }}
      >
        {visibleLines.map((line, index) => {
          const color = line.startsWith('+') && !line.startsWith('+++')
            ? '#86efac'
            : line.startsWith('-') && !line.startsWith('---')
            ? '#fca5a5'
            : line.startsWith('@@')
            ? '#93c5fd'
            : line.startsWith('...')
            ? '#facc15'
            : '#dbeafe';
          return <div key={`${index}-${line.slice(0, 12)}`} style={{ color }}>{line || ' '}</div>;
        })}
      </pre>
    </Space>
  );
}

function DetailSection({
  title,
  children,
  extra,
  defaultOpen = false,
}: {
  title: ReactNode;
  children: ReactNode;
  extra?: ReactNode;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);

  useEffect(() => {
    if (defaultOpen) {
      setOpen(true);
    }
  }, [defaultOpen]);

  function toggleOpen() {
    setOpen((value) => !value);
  }

  return (
    <Card
      size="small"
      styles={{
        header: { cursor: 'pointer' },
        body: { display: open ? undefined : 'none' },
      }}
      title={
        <Space
          role="button"
          tabIndex={0}
          onClick={toggleOpen}
          onKeyDown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault();
              toggleOpen();
            }
          }}
        >
          <span
            aria-hidden="true"
            style={{
              display: 'inline-block',
              transform: open ? 'rotate(90deg)' : 'rotate(0deg)',
              transition: 'transform 160ms ease',
            }}
          >
            &gt;
          </span>
          {title}
        </Space>
      }
      extra={extra ? <span onClick={(event) => event.stopPropagation()}>{extra}</span> : undefined}
    >
      {open ? children : null}
    </Card>
  );
}

export default function Pipeline() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const hasPermission = usePermissions((state) => state.hasPermission);
  const canUseCodeTasks = hasPermission('pipeline:read');
  const canApply = canUseCodeTasks;
  const canRunCommand = canUseCodeTasks;
  const [status, setStatus] = useState<string | undefined>();
  const [repositoryId, setRepositoryId] = useState<string | undefined>();
  const [page, setPage] = useState(1);
  const [selectedTaskId, setSelectedTaskId] = useState<string | undefined>();
  const [testCommand, setTestCommand] = useState('');
  const [manualRefreshing, setManualRefreshing] = useState(false);

  const repositoriesQuery = useQuery({ queryKey: queryKeys.rd.repositories(), queryFn: rdApi.listRepositories });
  const repositories = repositoriesQuery.data?.repositories ?? [];
  const repositoryNameMap = useMemo(() => new Map(repositories.map((repo) => [repo.id, repo.name])), [repositories]);

  const params = useMemo(() => ({ status, repositoryId, page, perPage: 20 }), [status, repositoryId, page]);
  const tasksQuery = useQuery({
    queryKey: queryKeys.rd.tasks(params),
    queryFn: () => rdApi.listTasks(params),
    refetchInterval: (query) => (query.state.data?.tasks ?? []).some((task) => ['queued', 'running'].includes(task.status)) ? 2500 : false,
  });
  const tasks = tasksQuery.data?.tasks ?? [];
  async function handleManualRefresh() {
    setManualRefreshing(true);
    try {
      await tasksQuery.refetch();
    } finally {
      setManualRefreshing(false);
    }
  }

  const selectedTaskQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.task(selectedTaskId) : ['rd', 'task', 'none'],
    queryFn: () => rdApi.getTask(selectedTaskId!),
    enabled: !!selectedTaskId,
    refetchInterval: (query) => {
      const task = query.state.data;
      return task && ['queued', 'running'].includes(task.status) ? 2500 : false;
    },
  });
  const selectedTask = selectedTaskQuery.data ?? null;

  const eventsQuery = useInfiniteQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskEvents(selectedTaskId, { perPage: RD_TASK_EVENT_PAGE_SIZE }) : ['rd', 'events', 'none'],
    queryFn: ({ pageParam }) =>
      rdApi.taskEventsPage(selectedTaskId!, {
        cursorBefore: typeof pageParam === 'number' ? pageParam : undefined,
        perPage: RD_TASK_EVENT_PAGE_SIZE,
      }),
    enabled: !!selectedTaskId,
    initialPageParam: null as number | null,
    getNextPageParam: (lastPage) => lastPage.hasMore ? lastPage.nextCursor ?? undefined : undefined,
    refetchInterval: selectedTask && ['queued', 'running'].includes(selectedTask.status) ? 2500 : false,
  });
  const tokenDiagnosticsQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskTokenDiagnostics(selectedTaskId) : ['rd', 'tokenDiagnostics', 'none'],
    queryFn: () => rdApi.taskTokenDiagnostics(selectedTaskId!),
    enabled: !!selectedTaskId,
    refetchInterval: selectedTask && ['queued', 'running'].includes(selectedTask.status) ? 2500 : false,
  });
  const changesQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskChanges(selectedTaskId) : ['rd', 'changes', 'none'],
    queryFn: () => rdApi.taskChanges(selectedTaskId!),
    enabled: !!selectedTaskId,
    refetchInterval: (query) => {
      if (!selectedTask) return false;
      if (['queued', 'running', 'waiting_approval'].includes(selectedTask.status)) return 2500;
      const data = query.state.data ?? [];
      const recentlyCompleted = selectedTask.completedAt
        ? dayjs().diff(dayjs(selectedTask.completedAt), 'second') < 20
        : dayjs().diff(dayjs(selectedTask.updatedAt), 'second') < 20;
      if (selectedTask.mode === 'modify' && selectedTask.status === 'completed' && data.length === 0 && recentlyCompleted) {
        return 2500;
      }
      return false;
    },
  });
  const testsQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskTests(selectedTaskId) : ['rd', 'tests', 'none'],
    queryFn: () => rdApi.taskTests(selectedTaskId!),
    enabled: !!selectedTaskId,
  });

  const applyMutation = useMutation({
    mutationFn: (changeIds?: string[]) => rdApi.applyChanges(selectedTaskId!, changeIds),
    onSuccess: (res) => {
      message.success(t('rd.applySuccess', '已应用 {{count}} 个修改', { count: res.applied }));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.task(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskChanges(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(params) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.applyFailed', '应用失败')),
  });

  const rollbackMutation = useMutation({
    mutationFn: (changeIds?: string[]) => rdApi.rollbackChanges(selectedTaskId!, changeIds),
    onSuccess: (res) => {
      message.success(t('rd.rollbackSuccess', '已回滚 {{count}} 个修改', { count: res.rolledBack }));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.task(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskChanges(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(params) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.rollbackFailed', '回滚失败')),
  });

  const testMutation = useMutation({
    mutationFn: () => rdApi.runTest(selectedTaskId!, testCommand.trim()),
    onSuccess: () => {
      message.success(t('rd.testStarted', '测试命令已执行'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTests(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId!) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.testFailed', '测试失败')),
  });

  function confirmApply(change?: RdFileChange) {
    if (change && !isRdFileChangeApplicable(change)) {
      message.warning(t('rd.changeNotApplicable', '该 Diff 已失效或包含内部运行时路径，不能应用'));
      return;
    }
    Modal.confirm({
      title: t('rd.confirmApplyTitle', '确认应用代码修改？'),
      content: change ? change.filePath : t('rd.confirmApplyAll', '将应用当前任务的所有未应用 Diff。请确认你已经审查过修改内容。'),
      okText: t('rd.applyPatch', '应用修改'),
      okButtonProps: { danger: true },
      cancelText: t('common.cancel'),
      onOk: () => applyMutation.mutate(change ? [change.id] : applicableChanges.map((item) => item.id)),
    });
  }

  function confirmRollback(change?: RdFileChange) {
    Modal.confirm({
      title: t('rd.confirmRollbackTitle', '确认回滚代码修改？'),
      content: change
        ? t('rd.confirmRollbackOne', '将对 {{file}} 反向应用该 Diff，撤回已应用的修改。', { file: change.filePath })
        : t('rd.confirmRollbackAll', '将按应用时间倒序回滚当前任务所有已应用 Diff。'),
      okText: t('rd.rollbackPatch', '回滚修改'),
      okButtonProps: { danger: true },
      cancelText: t('common.cancel'),
      onOk: () => rollbackMutation.mutate(change ? [change.id] : undefined),
    });
  }

  function confirmRunTest() {
    const command = testCommand.trim();
    if (!command) {
      message.warning(t('rd.noTestCommand', '未配置测试命令'));
      return;
    }
    Modal.confirm({
      title: t('rd.confirmRunTestTitle', '确认运行测试命令？'),
      content: command,
      okText: t('rd.runTest', '运行测试'),
      cancelText: t('common.cancel'),
      onOk: () => testMutation.mutate(),
    });
  }

  function openTaskInStudio(task: RdTask, followUp = false) {
    const search = new URLSearchParams({ taskId: task.id });
    if (task.repositoryId) {
      search.set('repositoryId', task.repositoryId);
    }
    if (followUp) {
      search.set('followUp', '1');
    }
    navigate(`/agent?${search.toString()}`);
  }

  const statusLabel = (value?: string | null) => {
    const raw = value?.trim();
    if (!raw) return '';
    return t(`rd.statuses.${raw.toLowerCase()}`, { defaultValue: raw });
  };
  const renderStatusTag = (value?: string | null) => statusTag(value, statusLabel(value));

  const columns: ColumnsType<RdTask> = [
    {
      title: t('rd.taskTitle', '任务'),
      dataIndex: 'title',
      width: 300,
      render: (title: string, record) => (
        <Space direction="vertical" size={0}>
          <Text strong>{title}</Text>
          <Text type="secondary" style={{ fontSize: 12 }}>{record.prompt.slice(0, 90)}</Text>
        </Space>
      ),
    },
    {
      title: t('rd.repository', '仓库'),
      dataIndex: 'repositoryId',
      width: 180,
      render: (id?: string | null) => id ? repositoryNameMap.get(id) ?? id : t('common.na'),
    },
    {
      title: t('common.status'),
      dataIndex: 'status',
      width: 130,
      render: renderStatusTag,
    },
    {
      title: t('common.model'),
      dataIndex: 'model',
      width: 180,
      render: (value?: string | null) => value || t('common.na'),
    },
    {
      title: t('common.createdAt'),
      dataIndex: 'createdAt',
      width: 180,
      render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm:ss'),
    },
    {
      title: t('common.actions'),
      key: 'actions',
      fixed: 'right',
      width: 180,
      render: (_, record) => (
        <Space>
          <Button size="small" onClick={() => setSelectedTaskId(record.id)}>{t('common.viewDetail')}</Button>
          <Button size="small" type="link" onClick={() => openTaskInStudio(record, true)}>{t('rd.followUp', '追问')}</Button>
        </Space>
      ),
    },
  ];

  const changes = changesQuery.data ?? [];
  const isDiffLoading = changes.length === 0 && (changesQuery.isLoading || changesQuery.isFetching);
  const applicableChanges = useMemo(() => changes.filter(isRdFileChangeApplicable), [changes]);
  const hasPendingChanges = applicableChanges.length > 0;
  const hasAppliedChanges = changes.some((change) => change.applied);
  const tests = testsQuery.data ?? [];
  const events = eventsQuery.data?.pages.flatMap((page) => page.events) ?? [];
  const tokenDiagnosticEvents = useMemo(
    () => mergeRdTaskDiagnosticEvents(events, tokenDiagnosticsQuery.data?.events ?? []),
    [events, tokenDiagnosticsQuery.data?.events],
  );
  const stageTokenUsageRows = useMemo(() => buildRdStageTokenUsageRows(tokenDiagnosticEvents, t), [tokenDiagnosticEvents, t]);
  const stageTokenUsageTotal = useMemo(
    () => stageTokenUsageRows.reduce((sum, row) => sum + row.totalTokens, 0),
    [stageTokenUsageRows],
  );
  const selectedPromptDisplay = selectedTask ? cleanRdPromptForDisplay(selectedTask.prompt) : '';
  const canRunTestCommand = !!testCommand.trim();

  return (
    <div style={{ padding: 24 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 16, marginBottom: 16 }}>
        <div>
          <Title level={4} style={{ margin: 0 }}>{t('pipeline.title', '代码任务')}</Title>
          <Text type="secondary">{t('pipeline.subtitle', '展示研发 Agent 的计划、Diff、测试与最终结果')}</Text>
        </div>
        <Space>
          <Button icon={<ReloadOutlined />} loading={manualRefreshing} onClick={handleManualRefresh}>{t('pipeline.refresh')}</Button>
          <Button type="primary" icon={<RobotOutlined />} onClick={() => navigate('/agent')}>{t('rd.openStudio', '打开 代码开发')}</Button>
        </Space>
      </div>

      <Card style={{ marginBottom: 16 }}>
        <Space wrap>
          <Select allowClear style={{ width: 180 }} placeholder={t('common.status')} value={status} onChange={(value) => { setStatus(value); setPage(1); }} options={TASK_STATUS_VALUES.map((value) => ({ value, label: statusLabel(value) }))} />
          <Select allowClear showSearch style={{ width: 240 }} placeholder={t('rd.repository', '仓库')} value={repositoryId} onChange={(value) => { setRepositoryId(value); setPage(1); }} options={repositories.map((repo) => ({ value: repo.id, label: repo.name }))} />
          <Button onClick={() => { setStatus(undefined); setRepositoryId(undefined); setPage(1); }}>{t('common.clearFilters')}</Button>
        </Space>
      </Card>

      <Card styles={{ body: { padding: 0 } }}>
        <Table
          rowKey="id"
          columns={columns}
          dataSource={tasks}
          loading={tasksQuery.isLoading}
          scroll={{ x: 'max-content' }}
          pagination={{
            current: page,
            pageSize: 20,
            total: tasksQuery.data?.total ?? 0,
            onChange: setPage,
            showSizeChanger: false,
          }}
        />
      </Card>

      <Drawer
        title={selectedTask ? <Space><CodeOutlined /> {selectedTask.title} {renderStatusTag(selectedTask.status)}</Space> : t('common.detail')}
        open={!!selectedTaskId}
        onClose={() => setSelectedTaskId(undefined)}
        width={960}
        extra={selectedTask ? (
          <Button type="primary" icon={<RobotOutlined />} onClick={() => openTaskInStudio(selectedTask, true)}>
            {t('rd.followUp', '追问')}
          </Button>
        ) : null}
      >
        {!selectedTask ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} /> : (
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            <DetailSection key={`requirement-${selectedTask.id}`} title={t('rd.requirement', '需求')} defaultOpen>
              {selectedPromptDisplay ? (
                <div className="rd-task-requirement-markdown">
                  <Markdown suppressHr>{selectedPromptDisplay}</Markdown>
                </div>
              ) : (
                <Text type="secondary">{t('rd.emptyDisplayPrompt', '需求内容为空或仅包含分隔符')}</Text>
              )}
              {selectedTask.errorMessage ? <Tag color="error">{selectedTask.errorMessage}</Tag> : null}
            </DetailSection>
            <DetailSection key={`token-root-cause-${selectedTask.id}`} title={t('rd.tokenRootCauseTitle', 'Token 根因诊断')} defaultOpen>
              <RdTokenRootCauseCard
                events={tokenDiagnosticEvents}
                loading={tokenDiagnosticsQuery.isLoading}
                embedded
              />
            </DetailSection>
            <DetailSection key={`timeline-${selectedTask.id}`} title={t('rd.timeline', '执行时间线')} defaultOpen>
              {events.length === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} /> : (
                <div
                  style={{ maxHeight: 420, overflowY: 'auto', paddingRight: 6 }}
                  onScroll={(event) => {
                    const target = event.currentTarget;
                    const nearBottom = target.scrollTop + target.clientHeight >= target.scrollHeight - 32;
                    if (nearBottom && eventsQuery.hasNextPage && !eventsQuery.isFetchingNextPage) {
                      void eventsQuery.fetchNextPage();
                    }
                  }}
                >
                  <Timeline
                    items={events.map((event) => ({
                      color: event.status === 'failed' ? 'red' : event.status === 'running' ? 'blue' : event.status === 'waiting_approval' ? 'orange' : 'green',
                      children: (
                        <div>
                          <Space>{event.stage}{renderStatusTag(event.status)}</Space>
                          <div><Text type="secondary">{event.message}</Text></div>
                          <Text type="secondary" style={{ fontSize: 12 }}>{dayjs(event.createdAt).format('YYYY-MM-DD HH:mm:ss')}</Text>
                        </div>
                      ),
                    }))}
                  />
                  <div style={{ textAlign: 'center', padding: '4px 0 8px' }}>
                    {eventsQuery.isFetchingNextPage ? (
                      <Spin size="small" />
                    ) : eventsQuery.hasNextPage ? (
                      <Button
                        type="link"
                        size="small"
                        onClick={() => {
                          void eventsQuery.fetchNextPage();
                        }}
                      >
                        {t('rd.scrollToLoadMoreEvents', '下滑加载更早事件')}
                      </Button>
                    ) : null}
                  </div>
                </div>
              )}
            </DetailSection>
            {stageTokenUsageRows.length > 0 ? (
              <DetailSection key={`tokens-${selectedTask.id}`} title={t('rd.stageTokenBill', 'Token 账单')} defaultOpen>
                <Space direction="vertical" size={10} style={{ width: '100%' }}>
                  <Alert
                    type="info"
                    showIcon
                    message={t('rd.stageTokenBillTitle', '阶段级 Token 消耗')}
                    description={t('rd.stageTokenBillDesc', '这里只展示任务事件里的阶段明细，不会额外计费；真实总账仍以系统 token_usage 记录为准。')}
                  />
                  <Space wrap>
                    <Tag color="geekblue">{t('rd.totalTokens', '总 Token')}: {stageTokenUsageTotal.toLocaleString()}</Tag>
                    <Tag color="blue">{t('rd.stageCount', '{{count}} 个阶段', { count: stageTokenUsageRows.length })}</Tag>
                  </Space>
                  {stageTokenUsageRows.map((row) => (
                    <Card key={row.key} size="small" style={{ background: '#0b1220' }}>
                      <Space direction="vertical" size={6} style={{ width: '100%' }}>
                        <Space wrap>
                          <Tag color={row.stage === 'context_plan_llm' ? 'purple' : row.stage.startsWith('runtime') ? 'cyan' : 'blue'}>
                            {row.label}
                          </Tag>
                          {row.model ? <Tag>{row.model}</Tag> : null}
                          <Tag color="geekblue">{t('rd.totalTokens', '总 Token')}: {row.totalTokens.toLocaleString()}</Tag>
                        </Space>
                        <Space wrap size={[6, 6]}>
                          <Tag>{t('rd.inputTokens', '输入')}: {row.inputTokens.toLocaleString()}</Tag>
                          <Tag>{t('rd.outputTokens', '输出')}: {row.outputTokens.toLocaleString()}</Tag>
                          {row.cacheCreationTokens > 0 ? <Tag color="gold">{t('rd.cacheWriteTokens', '缓存写入')}: {row.cacheCreationTokens.toLocaleString()}</Tag> : null}
                          {row.cacheReadTokens > 0 ? <Tag color="green">{t('rd.cacheReadTokens', '缓存读取')}: {row.cacheReadTokens.toLocaleString()}</Tag> : null}
                        </Space>
                      </Space>
                    </Card>
                  ))}
                </Space>
              </DetailSection>
            ) : null}
            <DetailSection key={`plan-${selectedTask.id}`} title={<Space><FileTextOutlined /> {t('rd.plan', '执行计划')}</Space>} defaultOpen={!!selectedTask.planMd}>
              {selectedTask.planMd ? <Markdown>{selectedTask.planMd}</Markdown> : <Text type="secondary">{t('rd.planPending', '等待 Agent 生成计划...')}</Text>}
            </DetailSection>
            <DetailSection key={`answer-${selectedTask.id}`} title={t('rd.answer', '结果总结')} defaultOpen={!!selectedTask.answerMd}>
              {selectedTask.answerMd ? <Markdown>{selectedTask.answerMd}</Markdown> : <Text type="secondary">{t('rd.answerPending', '结果生成中...')}</Text>}
            </DetailSection>
            {selectedTask.reviewMd ? (
              <DetailSection key={`review-${selectedTask.id}`} title={t('rd.review', '代码审查')}>
                <Markdown relaxed>{selectedTask.reviewMd}</Markdown>
              </DetailSection>
            ) : null}
            <DetailSection
              key={`diff-${selectedTask.id}-${changes.length > 0 ? 'has-diff' : 'empty-diff'}`}
              title={t('rd.diffWorkspace', 'Diff 审核')}
              extra={changes.length > 0 ? (
                <Space>
                  {hasAppliedChanges ? (
                    <Button
                      size="small"
                      icon={<RollbackOutlined />}
                      disabled={!canApply || rollbackMutation.isPending}
                      loading={rollbackMutation.isPending && !rollbackMutation.variables}
                      onClick={() => confirmRollback()}
                    >
                      {t('rd.rollbackAll', '回滚全部')}
                    </Button>
                  ) : null}
                  <Button
                    danger
                    size="small"
                    disabled={!canApply || !hasPendingChanges || applyMutation.isPending}
                    onClick={() => confirmApply()}
                  >
                    {t('rd.applyAll', '应用全部')}
                  </Button>
                </Space>
              ) : null}
              defaultOpen={changes.length > 0 && selectedTask.status === 'waiting_approval'}
            >
              {isDiffLoading ? (
                <div style={{ padding: '28px 0', textAlign: 'center' }}>
                  <Spin />
                  <div style={{ marginTop: 10 }}>
                    <Text type="secondary">{t('rd.diffLoading', '正在加载 Diff...')}</Text>
                  </div>
                </div>
              ) : changes.length === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.noChanges', '暂无 Diff')} /> : (
                <Space direction="vertical" style={{ width: '100%' }}>
                  {changes.map((change) => {
                    const notApplicableReason = rdFileChangeNotApplicableReason(change);
                    const applicable = isRdFileChangeApplicable(change);
                    return (
                      <Card
                        key={change.id}
                        size="small"
                        title={(
                          <Space>
                            {change.filePath}
                            {change.applied ? <Tag color="success"><CheckCircleOutlined /> {t('rd.applied', '已应用')}</Tag> : <Tag color="warning">{t('rd.pendingApply', '待应用')}</Tag>}
                            {notApplicableReason ? <Tag>{t('rd.notApplicable', '不可应用')}</Tag> : null}
                          </Space>
                        )}
                        extra={change.applied ? (
                          <Button
                            size="small"
                            icon={<RollbackOutlined />}
                            disabled={!canApply || rollbackMutation.isPending}
                            loading={rollbackMutation.isPending && rollbackMutation.variables?.length === 1 && rollbackMutation.variables[0] === change.id}
                            onClick={() => confirmRollback(change)}
                          >
                            {t('rd.rollbackPatch', '回滚修改')}
                          </Button>
                        ) : (
                          <Button size="small" disabled={!canApply || !applicable || applyMutation.isPending} onClick={() => confirmApply(change)}>{t('rd.applyPatch', '应用修改')}</Button>
                        )}
                      >
                        <DiffBlock change={change} />
                      </Card>
                    );
                  })}
                </Space>
              )}
            </DetailSection>
            <DetailSection
              key={`test-${selectedTask.id}`}
              title={<Space><ExperimentOutlined /> {t('rd.testPanel', '测试命令')}</Space>}
              extra={<Button icon={<PlayCircleOutlined />} disabled={!canRunCommand || !selectedTaskId || !canRunTestCommand || testMutation.isPending} loading={testMutation.isPending} onClick={confirmRunTest}>{t('rd.runTest', '运行测试')}</Button>}
            >
              <Space direction="vertical" style={{ width: '100%' }}>
                <Input value={testCommand} onChange={(event) => setTestCommand(event.target.value)} placeholder={t('rd.testCommandPlaceholder', '例如 npm test / cargo test --workspace')} />
                {tests.length === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.noTestRuns', '暂无测试记录')} /> : tests.map((test) => (
                  <Card key={test.id} size="small" style={{ background: '#0b1220' }}>
                    <Space direction="vertical" style={{ width: '100%' }}>
                      <Space>{renderStatusTag(test.status)}<Text style={{ color: '#94a3b8' }}>{test.command}</Text><Text style={{ color: '#94a3b8' }}>{test.durationMs ?? 0}ms</Text></Space>
                      <pre style={{ color: '#dbeafe', maxHeight: 180, overflow: 'auto', whiteSpace: 'pre-wrap' }}>{test.stdoutText || test.stderrText || t('rd.noTestOutput', '无输出')}</pre>
                    </Space>
                  </Card>
                ))}
              </Space>
            </DetailSection>
          </Space>
        )}
      </Drawer>
    </div>
  );
}
