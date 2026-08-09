import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Alert,
  Button,
  Card,
  Col,
  Drawer,
  Form,
  Input,
  Modal,
  Popconfirm,
  Row,
  Select,
  Space,
  Statistic,
  Switch,
  Table,
  Tag,
  Typography,
  message,
} from 'antd';
import {
  CloseCircleOutlined,
  ClockCircleOutlined,
  DeleteOutlined,
  DownloadOutlined,
  EditOutlined,
  FileSearchOutlined,
  PlayCircleOutlined,
  PlusOutlined,
  ShareAltOutlined,
} from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import { agentApi, pmApi } from '@/api';
import { Markdown } from '@/components/chat';

type Mission = {
  id: number;
  missionName: string;
  intent: string;
  countryCode?: string;
  scheduleCron?: string | null;
  lookbackDays?: number;
  maxSources?: number;
  maxSignalsPerSource?: number;
  autoDiscovery?: boolean;
  enabled: boolean;
  updatedAt: string;
};

type MissionTaskRun = {
  taskId: string;
  status: string;
  stage?: string | null;
  attempt?: number | null;
  elapsedMs: number;
  stageElapsedMs?: number | null;
  errorMessage?: string | null;
  detail?: Record<string, unknown> | null;
  response?: unknown | null;
  createdAt: string;
  updatedAt: string;
  completedAt?: string | null;
};

const PM_SHARE_PREVIEW_MAX_CHARS = 18000;

type PmSharePreviewPayload = {
  schema: 'aos-pm-share-v1';
  title: string;
  generatedAt: string;
  messageId: string;
  taskId?: string | null;
  content: string;
  truncated?: boolean;
};

function fmtTime(
  value?: string | null,
  locale?: string,
  options?: { timeZone?: 'UTC' | 'Asia/Shanghai' },
): string {
  if (!value) return '-';
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return value;
  return d.toLocaleString(locale || undefined, {
    hour12: false,
    timeZone: options?.timeZone,
  });
}

function statusTag(
  status: string,
  t: any,
) {
  const s = (status ?? '').toLowerCase();
  if (s === 'completed') {
    return <Tag color="green">{t('operations.statusCompleted', '已完成')}</Tag>;
  }
  if (s === 'failed') return <Tag color="red">{t('operations.statusFailed', '失败')}</Tag>;
  if (s === 'cancelled') {
    return <Tag color="default">{t('operations.statusCancelled', '已取消')}</Tag>;
  }
  if (s === 'running') return <Tag color="blue">{t('operations.statusRunning', '运行中')}</Tag>;
  if (s === 'queued') return <Tag color="gold">{t('operations.statusQueued', '排队中')}</Tag>;
  if (s === 'cancelling') {
    return <Tag color="gold">{t('operations.statusCancelling', '取消中')}</Tag>;
  }
  return <Tag>{status || '-'}</Tag>;
}

function percentText(value?: number | null): string {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-';
  return `${Math.round(value * 1000) / 10}%`;
}

function formatDurationMs(value?: number | null): string {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-';
  if (value < 1000) return `${value}ms`;
  if (value < 60_000) return `${(value / 1000).toFixed(1)}s`;
  return `${(value / 60_000).toFixed(1)}m`;
}

function isRunCancellable(status?: string): boolean {
  const normalized = (status || '').toLowerCase();
  return normalized === 'queued' || normalized === 'running' || normalized === 'cancelling';
}

function isRunResumable(status?: string): boolean {
  const normalized = (status || '').toLowerCase();
  return normalized === 'failed' || normalized === 'cancelled';
}

function encodeUtf8ToBase64Url(raw: string): string {
  const bytes = new TextEncoder().encode(raw);
  let binary = '';
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/g, '');
}

function buildPmSharePreviewUrl(payload: PmSharePreviewPayload): string {
  if (typeof window === 'undefined') return '';
  const encoded = encodeURIComponent(
    encodeUtf8ToBase64Url(JSON.stringify(payload)),
  );
  const next = new URL(window.location.href);
  next.pathname = '/preview/share';
  next.search = `?d=${encoded}`;
  next.hash = '';
  return next.toString();
}

function pickFirstNonEmptyString(value: unknown): string {
  if (typeof value === 'string') return value.trim();
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return '';
}

function findPreferredText(value: unknown, depth = 0): string {
  if (depth > 6 || value == null) return '';
  const scalar = pickFirstNonEmptyString(value);
  if (scalar) return scalar;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findPreferredText(item, depth + 1);
      if (found) return found;
    }
    return '';
  }
  if (typeof value !== 'object') return '';
  const obj = value as Record<string, unknown>;
  const preferredKeys = [
    'text',
    'answer',
    'content',
    'reply',
    'result',
    'output',
    'finalAnswer',
    'final_answer',
    'markdown',
    'md',
  ];
  for (const key of preferredKeys) {
    const found = pickFirstNonEmptyString(obj[key]);
    if (found) return found;
  }
  const preferredContainers = [
    'response',
    'data',
    'result',
    'output',
    'payload',
    'pm_report',
    'report',
  ];
  for (const key of preferredContainers) {
    const found = findPreferredText(obj[key], depth + 1);
    if (found) return found;
  }
  for (const nested of Object.values(obj)) {
    const found = findPreferredText(nested, depth + 1);
    if (found) return found;
  }
  return '';
}

function extractReplyMarkdown(response: unknown): string {
  const text = findPreferredText(response);
  if (text) return text;
  if (response == null) return '';
  try {
    return JSON.stringify(response, null, 2);
  } catch {
    return String(response);
  }
}

export default function OperationsTasks() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const locale = i18n.resolvedLanguage || i18n.language || undefined;

  const [missionOpen, setMissionOpen] = useState(false);
  const [editingMission, setEditingMission] = useState<Mission | null>(null);
  const [missionPage, setMissionPage] = useState(1);
  const [missionPageSize, setMissionPageSize] = useState(20);
  const [missionForm] = Form.useForm();

  const [cronPreviewLoading, setCronPreviewLoading] = useState(false);
  const [cronPreviewError, setCronPreviewError] = useState<string | null>(null);
  const [cronPreviewRuns, setCronPreviewRuns] = useState<string[]>([]);

  const [runDrawerOpen, setRunDrawerOpen] = useState(false);
  const [selectedMission, setSelectedMission] = useState<Mission | null>(null);
  const [runPage, setRunPage] = useState(1);
  const [runPageSize, setRunPageSize] = useState(20);
  const [runStatusFilter, setRunStatusFilter] = useState<string | undefined>(undefined);

  const [replyModalOpen, setReplyModalOpen] = useState(false);
  const [selectedReplyRun, setSelectedReplyRun] = useState<MissionTaskRun | null>(null);

  const missionSummaryQ = useQuery({
    queryKey: ['pm', 'missions', 'summary'],
    queryFn: () => pmApi.getMissionSummary(),
    refetchInterval: (query) => {
      const data = query.state.data;
      return data && (data.queuedRuns > 0 || data.runningRuns > 0 || data.cancellingRuns > 0)
        ? 2500
        : false;
    },
  });

  const missionsQ = useQuery({
    queryKey: ['pm', 'missions', missionPage, missionPageSize],
    queryFn: () =>
      pmApi.listMissions({
        page: missionPage,
        per_page: missionPageSize,
      }),
  });

  const missionRunsQ = useQuery({
    queryKey: ['pm', 'mission-runs', selectedMission?.id, runPage, runPageSize, runStatusFilter ?? 'all'],
    enabled: runDrawerOpen && !!selectedMission,
    queryFn: () =>
      pmApi.listMissionTaskRuns(selectedMission!.id, {
        page: runPage,
        per_page: runPageSize,
        status: runStatusFilter,
      }),
    refetchInterval: (query) => (
      query.state.data?.items?.some((run) => isRunCancellable(run.status)) ? 2500 : false
    ),
  });

  const missions: Mission[] = missionsQ.data?.items ?? [];
  const runs: MissionTaskRun[] = missionRunsQ.data?.items ?? [];
  const missionLoadError = missionSummaryQ.error || missionsQ.error;
  const replyMarkdown = useMemo(
    () => extractReplyMarkdown(selectedReplyRun?.response),
    [selectedReplyRun],
  );
  const replyHasContent = replyMarkdown.trim().length > 0;

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['pm', 'missions', 'summary'] });
    qc.invalidateQueries({ queryKey: ['pm', 'missions'] });
  };

  const retryMissionLoad = () => {
    void Promise.all([missionSummaryQ.refetch(), missionsQ.refetch()]);
  };

  const refreshRuns = () => {
    if (!selectedMission) return;
    qc.invalidateQueries({ queryKey: ['pm', 'missions', 'summary'] });
    qc.invalidateQueries({ queryKey: ['pm', 'mission-runs', selectedMission.id] });
  };

  const resetCronPreview = () => {
    setCronPreviewError(null);
    setCronPreviewRuns([]);
  };

  const createMissionMut = useMutation({
    mutationFn: (payload: Record<string, unknown>) => pmApi.createMission(payload as never),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      setMissionOpen(false);
      setEditingMission(null);
      missionForm.resetFields();
      resetCronPreview();
      refresh();
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
  });

  const updateMissionMut = useMutation({
    mutationFn: ({ id, payload }: { id: number; payload: Record<string, unknown> }) =>
      pmApi.updateMission(id, payload as never),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      setMissionOpen(false);
      setEditingMission(null);
      missionForm.resetFields();
      resetCronPreview();
      refresh();
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
  });

  const deleteMissionMut = useMutation({
    mutationFn: (id: number) => pmApi.deleteMission(id),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      refresh();
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
  });

  const runMissionNowMut = useMutation({
    mutationFn: (id: number) => pmApi.runMissionNow(id),
    onSuccess: (_data, missionId) => {
      message.success(t('common.operateSuccess'));
      qc.invalidateQueries({ queryKey: ['pm', 'missions', 'summary'] });
      qc.invalidateQueries({ queryKey: ['pm', 'mission-runs', missionId] });
      if (selectedMission?.id === missionId) {
        refreshRuns();
      }
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
  });
  const runningMissionId = runMissionNowMut.isPending
    ? Number(runMissionNowMut.variables)
    : null;

  const cancelRunMut = useMutation({
    mutationFn: (taskId: string) => agentApi.cancelPmResearchTask(taskId),
    onSuccess: () => {
      message.success(t('operations.pmBackgroundCancelling', 'Cancellation requested for background task'));
      refreshRuns();
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
  });

  const resumeRunMut = useMutation({
    mutationFn: (taskId: string) => agentApi.resumePmResearchTask(taskId),
    onSuccess: () => {
      message.success(t('operations.pmBackgroundResumed', 'Background research task resumed'));
      refreshRuns();
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
  });

  const handlePreviewCron = async () => {
    const scheduleCron = String(missionForm.getFieldValue('scheduleCron') ?? '').trim();
    if (!scheduleCron) {
      setCronPreviewError('invalid schedule_cron');
      setCronPreviewRuns([]);
      return;
    }
    setCronPreviewLoading(true);
    setCronPreviewError(null);
    try {
      const resp = await pmApi.previewMissionCron({ scheduleCron, count: 7 });
      setCronPreviewRuns(resp.nextRuns ?? []);
    } catch (e: any) {
      const msg = String(e?.message ?? 'invalid schedule_cron');
      setCronPreviewError(msg.includes('invalid schedule_cron') ? 'invalid schedule_cron' : msg);
      setCronPreviewRuns([]);
    } finally {
      setCronPreviewLoading(false);
    }
  };

  const handleDownloadReply = () => {
    if (!replyHasContent) {
      message.warning(t('operations.noReplyContent', '当前任务暂无可展示回复'));
      return;
    }
    try {
      const blob = new Blob([replyMarkdown], { type: 'text/markdown;charset=utf-8' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      const safeTaskId = (selectedReplyRun?.taskId || 'task-reply').replace(/[^a-zA-Z0-9_-]/g, '_');
      a.href = url;
      a.download = `pm-task-reply-${safeTaskId}.md`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      message.success(t('operations.replyDownloadSuccess', '回复已下载'));
    } catch {
      message.error(t('operations.replyDownloadFailed', '下载回复失败'));
    }
  };

  const handleShareReply = () => {
    if (!replyHasContent) {
      message.warning(t('operations.noReplyContent', '当前任务暂无可展示回复'));
      return;
    }
    const payload: PmSharePreviewPayload = {
      schema: 'aos-pm-share-v1',
      title: `${selectedMission?.missionName || t('operations.tasksTitle', '任务中心')} - ${t('operations.replyPreviewTitle', '回复预览')}`,
      generatedAt: new Date().toISOString(),
      messageId: selectedReplyRun?.taskId || '',
      taskId: selectedReplyRun?.taskId || null,
      content: replyMarkdown.slice(0, PM_SHARE_PREVIEW_MAX_CHARS),
      truncated: replyMarkdown.length > PM_SHARE_PREVIEW_MAX_CHARS,
    };
    const shareUrl = buildPmSharePreviewUrl(payload);
    if (!shareUrl) {
      message.error(t('operations.replyShareOpenFailed', '打开分享预览失败'));
      return;
    }
    const opened = window.open(shareUrl, '_blank', 'noopener,noreferrer');
    if (!opened) {
      message.error(t('operations.replyShareOpenFailed', '打开分享预览失败'));
    }
  };

  return (
    <div style={{ padding: '24px 24px 0', minHeight: 360 }}>
      <Card
        title={t('operations.tasksTitle', '任务中心')}
        extra={<Button onClick={refresh}>{t('operations.ui.buttons.refresh')}</Button>}
      >
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 12 }}
          message={t(
            'operations.tasksDesc',
            '面向 PM 的一句话任务中心：配置任务意图后即可定时执行，系统自动检索、提炼并沉淀证据与机会。',
          )}
        />

        {missionLoadError ? (
          <Alert
            type="error"
            showIcon
            style={{ marginBottom: 12 }}
            message={t('operations.taskLoadFailed', '任务数据加载失败')}
            description={
              missionLoadError instanceof Error
                ? missionLoadError.message
                : t('common.unknownError', '未知错误')
            }
            action={(
              <Button
                size="small"
                onClick={retryMissionLoad}
                loading={missionSummaryQ.isFetching || missionsQ.isFetching}
              >
                {t('common.retry', '重试')}
              </Button>
            )}
          />
        ) : null}

        <Row gutter={[12, 12]} style={{ marginBottom: 12 }}>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.taskMetricEnabled', 'Enabled Missions')}
                value={missionSummaryQ.data?.enabledMissions ?? 0}
                suffix={`/ ${missionSummaryQ.data?.totalMissions ?? 0}`}
                loading={missionSummaryQ.isLoading}
                valueStyle={{ fontSize: 22 }}
              />
            </Card>
          </Col>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.taskMetricActiveRuns', 'Active Runs')}
                value={(missionSummaryQ.data?.queuedRuns ?? 0) + (missionSummaryQ.data?.runningRuns ?? 0)}
                loading={missionSummaryQ.isLoading}
                valueStyle={{ fontSize: 22, color: '#1677ff' }}
              />
              <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                {t('operations.taskMetricCancelling', 'Cancelling')}: {missionSummaryQ.data?.cancellingRuns ?? 0}
              </Typography.Text>
            </Card>
          </Col>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.taskMetricSuccess30d', '30d Success')}
                value={percentText(missionSummaryQ.data?.successRate30d)}
                loading={missionSummaryQ.isLoading}
                valueStyle={{ fontSize: 22, color: '#3f8600' }}
              />
            </Card>
          </Col>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.taskMetricAvgElapsed30d', 'Avg Elapsed 30d')}
                value={formatDurationMs(missionSummaryQ.data?.avgElapsedMs30d)}
                loading={missionSummaryQ.isLoading}
                valueStyle={{ fontSize: 22 }}
              />
            </Card>
          </Col>
        </Row>

        <Row gutter={12} style={{ marginBottom: 12 }}>
          <Col xs={24} md={24} style={{ textAlign: 'right' }}>
            <Button
              type="primary"
              icon={<PlusOutlined />}
              onClick={() => {
                setEditingMission(null);
                missionForm.resetFields();
                missionForm.setFieldsValue({
                  scheduleCron: '0 0 9 * * *',
                  enabled: true,
                });
                resetCronPreview();
                setMissionOpen(true);
              }}
            >
              {t('operations.createTask', '新建任务')}
            </Button>
          </Col>
        </Row>

        <Table
          rowKey="id"
          loading={missionsQ.isLoading}
          dataSource={missions}
          locale={{ emptyText: t('operations.noTasks', '暂无任务') }}
          pagination={{
            current: missionPage,
            pageSize: missionPageSize,
            total: missionsQ.data?.total ?? 0,
            showSizeChanger: true,
            onChange: (page, pageSize) => {
              setMissionPage(page);
              setMissionPageSize(pageSize);
            },
          }}
          columns={[
            { title: t('common.name'), dataIndex: 'missionName', width: 180 },
            { title: t('operations.collectionIntent', '采集意图'), dataIndex: 'intent', ellipsis: true },
            {
              title: t('operations.cron', 'Cron'),
              dataIndex: 'scheduleCron',
              width: 160,
              render: (v?: string) => v || '-',
            },
            {
              title: t('common.status'),
              dataIndex: 'enabled',
              width: 100,
              render: (v: boolean) =>
                v ? <Tag color="green">{t('operations.statusEnabled', '启用')}</Tag> : <Tag>{t('operations.statusDisabled', '停用')}</Tag>,
            },
            {
              title: t('common.actions'),
              width: 320,
              render: (_: unknown, row: Mission) => (
                <Space wrap>
                  <Button
                    size="small"
                    icon={<FileSearchOutlined />}
                    onClick={() => {
                      qc.invalidateQueries({ queryKey: ['pm', 'mission-runs', row.id] });
                      setSelectedMission(row);
                      setRunPage(1);
                      setRunPageSize(20);
                      setRunStatusFilter(undefined);
                      setRunDrawerOpen(true);
                    }}
                  >
                    {t('operations.taskRecords', '任务记录')}
                  </Button>
                  <Popconfirm
                    title={t('operations.runNowConfirm', '确定立即运行该任务吗？')}
                    onConfirm={() => runMissionNowMut.mutate(row.id)}
                    okText={t('common.confirm')}
                    cancelText={t('common.cancel')}
                  >
                    <Button
                      size="small"
                      type="primary"
                      icon={<PlayCircleOutlined />}
                      loading={runningMissionId === row.id}
                      style={{
                        background: '#4f7cff',
                        borderColor: '#4f7cff',
                        color: '#ffffff',
                        fontWeight: 600,
                      }}
                    >
                      {t('operations.runNow', '立即运行')}
                    </Button>
                  </Popconfirm>
                  <Button
                    size="small"
                    icon={<EditOutlined />}
                    onClick={() => {
                      setEditingMission(row);
                      missionForm.setFieldsValue({ ...row });
                      resetCronPreview();
                      setMissionOpen(true);
                    }}
                  />
                  <Popconfirm
                    title={t('common.deleteConfirm')}
                    onConfirm={() => deleteMissionMut.mutate(row.id)}
                    okText={t('common.confirm')}
                    cancelText={t('common.cancel')}
                  >
                    <Button size="small" danger icon={<DeleteOutlined />} />
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      </Card>

      <Modal
        title={editingMission ? t('operations.editTask', '编辑任务') : t('operations.createTask', '新建任务')}
        open={missionOpen}
        onCancel={() => {
          setMissionOpen(false);
          setEditingMission(null);
          resetCronPreview();
        }}
        onOk={() => missionForm.submit()}
        confirmLoading={createMissionMut.isPending || updateMissionMut.isPending}
      >
        <Form
          form={missionForm}
          layout="vertical"
          onFinish={(values) => {
            if (editingMission) {
              updateMissionMut.mutate({ id: editingMission.id, payload: values });
            } else {
              createMissionMut.mutate(values);
            }
          }}
        >
          <Form.Item name="missionName" label={t('common.name')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="intent" label={t('operations.collectionIntent', '采集意图')} rules={[{ required: true }]}>
            <Input.TextArea rows={3} placeholder={t('operations.collectionIntentPlaceholder')} />
          </Form.Item>
          <Form.Item name="scheduleCron" label={t('operations.cron', 'Cron')}>
            <Input placeholder="0 0 9 * * *" />
          </Form.Item>
          <div style={{ marginTop: -12, marginBottom: 12 }}>
            <Button
              type="link"
              icon={<ClockCircleOutlined />}
              loading={cronPreviewLoading}
              onClick={handlePreviewCron}
              style={{ paddingLeft: 0 }}
            >
              {t('operations.previewCron', '查看下次运行时间')} ({t('operations.previewCronUtcLabel', 'UTC')})
            </Button>
            {cronPreviewError ? (
              <Typography.Text type="danger" style={{ display: 'block' }}>
                {cronPreviewError}
              </Typography.Text>
            ) : null}
            {!cronPreviewError && cronPreviewRuns.length > 0 ? (
              <div style={{ marginTop: 6, maxHeight: 140, overflow: 'auto' }}>
                {cronPreviewRuns.map((v, idx) => (
                  <Typography.Text key={`${v}-${idx}`} type="secondary" style={{ display: 'block' }}>
                    {idx + 1}. {fmtTime(v, locale, { timeZone: 'UTC' })} {t('operations.previewCronUtcLabel', 'UTC')}
                  </Typography.Text>
                ))}
              </div>
            ) : null}
          </div>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked">
            <Switch />
          </Form.Item>
        </Form>
      </Modal>

      <Drawer
        title={selectedMission ? `${selectedMission.missionName} - ${t('operations.taskRecords', '任务记录')}` : t('operations.taskRecords', '任务记录')}
        width="min(1120px, 96vw)"
        open={runDrawerOpen}
        onClose={() => {
          setRunDrawerOpen(false);
          setRunStatusFilter(undefined);
          setSelectedReplyRun(null);
          setReplyModalOpen(false);
        }}
        extra={<Button onClick={refreshRuns}>{t('operations.ui.buttons.refresh')}</Button>}
      >
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 12 }}
          message={t(
            'operations.taskCancelHint',
            'Cancel is cooperative for operations research tasks. Running external requests may take a few seconds to settle.',
          )}
        />
        <Space style={{ marginBottom: 12 }} wrap>
          <Select
            allowClear
            style={{ minWidth: 180 }}
            placeholder={t('common.status', 'Status')}
            value={runStatusFilter}
            onChange={(value) => {
              setRunStatusFilter((value as string | undefined) ?? undefined);
              setRunPage(1);
            }}
            options={[
              { value: 'queued', label: t('operations.statusQueued', 'Queued') },
              { value: 'running', label: t('operations.statusRunning', 'Running') },
              { value: 'cancelling', label: t('operations.statusCancelling', 'Cancelling') },
              { value: 'completed', label: t('operations.statusCompleted', 'Completed') },
              { value: 'failed', label: t('operations.statusFailed', 'Failed') },
              { value: 'cancelled', label: t('operations.statusCancelled', 'Cancelled') },
            ]}
          />
        </Space>
        <Table
          rowKey="taskId"
          loading={missionRunsQ.isLoading}
          dataSource={runs}
          locale={{ emptyText: t('operations.noTaskRecords', '暂无任务记录') }}
          scroll={{ x: 840 }}
          pagination={{
            current: runPage,
            pageSize: runPageSize,
            total: missionRunsQ.data?.total ?? 0,
            showSizeChanger: true,
            onChange: (page, size) => {
              setRunPage(page);
              setRunPageSize(size);
            },
          }}
          columns={[
            {
              title: t('operations.taskRunTaskId', '任务 ID'),
              dataIndex: 'taskId',
              width: 180,
              ellipsis: true,
            },
            {
              title: t('common.status'),
              dataIndex: 'status',
              width: 110,
              render: (v: string) => statusTag(v, t),
            },
            {
              title: t('operations.taskRunStage', '阶段'),
              dataIndex: 'stage',
              width: 100,
              render: (v?: string | null) => v || '-',
            },
            {
              title: t('operations.taskRunElapsedMs', '耗时(ms)'),
              dataIndex: 'elapsedMs',
              width: 100,
              render: (v: number) => v ?? 0,
            },
            {
              title: t('common.updatedAt', '更新时间'),
              dataIndex: 'updatedAt',
              width: 160,
              render: (v: string) => fmtTime(v, locale),
            },
            {
              title: t('common.actions'),
              width: 190,
              fixed: 'right' as const,
              render: (_: unknown, row: MissionTaskRun) => (
                <Space size={4} wrap>
                  <Button
                    size="small"
                    onClick={() => {
                      setSelectedReplyRun(row);
                      setReplyModalOpen(true);
                    }}
                  >
                    {t('operations.viewReply', '查看回复')}
                  </Button>
                  {isRunCancellable(row.status) ? (
                    <Popconfirm
                      title={t('operations.taskCancelConfirm', 'Cancel this task?')}
                      onConfirm={() => cancelRunMut.mutate(row.taskId)}
                      okText={t('common.confirm')}
                      cancelText={t('common.cancel')}
                    >
                      <Button
                        size="small"
                        danger
                        icon={<CloseCircleOutlined />}
                        loading={cancelRunMut.isPending && cancelRunMut.variables === row.taskId}
                      >
                        {t('common.cancel')}
                      </Button>
                    </Popconfirm>
                  ) : null}
                  {isRunResumable(row.status) ? (
                    <Button
                      size="small"
                      icon={<PlayCircleOutlined />}
                      loading={resumeRunMut.isPending && resumeRunMut.variables === row.taskId}
                      onClick={() => resumeRunMut.mutate(row.taskId)}
                    >
                      {t('operations.pmBackgroundResume', 'Resume Background Task')}
                    </Button>
                  ) : null}
                </Space>
              ),
            },
          ]}
        />
      </Drawer>

      <Modal
        title={t('operations.replyPreviewTitle', '回复预览')}
        open={replyModalOpen}
        width={980}
        onCancel={() => {
          setReplyModalOpen(false);
          setSelectedReplyRun(null);
        }}
        footer={[
          <Button key="download" icon={<DownloadOutlined />} onClick={handleDownloadReply}>
            {t('operations.downloadReply', '下载回复')}
          </Button>,
          <Button key="share" icon={<ShareAltOutlined />} onClick={handleShareReply}>
            {t('operations.shareReply', '分享回复')}
          </Button>,
          <Button
            key="close"
            type="primary"
            onClick={() => {
              setReplyModalOpen(false);
              setSelectedReplyRun(null);
            }}
          >
            {t('common.close')}
          </Button>,
        ]}
      >
        {!replyHasContent ? (
          <Alert
            type="info"
            showIcon
            message={t('operations.noReplyContent', '当前任务暂无可展示回复')}
          />
        ) : (
          <div
            style={{
              maxHeight: '68vh',
              overflow: 'auto',
              padding: '0 2px',
            }}
          >
            <Markdown>{replyMarkdown}</Markdown>
          </div>
        )}
      </Modal>
    </div>
  );
}
