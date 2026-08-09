import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import {
  Table, Button, Modal, Form, Input, Select, Space, Tag,
  message, Popconfirm, Typography, Card, Spin, Empty, Tooltip, Alert,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined, LineChartOutlined,
  CheckOutlined, CloseOutlined, SendOutlined,
} from '@ant-design/icons';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  MetricItem,
  CreateMetricRequest,
  UpdateMetricRequest,
  DataSourceInfo,
} from '@/types';

const { Text } = Typography;

const STATUS_COLORS: Record<string, string> = {
  draft: 'default',
  review: 'processing',
  published: 'success',
  deprecated: 'warning',
};

const GRANULARITY_OPTIONS = (t: (key: string) => string) => [
  { value: 'day', label: t('management.metrics.granularityOptions.day') },
  { value: 'week', label: t('management.metrics.granularityOptions.week') },
  { value: 'month', label: t('management.metrics.granularityOptions.month') },
  { value: 'quarter', label: t('management.metrics.granularityOptions.quarter') },
  { value: 'year', label: t('management.metrics.granularityOptions.year') },
];

interface MetricFormValues {
  metricName: string;
  metricAliases: string;
  expression: string;
  filterConditions: string;
  description: string;
  granularity: string;
}

function parseFilterConditionsInput(raw: string): Record<string, unknown> | string | undefined {
  const text = raw.trim();
  if (!text) {
    return undefined;
  }

  // Prefer JSON for precise, lossless structure.
  if ((text.startsWith('{') && text.endsWith('}')) || (text.startsWith('[') && text.endsWith(']'))) {
    try {
      const parsed = JSON.parse(text) as unknown;
      if (parsed && typeof parsed === 'object') {
        return parsed as Record<string, unknown>;
      }
      return text;
    } catch {
      return text;
    }
  }

  // Backward-compatible simple "k = 'v' AND k2 = 'v2'" parser.
  const obj: Record<string, unknown> = {};
  for (const part of text.split(/\s+AND\s+/i).map((p) => p.trim()).filter(Boolean)) {
    const quoted = part.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*'([^']*)'$/);
    const plain = part.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^\s].*)$/);
    if (quoted) {
      obj[quoted[1]] = quoted[2];
      continue;
    }
    if (plain) {
      const value = plain[2].trim();
      if (value) {
        obj[plain[1]] = value;
      }
    }
  }

  return Object.keys(obj).length > 0 ? obj : text;
}

function formatFilterConditionsForEditor(value: unknown): string {
  if (value == null) {
    return '';
  }
  if (typeof value === 'string') {
    return value;
  }
  if (typeof value === 'object') {
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return '';
    }
  }
  return String(value);
}

interface EditModalState {
  open: boolean;
  metric: MetricItem | null;
}

export function MetricsTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [form] = Form.useForm<MetricFormValues>();
  const [editModal, setEditModal] = useState<EditModalState>({ open: false, metric: null });
  const [selectedDsId, setSelectedDsId] = useState<string>('');

  const { data: dsResp, isLoading: dsLoading } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list({ per_page: 100 }),
  });

  const metricsQuery = useQuery({
    queryKey: queryKeys.nl2sql.metrics(selectedDsId || ''),
    queryFn: () => nl2sqlApi.listMetrics(selectedDsId),
    enabled: !!selectedDsId,
  });

  const invalidate = () => qc.invalidateQueries({ queryKey: queryKeys.nl2sql.metrics(selectedDsId || '') });

  const createMutation = useMutation({
    mutationFn: (data: CreateMetricRequest) => nl2sqlApi.createMetric(selectedDsId, data),
    onSuccess: () => { message.success(t('management.metrics.createSuccess')); invalidate(); setEditModal({ open: false, metric: null }); form.resetFields(); },
    onError: () => message.error(t('common.failed')),
  });

  const updateMutation = useMutation({
    mutationFn: ({ metricId, data }: { metricId: number; data: UpdateMetricRequest }) =>
      nl2sqlApi.updateMetric(selectedDsId, metricId, data),
    onSuccess: () => { message.success(t('management.metrics.updateSuccess')); invalidate(); setEditModal({ open: false, metric: null }); form.resetFields(); },
    onError: () => message.error(t('common.failed')),
  });

  const deleteMutation = useMutation({
    mutationFn: (metricId: number) => nl2sqlApi.deleteMetric(selectedDsId, metricId),
    onSuccess: () => { message.success(t('management.metrics.deleteSuccess')); invalidate(); },
    onError: () => message.error(t('common.failed')),
  });

  const statusMutation = useMutation({
    mutationFn: ({ metricId, action }: { metricId: number; action: string }) =>
      nl2sqlApi.updateMetricStatus(selectedDsId, metricId, action),
    onSuccess: () => { message.success(t('management.metrics.statusUpdateSuccess')); invalidate(); },
    onError: () => message.error(t('common.failed')),
  });

  const handleOpenCreate = () => {
    form.setFieldsValue({ metricName: '', metricAliases: '', expression: '', filterConditions: '', description: '', granularity: 'day' });
    setEditModal({ open: true, metric: null });
  };

  const handleOpenEdit = (record: MetricItem) => {
    const aliasesStr = record.metricAliases?.join?.(',') ?? '';
    const filterStr = formatFilterConditionsForEditor(record.filterConditions);
    form.setFieldsValue({ metricName: record.metricName, metricAliases: aliasesStr, expression: record.expression, filterConditions: filterStr, description: record.description ?? '', granularity: record.granularity });
    setEditModal({ open: true, metric: record });
  };

  const handleSave = () => {
    form.validateFields().then((values) => {
      const aliases = values.metricAliases ? values.metricAliases.split(',').map((s) => s.trim()).filter(Boolean) : [];
      const filterConditions = parseFilterConditionsInput(values.filterConditions || '');
      const payload = { metricName: values.metricName, metricAliases: aliases, expression: values.expression, filterConditions, description: values.description || undefined, granularity: values.granularity };
      if (editModal.metric) {
        updateMutation.mutate({ metricId: editModal.metric.id, data: payload });
      } else {
        createMutation.mutate(payload);
      }
    });
  };

  const statusLabel = (s?: string | null) => {
    if (!s) return null;
    const labels: Record<string, string> = {
      draft: t('management.metrics.statusDraft'),
      review: t('management.metrics.statusReview'),
      published: t('management.metrics.statusPublished'),
      deprecated: t('management.metrics.statusDeprecated'),
    };
    return <Tag color={STATUS_COLORS[s] ?? 'default'} style={{ fontSize: 11 }}>{labels[s] ?? s}</Tag>;
  };

  const datasources: DataSourceInfo[] = useMemo(() => Array.isArray(dsResp)
    ? dsResp
    : Array.isArray(dsResp?.data_sources)
      ? dsResp.data_sources
      : [], [dsResp]);
  const dsOptions = useMemo(
    () => datasources.map((ds: DataSourceInfo) => ({ value: ds.id, label: ds.name })),
    [datasources],
  );
  useEffect(() => {
    if (!selectedDsId && dsOptions.length > 0) {
      setSelectedDsId(dsOptions[0].value);
    }
  }, [dsOptions, selectedDsId]);
  const isModalLoading = createMutation.isPending || updateMutation.isPending;

  const tableColumns = [
    {
      title: t('management.metrics.metricName'),
      dataIndex: 'metricName',
      key: 'metricName',
      render: (name: string) => (
        <Space>
          <LineChartOutlined style={{ color: 'var(--accent-color)', fontSize: 13 }} />
          <Text strong style={{ fontSize: 13 }}>{name}</Text>
        </Space>
      ),
    },
    {
      title: t('management.metrics.status'),
      dataIndex: 'status',
      key: 'status',
      width: 110,
      render: (s: string | null) => statusLabel(s) ?? <Tag style={{ fontSize: 11 }}>{t('management.metrics.statusDraft')}</Tag>,
    },
    {
      title: t('management.metrics.aliases'),
      dataIndex: 'metricAliases',
      key: 'metricAliases',
      render: (aliases: string[]) =>
        aliases && aliases.length > 0
          ? aliases.map((a) => <Tag key={a} style={{ fontSize: 11 }}>{a}</Tag>)
          : <Text type="secondary" style={{ fontSize: 12 }}>—</Text>,
    },
    {
      title: t('management.metrics.expression'),
      dataIndex: 'expression',
      key: 'expression',
      render: (expr: string) => (
        <Tooltip title={expr}>
          <Text code style={{ fontSize: 11, maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', display: 'block' }}>{expr}</Text>
        </Tooltip>
      ),
    },
    {
      title: t('management.metrics.granularity'),
      dataIndex: 'granularity',
      key: 'granularity',
      width: 100,
      render: (g: string) => <Tag color="blue" style={{ fontSize: 11 }}>{g}</Tag>,
    },
    {
      title: t('management.metrics.createdBy'),
      dataIndex: 'createdBy',
      key: 'createdBy',
      width: 120,
      render: (v: string | null) => <Text type="secondary" style={{ fontSize: 12 }}>{v ?? '—'}</Text>,
    },
    {
      title: '',
      key: 'actions',
      width: 160,
      render: (_: unknown, record: MetricItem) => {
        const status = record.status ?? 'draft';
        return (
          <Space size={4}>
            {status === 'draft' && (
              <Tooltip title={t('management.metrics.submitReview')}>
                <Popconfirm title={t('management.metrics.submitReviewConfirm')} onConfirm={() => statusMutation.mutate({ metricId: record.id, action: 'submit_review' })} okText={t('common.confirm')} cancelText={t('common.cancel')}>
                  <Button type="text" size="small" icon={<SendOutlined />} style={{ color: '#1677ff', fontSize: 13 }} />
                </Popconfirm>
              </Tooltip>
            )}
            {status === 'review' && (
              <>
                <Tooltip title={t('management.metrics.approve')}>
                  <Popconfirm title={t('management.metrics.approveConfirm')} onConfirm={() => statusMutation.mutate({ metricId: record.id, action: 'approve' })} okText={t('common.confirm')} cancelText={t('common.cancel')}>
                    <Button type="text" size="small" icon={<CheckOutlined />} style={{ color: '#52c41a', fontSize: 13 }} />
                  </Popconfirm>
                </Tooltip>
                <Tooltip title={t('management.metrics.reject')}>
                  <Popconfirm title={t('management.metrics.rejectConfirm')} onConfirm={() => statusMutation.mutate({ metricId: record.id, action: 'reject' })} okText={t('common.confirm')} cancelText={t('common.cancel')}>
                    <Button type="text" size="small" icon={<CloseOutlined />} danger style={{ fontSize: 13 }} />
                  </Popconfirm>
                </Tooltip>
              </>
            )}
            <Button type="text" size="small" icon={<EditOutlined />} onClick={() => handleOpenEdit(record)} style={{ color: 'var(--text-secondary)', fontSize: 13 }} />
            <Popconfirm title={t('management.metrics.deleteConfirm')} onConfirm={() => deleteMutation.mutate(record.id)} okText={t('common.yes')} cancelText={t('common.no')}>
              <Button type="text" size="small" icon={<DeleteOutlined />} danger style={{ fontSize: 13 }} />
            </Popconfirm>
          </Space>
        );
      },
    },
  ];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
      <Alert
        type="info"
        showIcon
        message={t('management.metrics.guideTitle')}
        description={t('management.metrics.guideDescription')}
      />
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
        <Select
          placeholder={t('nl2sql.selectDataSourceFirst')}
          value={selectedDsId || undefined}
          onChange={(val) => setSelectedDsId(val)}
          options={dsOptions}
          loading={dsLoading}
          style={{ minWidth: 200 }}
          allowClear
          showSearch
          filterOption={(input, option) => (option?.label as string)?.toLowerCase().includes(input.toLowerCase())}
        />
        <Button type="primary" icon={<PlusOutlined />} onClick={handleOpenCreate} disabled={!selectedDsId}>
          {t('management.metrics.newMetric')}
        </Button>
        <Text type="secondary" style={{ fontSize: 12 }}>{t('management.metrics.hint')}</Text>
      </div>

      <Card size="small" style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }} styles={{ body: { padding: 0 } }}>
        {metricsQuery.isLoading ? (
          <div style={{ padding: 48, textAlign: 'center' }}><Spin /></div>
        ) : !selectedDsId ? (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('nl2sql.selectDataSourceFirst')} style={{ padding: 48 }} />
        ) : metricsQuery.data?.metrics.length === 0 ? (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('management.metrics.noMetrics')} style={{ padding: 48 }} />
        ) : (
          <Table dataSource={metricsQuery.data?.metrics} columns={tableColumns} rowKey="id" size="small" pagination={{ pageSize: 20, size: 'small' }} style={{ fontSize: 12 }} />
        )}
      </Card>

      <Modal
        open={editModal.open}
        title={editModal.metric ? t('management.metrics.editMetric') : t('management.metrics.newMetric')}
        onCancel={() => { setEditModal({ open: false, metric: null }); form.resetFields(); }}
        onOk={handleSave}
        okText={t('management.metrics.save')}
        cancelText={t('management.metrics.cancel')}
        confirmLoading={isModalLoading}
        width={560}
        destroyOnHidden
      >
        <Form form={form} layout="vertical" style={{ marginTop: 16 }}>
          <Form.Item name="metricName" label={t('management.metrics.metricName')} rules={[{ required: true, message: t('common.required') }]}>
            <Input placeholder={t('management.metrics.metricNamePlaceholder')} />
          </Form.Item>
          <Form.Item name="metricAliases" label={t('management.metrics.aliases')}>
            <Input placeholder={t('management.metrics.aliasesPlaceholder')} />
          </Form.Item>
          <Form.Item name="expression" label={t('management.metrics.expression')} rules={[{ required: true, message: t('common.required') }]}>
            <Input.TextArea placeholder={t('management.metrics.expressionPlaceholder')} rows={2} style={{ fontFamily: 'monospace' }} />
          </Form.Item>
          <Form.Item name="filterConditions" label={t('management.metrics.filterConditions')}>
            <Input.TextArea placeholder={t('management.metrics.filterConditionsPlaceholder')} rows={2} style={{ fontFamily: 'monospace' }} />
          </Form.Item>
          <Form.Item name="granularity" label={t('management.metrics.defaultGranularity')} initialValue="day">
            <Select options={GRANULARITY_OPTIONS(t)} />
          </Form.Item>
          <Form.Item name="description" label={t('management.metrics.description')}>
            <Input.TextArea placeholder={t('management.metrics.descriptionPlaceholder')} rows={2} />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
