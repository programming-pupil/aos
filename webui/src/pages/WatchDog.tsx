import { useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Card,
  Col,
  Descriptions,
  Empty,
  Input,
  List,
  Progress,
  Row,
  Space,
  Statistic,
  Table,
  Tag,
  Timeline,
  Typography,
  message,
} from 'antd';
import type { ColumnsType } from 'antd/es/table';
import { ExportOutlined, PauseCircleOutlined, ReloadOutlined, SendOutlined } from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { useNavigate, useSearchParams } from '@/router';
import { agentOpsApi, type AgentOpsTask } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { tasksApi } from '@/api/tasks';

const { Title, Text, Paragraph } = Typography;

const ACTIVE_STATUSES = new Set(['queued', 'claimed', 'running', 'waiting_input', 'retrying', 'cancelling']);
const RETRYABLE_RESOURCE_TYPES = new Set([
  'rd_task',
  'pm_research_task',
  'chat_adversarial_run',
  'nl2sql_agent_query',
]);

function statusColor(status: string) {
  switch (status) {
    case 'completed':
      return 'green';
    case 'failed':
    case 'timed_out':
      return 'red';
    case 'cancelled':
      return 'default';
    case 'blocked':
    case 'waiting_input':
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

function shortTime(value?: string | null) {
  if (!value) return '-';
  return new Date(value).toLocaleString();
}

function linkedResourcePath(task: AgentOpsTask): string | null {
  switch (task.linkedResourceType) {
    case 'rd_task':
      return '/agent';
    case 'chat_adversarial_run':
      return '/adversarial';
    case 'nl2sql_agent_query':
      return '/nl2sql';
    case 'pm_research_task':
      return '/operations/tasks';
    default:
      return null;
  }
}

function runtimeStatusColor(status?: string | null) {
  switch (status) {
    case 'running':
      return 'blue';
    case 'cancelling':
      return 'orange';
    case 'completed':
      return 'green';
    case 'failed':
    case 'stale':
      return 'red';
    case 'cancelled':
      return 'default';
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
    case 'cancelled':
      return 'default';
    default:
      return 'default';
  }
}

export default function WatchDog() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const [selectedTask, setSelectedTask] = useState<AgentOpsTask | null>(null);
  const [selectedArtifactId, setSelectedArtifactId] = useState<string | null>(null);
  const [question, setQuestion] = useState('');
  const [watchdogAnswer, setWatchdogAnswer] = useState('');
  const [watchdogAskTaskId, setWatchdogAskTaskId] = useState<string | null>(null);
  const [watchdogAskPending, setWatchdogAskPending] = useState(false);

  const summaryQuery = useQuery({
    queryKey: queryKeys.agentOps.summary(),
    queryFn: agentOpsApi.summary,
    refetchInterval: 10_000,
  });
  const agentsQuery = useQuery({
    queryKey: queryKeys.agentOps.agents(),
    queryFn: agentOpsApi.agents,
    refetchInterval: 10_000,
  });
  const tasksQuery = useQuery({
    queryKey: queryKeys.agentOps.tasks({ page: 1, per_page: 50 }),
    queryFn: () => agentOpsApi.tasks({ page: 1, per_page: 50 }),
    refetchInterval: 8_000,
  });
  const deadQueueQuery = useQuery({
    queryKey: queryKeys.agentOps.queue({ deadOnly: true, page: 1, per_page: 8 }),
    queryFn: () => agentOpsApi.queue({ deadOnly: true, page: 1, per_page: 8 }),
    refetchInterval: 10_000,
  });
  const staleQueueQuery = useQuery({
    queryKey: queryKeys.agentOps.queue({ staleOnly: true, leaseTimeoutSecs: 600, page: 1, per_page: 8 }),
    queryFn: () => agentOpsApi.queue({ staleOnly: true, leaseTimeoutSecs: 600, page: 1, per_page: 8 }),
    refetchInterval: 10_000,
  });
  const failedDeliveriesQuery = useQuery({
    queryKey: queryKeys.tasks.deliveries({ scope: 'tenant', status: 'failed', page: 1, perPage: 20 }),
    queryFn: () => tasksApi.deliveries({ scope: 'tenant', status: 'failed', page: 1, perPage: 20 }),
    refetchInterval: 10_000,
  });
  const traceQuery = useQuery({
    queryKey: selectedTask ? [...queryKeys.agentOps.taskEvents(selectedTask.id), 'trace'] : ['agentOps', 'trace', 'none'],
    queryFn: () => agentOpsApi.taskTrace(selectedTask!.id, { page: 1, per_page: 30 }),
    enabled: !!selectedTask,
    refetchInterval: 8_000,
  });
  const runtimeSessionId = selectedTask?.runtimeSession?.id;
  const runtimeProcessesQuery = useQuery({
    queryKey: runtimeSessionId ? queryKeys.agentOps.runtimeProcesses(runtimeSessionId) : ['agentOps', 'runtime', 'none', 'processes'],
    queryFn: () => agentOpsApi.runtimeProcesses(runtimeSessionId!, { page: 1, per_page: 5 }),
    enabled: !!runtimeSessionId,
    refetchInterval: 8_000,
  });
  const runtimeArtifactsQuery = useQuery({
    queryKey: runtimeSessionId ? queryKeys.agentOps.runtimeArtifacts(runtimeSessionId) : ['agentOps', 'runtime', 'none', 'artifacts'],
    queryFn: () => agentOpsApi.runtimeArtifacts(runtimeSessionId!, { page: 1, per_page: 5 }),
    enabled: !!runtimeSessionId,
    refetchInterval: 15_000,
  });
  const runtimeArtifactQuery = useQuery({
    queryKey: runtimeSessionId && selectedArtifactId
      ? queryKeys.agentOps.runtimeArtifact(runtimeSessionId, selectedArtifactId)
      : ['agentOps', 'runtime', 'none', 'artifact'],
    queryFn: () => agentOpsApi.runtimeArtifact(runtimeSessionId!, selectedArtifactId!),
    enabled: !!runtimeSessionId && !!selectedArtifactId,
  });
  const askTaskQuery = useQuery({
    queryKey: watchdogAskTaskId ? queryKeys.agentOps.task(watchdogAskTaskId) : ['agentOps', 'watchdogAsk', 'none'],
    queryFn: () => agentOpsApi.task(watchdogAskTaskId!),
    enabled: !!watchdogAskTaskId,
    refetchInterval: watchdogAskPending ? 1500 : false,
  });

  const askMutation = useMutation({
    mutationFn: (value: string) => agentOpsApi.ask({ question: value, scope: 'tenant', asyncMode: false }),
    onSuccess: (res) => {
      if (res.taskId) {
        setWatchdogAskTaskId(res.taskId);
        setWatchdogAskPending(true);
        setWatchdogAnswer(t('watchdog.askQueued'));
        qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
        return;
      }
      setWatchdogAskPending(false);
      setWatchdogAnswer(res.answer ?? '');
    },
    onError: (error: Error) => message.error(error.message || t('watchdog.askFailed')),
  });
  const cancelMutation = useMutation({
    mutationFn: (id: string) => agentOpsApi.cancelTask(id),
    onSuccess: () => {
      message.success(t('watchdog.cancelRequested'));
      qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    },
    onError: (error: Error) => message.error(error.message || t('common.operateFailed')),
  });
  const retryMutation = useMutation({
    mutationFn: (id: string) => agentOpsApi.retryTask(id),
    onSuccess: () => {
      message.success(t('watchdog.retryRequested'));
      qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    },
    onError: (error: Error) => message.error(error.message || t('common.operateFailed')),
  });
  const recoverQueueMutation = useMutation({
    mutationFn: () => agentOpsApi.recoverQueue({ leaseTimeoutSecs: 600 }),
    onSuccess: (res) => {
      message.success(t('watchdog.queue.recovered', { dead: res.dead, recovered: res.recovered }));
      qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    },
    onError: (error: Error) => message.error(error.message || t('common.operateFailed')),
  });
  const recoverRuntimeMutation = useMutation({
    mutationFn: () => agentOpsApi.recoverRuntime({ timeout_secs: 900 }),
    onSuccess: (res) => {
      message.success(t('watchdog.runtime.recovered', { recovered: res.recovered }));
      qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    },
    onError: (error: Error) => message.error(error.message || t('common.operateFailed')),
  });
  const replayDeliveryMutation = useMutation({
    mutationFn: (id: string) => tasksApi.replayDelivery(id),
    onSuccess: () => {
      message.success(t('tasks.settings.deliveryReplayQueued'));
      qc.invalidateQueries({ queryKey: queryKeys.tasks.all });
    },
    onError: (error: Error) => message.error(error.message || t('common.operateFailed')),
  });

  const tasks = tasksQuery.data?.items ?? [];
  const activeTasks = useMemo(() => tasks.filter((item) => ACTIVE_STATUSES.has(item.status)), [tasks]);
  const failedTasks = useMemo(() => tasks.filter((item) => item.status === 'failed'), [tasks]);
  const selectedResourcePath = selectedTask ? linkedResourcePath(selectedTask) : null;
  const selectedTaskRetryable = selectedTask?.linkedResourceType
    ? RETRYABLE_RESOURCE_TYPES.has(selectedTask.linkedResourceType)
    : false;

  useEffect(() => {
    const taskId = searchParams.get('task');
    if (!taskId || selectedTask?.id === taskId) return;
    const matched = tasks.find((item) => item.id === taskId);
    if (matched) setSelectedTask(matched);
  }, [searchParams, selectedTask?.id, tasks]);

  useEffect(() => {
    setSelectedArtifactId(null);
  }, [selectedTask?.id]);

  useEffect(() => {
    const task = askTaskQuery.data;
    if (!task) return;
    if (task.status === 'completed') {
      const answer = typeof task.outputJson?.answer === 'string' ? task.outputJson.answer : '';
      setWatchdogAnswer(answer || t('watchdog.askCompleted'));
      setWatchdogAskPending(false);
      qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    } else if (task.status === 'failed') {
      setWatchdogAnswer(task.errorMessage || t('watchdog.askFailed'));
      setWatchdogAskPending(false);
      qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    } else if (task.status === 'cancelled') {
      setWatchdogAnswer(t('watchdog.askCancelled'));
      setWatchdogAskPending(false);
      qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    } else if (watchdogAskPending) {
      setWatchdogAnswer(task.lastEvent || t('watchdog.askRunning'));
    }
  }, [askTaskQuery.data, qc, t, watchdogAskPending]);

  const loadErrors = [
    summaryQuery.error ? `summary: ${(summaryQuery.error as Error).message}` : null,
    agentsQuery.error ? `agents: ${(agentsQuery.error as Error).message}` : null,
    tasksQuery.error ? `tasks: ${(tasksQuery.error as Error).message}` : null,
    deadQueueQuery.error ? `queue/dead: ${(deadQueueQuery.error as Error).message}` : null,
    staleQueueQuery.error ? `queue/stale: ${(staleQueueQuery.error as Error).message}` : null,
    failedDeliveriesQuery.error ? `deliveries/failed: ${(failedDeliveriesQuery.error as Error).message}` : null,
    traceQuery.error ? `trace: ${(traceQuery.error as Error).message}` : null,
    runtimeProcessesQuery.error ? `runtime processes: ${(runtimeProcessesQuery.error as Error).message}` : null,
    runtimeArtifactsQuery.error ? `runtime artifacts: ${(runtimeArtifactsQuery.error as Error).message}` : null,
    runtimeArtifactQuery.error ? `runtime artifact: ${(runtimeArtifactQuery.error as Error).message}` : null,
  ].filter(Boolean);

  const columns: ColumnsType<AgentOpsTask> = [
    {
      title: t('watchdog.columns.capability'),
      dataIndex: 'capabilityKey',
      width: 140,
      render: (value: string) => <Tag>{capabilityName(value, t)}</Tag>,
    },
    {
      title: t('watchdog.columns.task'),
      dataIndex: 'title',
      width: 260,
      render: (value: string, row) => (
        <Space direction="vertical" size={0} style={{ width: '100%' }}>
          <Paragraph
            strong
            ellipsis={{ rows: 2, tooltip: value }}
            style={{ margin: 0, maxWidth: 240, overflowWrap: 'anywhere' }}
          >
            {value}
          </Paragraph>
          <Text type="secondary" ellipsis style={{ fontSize: 12, maxWidth: 240 }}>
            {row.agentName || row.sourceLabel || row.source}
          </Text>
        </Space>
      ),
    },
    {
      title: t('common.status'),
      dataIndex: 'status',
      width: 110,
      render: (value: string) => <Tag color={statusColor(value)}>{value}</Tag>,
    },
    {
      title: t('watchdog.columns.phase'),
      dataIndex: 'phase',
      width: 150,
      render: (value: string, row) => (
        <Space direction="vertical" size={2} style={{ width: 130 }}>
          <Text>{value}</Text>
          <Progress percent={row.progressPercent} size="small" showInfo={false} />
        </Space>
      ),
    },
    {
      title: t('watchdog.columns.lastEvent'),
      dataIndex: 'lastEvent',
      width: 260,
      render: (value?: string | null) => value ? (
        <Paragraph
          ellipsis={{ rows: 2, tooltip: value }}
          style={{ margin: 0, maxWidth: 240, overflowWrap: 'anywhere' }}
        >
          {value}
        </Paragraph>
      ) : '-',
    },
    {
      title: t('watchdog.columns.queue'),
      dataIndex: 'queue',
      width: 170,
      render: (_: unknown, row) => (
        <Space direction="vertical" size={2}>
          <Tag color={queueStatusColor(row.queue?.status)}>{row.queue?.status || '-'}</Tag>
          <Text type="secondary" style={{ fontSize: 12 }}>
            {t('watchdog.queue.attempts', {
              current: row.queue?.attemptCount ?? 0,
              max: row.queue?.maxAttempts ?? 0,
            })}
          </Text>
        </Space>
      ),
    },
    {
      title: t('watchdog.columns.runtime'),
      dataIndex: 'runtimeSession',
      width: 260,
      render: (_: unknown, row) => row.runtimeSession ? (
        <Space direction="vertical" size={2} style={{ width: 240 }}>
          <Space size={4} wrap>
            <Tag color={runtimeStatusColor(row.runtimeSession.status)}>{row.runtimeSession.status}</Tag>
            {row.runtimeSession.currentProcessStatus && (
              <Tag>{row.runtimeSession.currentProcessStatus}</Tag>
            )}
          </Space>
          {row.runtimeSession.currentCommand ? (
            <Paragraph
              code
              ellipsis={{ rows: 2, tooltip: row.runtimeSession.currentCommand }}
              style={{ margin: 0, maxWidth: 240, overflowWrap: 'anywhere' }}
            >
              {row.runtimeSession.currentCommand}
            </Paragraph>
          ) : (
            <Text type="secondary" style={{ fontSize: 12 }}>{t('watchdog.runtime.noCommand')}</Text>
          )}
        </Space>
      ) : <Text type="secondary">-</Text>,
    },
    {
      title: t('common.updatedAt'),
      dataIndex: 'updatedAt',
      width: 170,
      render: shortTime,
    },
  ];

  return (
    <div style={{ padding: 20, height: '100%', overflow: 'auto', background: 'var(--bg-void)' }}>
      <Space direction="vertical" size={16} style={{ width: '100%' }}>
        <Space align="center" style={{ justifyContent: 'space-between', width: '100%' }}>
          <div>
            <Title level={3} style={{ margin: 0 }}>{t('watchdog.title')}</Title>
            <Text type="secondary">{t('watchdog.subtitle')}</Text>
          </div>
          <Space wrap>
            <Button icon={<ReloadOutlined />} onClick={() => qc.invalidateQueries({ queryKey: queryKeys.agentOps.all })}>
              {t('common.refresh')}
            </Button>
            <Button
              loading={recoverQueueMutation.isPending}
              onClick={() => recoverQueueMutation.mutate()}
            >
              {t('watchdog.queue.recover')}
            </Button>
            <Button
              loading={recoverRuntimeMutation.isPending}
              onClick={() => recoverRuntimeMutation.mutate()}
            >
              {t('watchdog.runtime.recover')}
            </Button>
          </Space>
        </Space>

        {loadErrors.length > 0 && (
          <Alert
            type="error"
            showIcon
            message={t('watchdog.loadFailed')}
            description={loadErrors.join('\n')}
          />
        )}

        <Row gutter={[12, 12]}>
          <Col xs={12} md={6}><Card><Statistic title={t('watchdog.stats.running')} value={summaryQuery.data?.running ?? activeTasks.length} /></Card></Col>
          <Col xs={12} md={6}><Card><Statistic title={t('watchdog.stats.stale')} value={summaryQuery.data?.stale ?? 0} /></Card></Col>
          <Col xs={12} md={6}><Card><Statistic title={t('watchdog.stats.failed')} value={failedTasks.length} /></Card></Col>
          <Col xs={12} md={6}><Card><Statistic title={t('watchdog.stats.recent')} value={tasks.length} /></Card></Col>
        </Row>

        <Card title={t('watchdog.queue.health')}>
          <Row gutter={[12, 12]}>
            <Col xs={24} md={12}>
              <Space direction="vertical" size={8} style={{ width: '100%' }}>
                <Space style={{ justifyContent: 'space-between', width: '100%' }}>
                  <Text strong>{t('watchdog.queue.deadTasks')}</Text>
                  <Tag color={deadQueueQuery.data?.total ? 'red' : 'green'}>{deadQueueQuery.data?.total ?? 0}</Tag>
                </Space>
                <List
                  size="small"
                  loading={deadQueueQuery.isLoading}
                  dataSource={deadQueueQuery.data?.items ?? []}
                  locale={{ emptyText: t('watchdog.queue.noDeadTasks') }}
                  renderItem={(item) => (
                    <List.Item onClick={() => setSelectedTask(item)} style={{ cursor: 'pointer', paddingInline: 0 }}>
                      <Space direction="vertical" size={2} style={{ width: '100%' }}>
                        <Paragraph ellipsis={{ rows: 1, tooltip: item.title }} style={{ margin: 0, overflowWrap: 'anywhere' }}>
                          {item.title}
                        </Paragraph>
                        <Text type="danger" ellipsis style={{ maxWidth: '100%' }}>
                          {item.queue.deadReason || item.queue.lastError || item.errorMessage || item.lastEvent || '-'}
                        </Text>
                      </Space>
                    </List.Item>
                  )}
                />
              </Space>
            </Col>
            <Col xs={24} md={12}>
              <Space direction="vertical" size={8} style={{ width: '100%' }}>
                <Space style={{ justifyContent: 'space-between', width: '100%' }}>
                  <Text strong>{t('watchdog.queue.staleLeases')}</Text>
                  <Tag color={staleQueueQuery.data?.total ? 'orange' : 'green'}>{staleQueueQuery.data?.total ?? 0}</Tag>
                </Space>
                <List
                  size="small"
                  loading={staleQueueQuery.isLoading}
                  dataSource={staleQueueQuery.data?.items ?? []}
                  locale={{ emptyText: t('watchdog.queue.noStaleLeases') }}
                  renderItem={(item) => (
                    <List.Item onClick={() => setSelectedTask(item)} style={{ cursor: 'pointer', paddingInline: 0 }}>
                      <Space direction="vertical" size={2} style={{ width: '100%' }}>
                        <Paragraph ellipsis={{ rows: 1, tooltip: item.title }} style={{ margin: 0, overflowWrap: 'anywhere' }}>
                          {item.title}
                        </Paragraph>
                        <Text type="secondary" ellipsis style={{ maxWidth: '100%' }}>
                          {item.queue.status} · {item.queue.claimedBy || '-'} · {shortTime(item.queue.leaseExpiresAt)}
                        </Text>
                      </Space>
                    </List.Item>
                  )}
                />
              </Space>
            </Col>
          </Row>
        </Card>

        <Card title={t('tasks.settings.deliveryHealth')}>
          <List
            size="small"
            loading={failedDeliveriesQuery.isLoading}
            dataSource={failedDeliveriesQuery.data?.items ?? []}
            locale={{ emptyText: t('tasks.settings.noFailedDeliveries') }}
            renderItem={(delivery) => (
              <List.Item
                actions={[
                  <Button
                    key="replay"
                    icon={<SendOutlined />}
                    loading={replayDeliveryMutation.isPending && replayDeliveryMutation.variables === delivery.id}
                    onClick={() => replayDeliveryMutation.mutate(delivery.id)}
                  >
                    {t('tasks.settings.replayDelivery')}
                  </Button>,
                ]}
              >
                <Space direction="vertical" size={2} style={{ width: '100%' }}>
                  <Space wrap>
                    <Tag color="error">{delivery.platform}</Tag>
                    <Text strong>{delivery.title}</Text>
                    <Text code>#{delivery.shortCode ?? delivery.taskId}</Text>
                  </Space>
                  <Text type="danger">{delivery.lastError ?? t('common.error')}</Text>
                  <Text type="secondary">
                    {delivery.attemptCount}/{delivery.maxAttempts} · {shortTime(delivery.updatedAt)}
                  </Text>
                </Space>
              </List.Item>
            )}
          />
        </Card>

        <Row gutter={[12, 12]} align="stretch">
          <Col xs={24} lg={6}>
            <Card title={t('watchdog.agentHealth')} style={{ height: '100%' }}>
              <List
                dataSource={agentsQuery.data?.items ?? []}
                locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('watchdog.emptyAgents')} /> }}
                renderItem={(item) => (
                  <List.Item>
                    <Space direction="vertical" size={2} style={{ width: '100%' }}>
                      <Space style={{ justifyContent: 'space-between', width: '100%' }}>
                        <Text strong>{capabilityName(item.capabilityKey, t)}</Text>
                        <Tag color={item.active ? 'blue' : 'default'}>{t('watchdog.activeShort', { count: item.active })}</Tag>
                      </Space>
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('watchdog.agentCounts', { total: item.total, failed: item.failed24h })}
                      </Text>
                    </Space>
                  </List.Item>
                )}
              />
            </Card>
          </Col>
          <Col xs={24} lg={12}>
            <Card title={t('watchdog.liveTaskBoard')} bodyStyle={{ padding: 0 }}>
              <Table
                rowKey="id"
                size="small"
                columns={columns}
                dataSource={tasks}
                loading={tasksQuery.isLoading}
                pagination={{ pageSize: 12 }}
                scroll={{ x: 'max-content' }}
                onRow={(record) => ({ onClick: () => setSelectedTask(record) })}
              />
            </Card>
          </Col>
          <Col xs={24} lg={6}>
            <Card
              title={t('watchdog.taskInspector')}
              extra={selectedTask ? <Tag color={statusColor(selectedTask.status)}>{selectedTask.status}</Tag> : null}
              style={{ height: '100%' }}
            >
              {!selectedTask ? (
                <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('watchdog.selectTask')} />
              ) : (
                <Space direction="vertical" size={12} style={{ width: '100%' }}>
                  <Descriptions column={1} size="small">
                    <Descriptions.Item label={t('watchdog.fields.task')}>{selectedTask.title}</Descriptions.Item>
                    <Descriptions.Item label={t('watchdog.fields.capability')}>{capabilityName(selectedTask.capabilityKey, t)}</Descriptions.Item>
                    <Descriptions.Item label={t('watchdog.fields.phase')}>{selectedTask.phase}</Descriptions.Item>
                    <Descriptions.Item label={t('common.source')}>{selectedTask.source}</Descriptions.Item>
                    <Descriptions.Item label={t('watchdog.fields.resource')}>{selectedTask.linkedResourceType || '-'} {selectedTask.linkedResourceId || ''}</Descriptions.Item>
                    <Descriptions.Item label={t('watchdog.fields.queue')}>
                      <Space direction="vertical" size={2} style={{ maxWidth: '100%' }}>
                        <Space size={4} wrap>
                          <Tag color={queueStatusColor(selectedTask.queue?.status)}>
                            {selectedTask.queue?.status || '-'}
                          </Tag>
                          <Text type="secondary">
                            {t('watchdog.queue.attempts', {
                              current: selectedTask.queue?.attemptCount ?? 0,
                              max: selectedTask.queue?.maxAttempts ?? 0,
                            })}
                          </Text>
                        </Space>
                        {selectedTask.queue?.claimedBy && (
                          <Text ellipsis style={{ maxWidth: 260 }}>
                            {t('watchdog.queue.claimedBy')}: {selectedTask.queue.claimedBy}
                          </Text>
                        )}
                        {selectedTask.queue?.leaseExpiresAt && (
                          <Text type="secondary" style={{ fontSize: 12 }}>
                            {t('watchdog.queue.leaseExpiresAt')}: {shortTime(selectedTask.queue.leaseExpiresAt)}
                          </Text>
                        )}
                        {(selectedTask.queue?.deadReason || selectedTask.queue?.lastError) && (
                          <Paragraph
                            type="danger"
                            ellipsis={{ rows: 3, tooltip: selectedTask.queue.deadReason || selectedTask.queue.lastError || '' }}
                            style={{ margin: 0, maxWidth: 260, overflowWrap: 'anywhere' }}
                          >
                            {selectedTask.queue.deadReason || selectedTask.queue.lastError}
                          </Paragraph>
                        )}
                      </Space>
                    </Descriptions.Item>
                    <Descriptions.Item label={t('watchdog.fields.runtime')}>
                      {selectedTask.runtimeSession ? (
                        <Space direction="vertical" size={2} style={{ maxWidth: '100%' }}>
                          <Space size={4} wrap>
                            <Tag color={runtimeStatusColor(selectedTask.runtimeSession.status)}>
                              {selectedTask.runtimeSession.status}
                            </Tag>
                            <Text type="secondary">{selectedTask.runtimeSession.isolationMode}</Text>
                          </Space>
                          <Text copyable ellipsis style={{ maxWidth: 260 }}>
                            {selectedTask.runtimeSession.id}
                          </Text>
                          {selectedTask.runtimeSession.currentCommand && (
                            <Paragraph
                              code
                              ellipsis={{ rows: 3, tooltip: selectedTask.runtimeSession.currentCommand }}
                              style={{ margin: 0, maxWidth: 260, overflowWrap: 'anywhere' }}
                            >
                              {selectedTask.runtimeSession.currentCommand}
                            </Paragraph>
                          )}
                          <Text type="secondary" style={{ fontSize: 12 }}>
                            {t('watchdog.runtime.heartbeat')}: {shortTime(selectedTask.runtimeSession.heartbeatAt)}
                          </Text>
                        </Space>
                      ) : '-'}
                    </Descriptions.Item>
                  </Descriptions>
                  {selectedTask.errorMessage && <Alert type="error" showIcon message={selectedTask.errorMessage} />}
                  {selectedTask.runtimeSession && (
                    <Space direction="vertical" size={8} style={{ width: '100%' }}>
                      <Text strong>{t('watchdog.runtime.processes')}</Text>
                      <List
                        size="small"
                        loading={runtimeProcessesQuery.isLoading}
                        dataSource={runtimeProcessesQuery.data?.items ?? []}
                        locale={{ emptyText: t('watchdog.runtime.noProcesses') }}
                        renderItem={(item) => (
                          <List.Item style={{ paddingInline: 0 }}>
                            <Space direction="vertical" size={2} style={{ width: '100%' }}>
                              <Space size={4} wrap>
                                <Tag color={runtimeStatusColor(item.status)}>{item.status}</Tag>
                                {typeof item.exitCode === 'number' && <Tag>exit {item.exitCode}</Tag>}
                              </Space>
                              <Paragraph
                                code
                                ellipsis={{ rows: 2, tooltip: item.command }}
                                style={{ margin: 0, maxWidth: 260, overflowWrap: 'anywhere' }}
                              >
                                {item.command}
                              </Paragraph>
                              {(item.stderrPreview || item.stdoutPreview) && (
                                <Paragraph
                                  ellipsis={{ rows: 2, tooltip: item.stderrPreview || item.stdoutPreview || '' }}
                                  style={{ margin: 0, maxWidth: 260, overflowWrap: 'anywhere', fontSize: 12 }}
                                  type={item.stderrPreview ? 'danger' : 'secondary'}
                                >
                                  {item.stderrPreview || item.stdoutPreview}
                                </Paragraph>
                              )}
                            </Space>
                          </List.Item>
                        )}
                      />
                      <Text strong>{t('watchdog.runtime.artifacts')}</Text>
                      <List
                        size="small"
                        loading={runtimeArtifactsQuery.isLoading}
                        dataSource={runtimeArtifactsQuery.data?.items ?? []}
                        locale={{ emptyText: t('watchdog.runtime.noArtifacts') }}
                        renderItem={(item) => (
                          <List.Item style={{ paddingInline: 0 }}>
                            <Space direction="vertical" size={2} style={{ width: '100%' }}>
                              <Space size={4} wrap>
                                <Tag>{item.artifactType}</Tag>
                                <Text type="secondary" style={{ fontSize: 12 }}>{item.sizeBytes} B</Text>
                                <Button
                                  type="link"
                                  size="small"
                                  style={{ paddingInline: 0 }}
                                  onClick={() => setSelectedArtifactId(item.id)}
                                >
                                  {t('watchdog.runtime.viewArtifact')}
                                </Button>
                              </Space>
                              <Text copyable ellipsis style={{ maxWidth: 260 }}>
                                {item.path || item.id}
                              </Text>
                            </Space>
                          </List.Item>
                        )}
                      />
                      {selectedArtifactId && (
                        <Card
                          size="small"
                          loading={runtimeArtifactQuery.isLoading}
                          title={runtimeArtifactQuery.data?.path || runtimeArtifactQuery.data?.id || t('watchdog.runtime.artifactDetail')}
                          styles={{ body: { padding: 8 } }}
                        >
                          <Space direction="vertical" size={4} style={{ width: '100%' }}>
                            {runtimeArtifactQuery.data?.contentTruncated && (
                              <Alert type="warning" showIcon message={t('watchdog.runtime.artifactTruncated')} />
                            )}
                            <pre
                              style={{
                                maxHeight: 240,
                                overflow: 'auto',
                                margin: 0,
                                padding: 8,
                                background: '#111827',
                                color: '#e5e7eb',
                                borderRadius: 4,
                                whiteSpace: 'pre-wrap',
                                overflowWrap: 'anywhere',
                                fontSize: 12,
                              }}
                            >
                              {runtimeArtifactQuery.data?.content || runtimeArtifactQuery.data?.contentText || t('watchdog.runtime.noArtifactContent')}
                            </pre>
                          </Space>
                        </Card>
                      )}
                    </Space>
                  )}
                  <Space>
                    {selectedResourcePath && (
                      <Button icon={<ExportOutlined />} onClick={() => navigate(selectedResourcePath)}>
                        {t('watchdog.openResource')}
                      </Button>
                    )}
                    <Button
                      icon={<PauseCircleOutlined />}
                      disabled={!ACTIVE_STATUSES.has(selectedTask.status)}
                      loading={cancelMutation.isPending}
                      onClick={() => cancelMutation.mutate(selectedTask.id)}
                    >
                      {t('common.cancel')}
                    </Button>
                    <Button
                      loading={retryMutation.isPending}
                      disabled={!selectedTaskRetryable}
                      title={selectedTaskRetryable ? undefined : t('watchdog.retryUnsupported')}
                      onClick={() => retryMutation.mutate(selectedTask.id)}
                    >
                      {t('common.retry')}
                    </Button>
                  </Space>
                  <Timeline
                    items={(traceQuery.data?.items ?? []).map((event) => ({
                      color: event.severity === 'error' ? 'red' : event.severity === 'warn' ? 'orange' : 'blue',
                      children: (
                        <Space direction="vertical" size={0}>
                          <Text>{event.message}</Text>
                          <Text type="secondary" style={{ fontSize: 12 }}>
                            {event.eventType} · {shortTime(event.createdAt)}
                            {event.durationMs ? ` · ${event.durationMs}ms` : ''}
                            {event.tokenInput || event.tokenOutput ? ` · ${event.tokenInput ?? 0}/${event.tokenOutput ?? 0} tokens` : ''}
                          </Text>
                        </Space>
                      ),
                    }))}
                  />
                </Space>
              )}
            </Card>
          </Col>
        </Row>

        <Card title={t('watchdog.askTitle')}>
          <Space.Compact style={{ width: '100%' }}>
            <Input
              value={question}
              onChange={(event) => setQuestion(event.target.value)}
              onPressEnter={() => question.trim() && askMutation.mutate(question.trim())}
              placeholder={t('watchdog.askPlaceholder')}
            />
            <Button
              type="primary"
              icon={<SendOutlined />}
              loading={askMutation.isPending}
              onClick={() => question.trim() && askMutation.mutate(question.trim())}
            >
              {t('watchdog.askButton')}
            </Button>
          </Space.Compact>
          {watchdogAskPending && (
            <Text type="secondary" style={{ display: 'block', marginTop: 8 }}>
              {askTaskQuery.data?.lastEvent || t('watchdog.askRunning')}
            </Text>
          )}
          {watchdogAnswer && (
            <Paragraph style={{ marginTop: 12, whiteSpace: 'pre-wrap' }}>{watchdogAnswer}</Paragraph>
          )}
        </Card>
      </Space>
    </div>
  );
}
