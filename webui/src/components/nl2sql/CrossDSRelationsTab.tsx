// ── Cross-Datasource Relations Tab ──────────────────────────────────────────

import { useState, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Table, Button, Modal, Form, Input, Select, Space, Tag, message,
  Popconfirm, Typography, Card, Spin, Empty, Tooltip,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined, NodeIndexOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { ApiError } from '@/api/errors';
import type {
  CrossDSRelationItem, CreateCrossDSRelationRequest,
  UpdateCrossDSRelationRequest, ListCrossDSRelationsResponse,
} from '@/types';

const { Text } = Typography;

const MATCH_TYPES = (t: (key: string) => string) => [
  { label: t('management.crossDs.matchTypes.id'), value: 'id' },
  { label: t('management.crossDs.matchTypes.email'), value: 'email' },
  { label: t('management.crossDs.matchTypes.name'), value: 'name' },
  { label: t('management.crossDs.matchTypes.foreignKey'), value: 'foreign_key' },
  { label: t('management.crossDs.matchTypes.custom'), value: 'custom' },
];

export function CrossDSRelationsTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const [createOpen, setCreateOpen] = useState(false);
  const [editingItem, setEditingItem] = useState<CrossDSRelationItem | null>(null);
  const [filterVerified, setFilterVerified] = useState<boolean | undefined>(undefined);
  const [createForm] = Form.useForm();
  const [editForm] = Form.useForm();
  const toolbarBtnStyle: CSSProperties = { height: 40, borderRadius: 10 };

  const { data, isLoading } = useQuery<ListCrossDSRelationsResponse>({
    queryKey: queryKeys.nl2sql.crossDSRelations.all(),
    queryFn: () => nl2sqlApi.listCrossDSRelations(),
    staleTime: 30_000,
  });

  const { data: dsList } = useQuery({
    queryKey: queryKeys.dataSources.all(),
    queryFn: () => dataSourcesApi.list(),
    staleTime: 60_000,
  });

  const dsOptions = (dsList?.data_sources ?? []).map((d) => ({ label: d.name, value: d.id }));
  const relations = data?.relations ?? [];
  const filtered = relations.filter((r) =>
    filterVerified !== undefined ? r.verified === filterVerified : true,
  );
  const showCrossDsError = (err: unknown) => {
    if (err instanceof ApiError && err.message) {
      message.error(err.message);
      return;
    }
    const msg = (err as { message?: string })?.message;
    if (msg) {
      message.error(msg);
      return;
    }
    message.error(t('common.failed'));
  };
  const matchTypeLabel = (value: string) => {
    const option = MATCH_TYPES(t).find((item) => item.value === value);
    return option?.label ?? value;
  };

  const create = useMutation({
    mutationFn: (p: CreateCrossDSRelationRequest) => nl2sqlApi.createCrossDSRelation(p),
    onSuccess: () => {
      message.success(t('management.crossDs.create'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.crossDSRelations.all() });
      setCreateOpen(false);
      createForm.resetFields();
    },
    onError: (err) => showCrossDsError(err),
  });

  const update = useMutation({
    mutationFn: ({ id, data: p }: { id: number; data: UpdateCrossDSRelationRequest }) =>
      nl2sqlApi.updateCrossDSRelation(id, p),
    onSuccess: () => {
      message.success(t('management.crossDs.update'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.crossDSRelations.all() });
      setEditingItem(null);
    },
    onError: (err) => showCrossDsError(err),
  });

  const toggleVerified = useMutation({
    mutationFn: ({ id, verified }: { id: number; verified: boolean }) =>
      nl2sqlApi.updateCrossDSRelation(id, { verified }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.crossDSRelations.all() });
    },
    onError: (err) => showCrossDsError(err),
  });

  const del = useMutation({
    mutationFn: (id: number) => nl2sqlApi.deleteCrossDSRelation(id),
    onSuccess: () => {
      message.success(t('management.crossDs.delete'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.crossDSRelations.all() });
    },
    onError: (err) => showCrossDsError(err),
  });

  const columns = [
    {
      title: t('management.crossDs.leftDatasource'),
      key: 'left',
      width: 180,
      render: (_: unknown, r: CrossDSRelationItem) => (
        <Space direction="vertical" size={2}>
          <Tag color="blue">{r.leftDatasource}</Tag>
          <Text style={{ fontSize: 12 }}>
            <Text code style={{ fontSize: 11 }}>{r.leftTable}</Text>.{r.leftColumn}
          </Text>
        </Space>
      ),
    },
    {
      title: t('management.crossDs.rightDatasource'),
      key: 'right',
      width: 180,
      render: (_: unknown, r: CrossDSRelationItem) => (
        <Space direction="vertical" size={2}>
          <Tag color="green">{r.rightDatasource}</Tag>
          <Text style={{ fontSize: 12 }}>
            <Text code style={{ fontSize: 11 }}>{r.rightTable}</Text>.{r.rightColumn}
          </Text>
        </Space>
      ),
    },
    {
      title: t('management.crossDs.matchType'),
      dataIndex: 'matchType',
      key: 'matchType',
      width: 140,
      render: (v: string) => <Tag icon={<NodeIndexOutlined />}>{v ? matchTypeLabel(v) : '-'}</Tag>,
    },
    {
      title: t('management.crossDs.confidence'),
      dataIndex: 'confidence',
      key: 'confidence',
      width: 90,
      render: (v: number) => {
        const pct = v != null ? (Number(v) * 100).toFixed(0) : '—';
        return (
          <Tooltip title={`${pct}%`}>
            <Text type={v != null && Number(v) >= 0.8 ? 'success' : v != null && Number(v) >= 0.5 ? 'warning' : 'secondary'}>
              {pct}%
            </Text>
          </Tooltip>
        );
      },
    },
    {
      title: t('management.crossDs.verifiedStatus'),
      dataIndex: 'verified',
      key: 'verified',
      width: 100,
      render: (v: boolean, r: CrossDSRelationItem) => (
        <Button
          size="small"
          type={v ? 'primary' : 'default'}
          onClick={() => toggleVerified.mutate({ id: r.id, verified: !v })}
          loading={toggleVerified.isPending}
          style={{ borderRadius: 8 }}
        >
          {v ? t('management.crossDs.verified') : t('management.crossDs.unverified')}
        </Button>
      ),
    },
    {
      title: t('management.crossDs.source'),
      dataIndex: 'source',
      key: 'source',
      width: 90,
      render: (v: string) => (
        <Tag color={v === 'manual' ? 'orange' : 'default'}>
          {v === 'manual' ? t('management.crossDs.manual') : t('management.crossDs.auto')}
        </Tag>
      ),
    },
    {
      title: '',
      key: 'actions',
      width: 100,
      render: (_: unknown, r: CrossDSRelationItem) => (
        <Space size="small">
          <Button size="small" icon={<EditOutlined />} onClick={() => setEditingItem(r)} />
          <Popconfirm title={t('management.crossDs.deleteConfirm')} onConfirm={() => del.mutate(r.id)}>
            <Button size="small" danger icon={<DeleteOutlined />} />
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const RelationModal = ({
    open,
    initialData,
    onClose,
    onSubmit,
    loading,
    form,
  }: {
    open: boolean;
    initialData?: CrossDSRelationItem;
    onClose: () => void;
    onSubmit: (p: CreateCrossDSRelationRequest) => void;
    loading?: boolean;
    form: ReturnType<typeof Form.useForm>[0];
  }) => (
    <Modal
      title={initialData ? t('management.crossDs.editRelation') : t('management.crossDs.addRelation')}
      open={open}
      onCancel={() => { form.resetFields(); onClose(); }}
      footer={null}
      width={640}
      destroyOnHidden
    >
      <Form
        form={form}
        layout="vertical"
        initialValues={
          initialData
            ? {
                leftDatasource: initialData.leftDatasource,
                leftTable: initialData.leftTable,
                leftColumn: initialData.leftColumn,
                rightDatasource: initialData.rightDatasource,
                rightTable: initialData.rightTable,
                rightColumn: initialData.rightColumn,
                matchType: initialData.matchType,
              }
            : {}
        }
        onFinish={(values) => onSubmit(values as CreateCrossDSRelationRequest)}
      >
        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 16px' }}>
          <Form.Item name="leftDatasource" label={t('management.crossDs.leftDatasource')} rules={[{ required: true }]}>
            <Select showSearch filterOption={(i, o) => (o?.label ?? '').toLowerCase().includes(i.toLowerCase())} options={dsOptions} />
          </Form.Item>
          <Form.Item name="rightDatasource" label={t('management.crossDs.rightDatasource')} rules={[{ required: true }]}>
            <Select showSearch filterOption={(i, o) => (o?.label ?? '').toLowerCase().includes(i.toLowerCase())} options={dsOptions} />
          </Form.Item>
          <Form.Item name="leftTable" label={t('management.crossDs.leftTable')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="rightTable" label={t('management.crossDs.rightTable')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="leftColumn" label={t('management.crossDs.leftColumn')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="rightColumn" label={t('management.crossDs.rightColumn')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
        </div>
        <Form.Item name="matchType" label={t('management.crossDs.matchType')} rules={[{ required: true }]}>
          <Select options={MATCH_TYPES(t)} />
        </Form.Item>
        <Form.Item style={{ marginBottom: 0 }}>
          <Space>
            <Button type="primary" htmlType="submit" loading={loading}>
              {initialData ? t('management.crossDs.update') : t('management.crossDs.create')}
            </Button>
            <Button onClick={() => { form.resetFields(); onClose(); }}>
              {t('management.crossDs.cancel')}
            </Button>
          </Space>
        </Form.Item>
      </Form>
    </Modal>
  );

  return (
    <div>
      <Card size="small" style={{ marginBottom: 12, background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
          <NodeIndexOutlined style={{ color: 'var(--accent-color)' }} />
          <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
            {relations.length === 0
              ? t('management.crossDs.noRelations')
              : t('management.crossDs.relationsConfigured', { count: relations.length })}
          </Text>
        </div>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
          <Button
            type="primary"
            size="middle"
            icon={<PlusOutlined />}
            onClick={() => setCreateOpen(true)}
            style={{ ...toolbarBtnStyle, paddingInline: 18 }}
          >
            {t('management.crossDs.addRelation')}
          </Button>
          <Space>
            <Button
              size="middle"
              type={filterVerified === undefined ? 'primary' : 'default'}
              onClick={() => setFilterVerified(undefined)}
              style={toolbarBtnStyle}
            >
              {t('management.crossDs.all')}
            </Button>
            <Button
              size="middle"
              type={filterVerified === true ? 'primary' : 'default'}
              onClick={() => setFilterVerified(true)}
              style={toolbarBtnStyle}
            >
              {t('management.crossDs.verified')}
            </Button>
            <Button
              size="middle"
              type={filterVerified === false ? 'primary' : 'default'}
              onClick={() => setFilterVerified(false)}
              style={toolbarBtnStyle}
            >
              {t('management.crossDs.unverified')}
            </Button>
          </Space>
          <Text type="secondary" style={{ marginLeft: 'auto', fontSize: 12 }}>
            {filtered.length} / {relations.length}
          </Text>
        </div>
      </Card>

      <Spin spinning={isLoading}>
        <Table
          dataSource={filtered}
          columns={columns}
          rowKey="id"
          loading={isLoading}
          size="small"
          pagination={{ pageSize: 10, showSizeChanger: true }}
          scroll={{ x: 900 }}
          locale={{ emptyText: <Empty description={t('management.crossDs.noRelations')} image={Empty.PRESENTED_IMAGE_SIMPLE} /> }}
        />
      </Spin>

      <RelationModal
        open={createOpen}
        form={createForm}
        onClose={() => { setCreateOpen(false); createForm.resetFields(); }}
        onSubmit={(p) => create.mutate(p)}
        loading={create.isPending}
      />

      {editingItem && (
        <RelationModal
          open={true}
          form={editForm}
          initialData={editingItem}
          onClose={() => { setEditingItem(null); editForm.resetFields(); }}
          onSubmit={(p) => update.mutate({ id: editingItem.id, data: p })}
          loading={update.isPending}
        />
      )}
    </div>
  );
}
