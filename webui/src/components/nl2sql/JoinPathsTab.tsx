// ── JOIN Paths Tab — NL2SQL Management Page ──────────────────────────────────

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Table, Button, Modal, Form, Input, Select, Space, Tag, message,
  Popconfirm, Typography, Card, Spin, Empty, Tooltip, Alert, Switch,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined,
  BranchesOutlined, SyncOutlined, CheckCircleOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  JoinPathItem,
  CreateJoinPathRequest,
  UpdateJoinPathRequest,
  DataSourceInfo,
} from '@/types';

const { Text } = Typography;

interface JoinPathFormValues {
  sourceTable: string;
  targetTable: string;
  sourceColumn: string;
  targetColumn: string;
  joinType: string;
  notes?: string;
  cardinality: '1:1' | '1:N' | 'N:1' | 'N:N';
  temporalCondition?: string;
  nullable: boolean;
  dedupStrategy?: string;
  allowedGrains: string[];
}

interface EditModalState {
  open: boolean;
  path: JoinPathItem | null;
}

export function JoinPathsTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [form] = Form.useForm<JoinPathFormValues>();

  const [selectedDsId, setSelectedDsId] = useState<string | undefined>();
  const [editModal, setEditModal] = useState<EditModalState>({ open: false, path: null });
  const [discoverMsg, setDiscoverMsg] = useState<string | null>(null);

  // ── Data sources list ───────────────────────────────────────────────────────
  const { data: dsList, isLoading: dsLoading } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list({ per_page: 200 }),
  });

  const datasourceId = selectedDsId;

  useEffect(() => {
    const firstDatasourceId = dsList?.data_sources?.[0]?.id;
    if (!selectedDsId && firstDatasourceId) {
      setSelectedDsId(firstDatasourceId);
    }
  }, [dsList?.data_sources, selectedDsId]);

  // ── Join paths list ─────────────────────────────────────────────────────────
  const {
    data: pathsData,
    isLoading: pathsLoading,
    refetch: refetchPaths,
  } = useQuery({
    queryKey: queryKeys.nl2sql.joinPaths(datasourceId ?? ''),
    queryFn: () => nl2sqlApi.listJoinPaths(datasourceId!),
    enabled: !!datasourceId,
  });

  // ── Mutations ───────────────────────────────────────────────────────────────
  const discoverMutation = useMutation({
    mutationFn: (dsId: string) => nl2sqlApi.rediscoverJoinPaths(dsId),
    onSuccess: (res) => {
      message.success(t('management.joinPaths.pathsDiscoveredWithVisible', {
        discovered: res.pathsDiscovered,
        visible: res.pathsVisible,
      }));
      setDiscoverMsg(t('management.joinPaths.pathsDiscoveredWithVisible', {
        discovered: res.pathsDiscovered,
        visible: res.pathsVisible,
      }));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.joinPaths(datasourceId ?? '') });
    },
    onError: (err: unknown) =>
      message.error(err instanceof Error ? err.message : t('management.joinPaths.failed')),
  });

  const createMutation = useMutation({
    mutationFn: (data: CreateJoinPathRequest) =>
      nl2sqlApi.createJoinPath(datasourceId!, data),
    onSuccess: () => {
      message.success(t('management.joinPaths.createSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.joinPaths(datasourceId ?? '') });
      setEditModal({ open: false, path: null });
      form.resetFields();
    },
    onError: () => message.error(t('management.joinPaths.failed')),
  });

  const updateMutation = useMutation({
    mutationFn: ({ pathId, data }: { pathId: number; data: UpdateJoinPathRequest }) =>
      nl2sqlApi.updateJoinPath(datasourceId!, pathId, data),
    onSuccess: () => {
      message.success(t('management.joinPaths.updateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.joinPaths(datasourceId ?? '') });
      setEditModal({ open: false, path: null });
      form.resetFields();
    },
    onError: () => message.error(t('management.joinPaths.failed')),
  });

  const deleteMutation = useMutation({
    mutationFn: (pathId: number) => nl2sqlApi.deleteJoinPath(datasourceId!, pathId),
    onSuccess: () => {
      message.success(t('management.joinPaths.deleteSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.joinPaths(datasourceId ?? '') });
    },
    onError: () => message.error(t('management.joinPaths.failed')),
  });

  const verifyMutation = useMutation({
    mutationFn: ({ pathId, verified }: { pathId: number; verified: boolean }) =>
      nl2sqlApi.verifyJoinPath(datasourceId!, pathId, verified),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.joinPaths(datasourceId ?? '') });
    },
    onError: () => message.error(t('management.joinPaths.failed')),
  });

  // ── Join type dropdown options ───────────────────────────────────────────────
  const getJoinTypeOptions = () => [
    { value: 'INNER', label: t('management.joinPaths.joinTypeInner') },
    { value: 'LEFT', label: t('management.joinPaths.joinTypeLeft') },
    { value: 'RIGHT', label: t('management.joinPaths.joinTypeRight') },
    { value: 'FULL', label: t('management.joinPaths.joinTypeFull') },
  ];

  // ── Handlers ───────────────────────────────────────────────────────────────
  const handleOpenCreate = () => {
    form.setFieldsValue({
      sourceTable: '',
      targetTable: '',
      sourceColumn: '',
      targetColumn: '',
      joinType: 'INNER',
      cardinality: 'N:1',
      temporalCondition: '',
      nullable: false,
      dedupStrategy: '',
      allowedGrains: [],
      notes: '',
    });
    setEditModal({ open: true, path: null });
  };

  const handleOpenEdit = (record: JoinPathItem) => {
    form.setFieldsValue({
      sourceTable: record.sourceTable ?? '',
      targetTable: record.targetTable ?? '',
      sourceColumn: record.sourceColumn ?? '',
      targetColumn: record.targetColumn ?? '',
      joinType: record.joinType ?? 'INNER',
      cardinality: record.cardinality ?? 'N:1',
      temporalCondition: record.temporalCondition ?? '',
      nullable: record.nullable ?? false,
      dedupStrategy: record.dedupStrategy ?? '',
      allowedGrains: record.allowedGrains ?? [],
      notes: record.notes ?? '',
    });
    setEditModal({ open: true, path: record });
  };

  const handleSave = () => {
    form.validateFields().then((values) => {
      const payload = {
        sourceTable: values.sourceTable,
        targetTable: values.targetTable,
        sourceColumn: values.sourceColumn,
        targetColumn: values.targetColumn,
        joinType: values.joinType,
        cardinality: values.cardinality,
        temporalCondition: values.temporalCondition?.trim() || undefined,
        nullable: values.nullable,
        dedupStrategy: values.dedupStrategy?.trim() || undefined,
        allowedGrains: values.allowedGrains,
        notes: values.notes || undefined,
      };

      if (editModal.path) {
        updateMutation.mutate({ pathId: editModal.path.id, data: payload });
      } else {
        createMutation.mutate(payload);
      }
    });
  };

  // ── Derived data ───────────────────────────────────────────────────────────
  const dsOptions = dsList?.data_sources?.map((ds: DataSourceInfo) => ({
    value: ds.id,
    label: ds.name,
  })) ?? [];

  const isModalLoading = createMutation.isPending || updateMutation.isPending;
  const joinPathRows = (pathsData?.paths ?? []).filter((row) => row.source !== 'cross_ds');

  // ── Format path for display ───────────────────────────────────────────────
  const formatPath = (path: string[]): string => {
    return path.join(` ${t('common.arrow')} `);
  };

  // ── Table columns ──────────────────────────────────────────────────────────
  const tableColumns = [
    {
      title: t('management.joinPaths.sourceTable'),
      dataIndex: 'sourceTable',
      key: 'sourceTable',
      width: 160,
      render: (v: string) => (
        <Text style={{ fontSize: 12 }}>{v || t('common.dash')}</Text>
      ),
    },
    {
      title: t('management.joinPaths.targetTable'),
      dataIndex: 'targetTable',
      key: 'targetTable',
      width: 160,
      render: (v: string) => (
        <Text style={{ fontSize: 12 }}>{v || t('common.dash')}</Text>
      ),
    },
    {
      title: t('management.joinPaths.path'),
      dataIndex: 'path',
      key: 'path',
      render: (path: string[]) =>
        path && path.length > 0 ? (
          <Space>
            <BranchesOutlined style={{ color: 'var(--accent-color)', fontSize: 12 }} />
            <Text code style={{ fontSize: 11 }}>{formatPath(path)}</Text>
          </Space>
        ) : (
          <Text type="secondary" style={{ fontSize: 12 }}>{t('common.dash')}</Text>
        ),
    },
    {
      title: t('management.joinPaths.joinType'),
      dataIndex: 'joinType',
      key: 'joinType',
      width: 130,
      render: (v: string) =>
        v ? <Tag style={{ fontSize: 11 }}>{v}</Tag> : <Text type="secondary" style={{ fontSize: 12 }}>{t('common.dash')}</Text>,
    },
    {
      title: t('management.joinPaths.cardinality'),
      dataIndex: 'cardinality',
      key: 'cardinality',
      width: 90,
      render: (value?: string) => <Tag>{value || t('common.dash')}</Tag>,
    },
    {
      title: t('management.joinPaths.verifiedToggle'),
      dataIndex: 'verified',
      key: 'verified',
      width: 100,
      render: (verified: boolean, record: JoinPathItem) => (
          <Tooltip
            title={record.id > 0
              ? (verified ? t('management.joinPaths.clickToUnverify') : t('management.joinPaths.clickToVerify'))
              : t('management.joinPaths.failed')}
          >
          <Button
            type="text"
            size="small"
            icon={<CheckCircleOutlined style={{ color: verified ? '#52c41a' : 'var(--text-secondary)', fontSize: 14 }} />}
            onClick={() => {
              if (record.id <= 0) {
                message.warning(t('management.joinPaths.failed'));
                return;
              }
              verifyMutation.mutate({ pathId: record.id, verified: !verified });
            }}
            loading={verifyMutation.isPending}
            disabled={record.id <= 0}
            style={{ padding: '0 4px' }}
          />
        </Tooltip>
      ),
    },
    {
      title: t('management.joinPaths.notes'),
      dataIndex: 'notes',
      key: 'notes',
      ellipsis: true,
      render: (v?: string) =>
        v ? (
          <Tooltip title={v}>
            <Text type="secondary" style={{ fontSize: 12 }}>{v}</Text>
          </Tooltip>
        ) : (
          <Text type="secondary" style={{ fontSize: 12 }}>{t('common.dash')}</Text>
        ),
    },
    {
      title: t('management.joinPaths.actions'),
      key: 'actions',
      width: 100,
      render: (_: unknown, record: JoinPathItem) => (
        <Space size={4}>
          <Button
            type="text"
            size="small"
            icon={<EditOutlined />}
            disabled={record.id <= 0}
            onClick={() => handleOpenEdit(record)}
            style={{ color: 'var(--text-secondary)', fontSize: 13 }}
          />
          <Popconfirm
            title={t('management.joinPaths.deleteConfirm')}
            onConfirm={() => {
              if (record.id <= 0) {
                message.warning(t('management.joinPaths.failed'));
                return;
              }
              deleteMutation.mutate(record.id);
            }}
            okText={t('common.yes')}
            cancelText={t('common.no')}
            disabled={record.id <= 0}
          >
            <Button
              type="text"
              size="small"
              icon={<DeleteOutlined />}
              danger
              disabled={record.id <= 0}
              style={{ fontSize: 13 }}
            />
          </Popconfirm>
        </Space>
      ),
    },
  ];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
      {/* Header controls */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
        <Select
          placeholder={t('management.joinPaths.selectDatasource')}
          value={selectedDsId || undefined}
          onChange={(val) => { setSelectedDsId(val); setDiscoverMsg(null); }}
          options={dsOptions}
          loading={dsLoading}
          style={{ minWidth: 200 }}
          allowClear
          showSearch
          filterOption={(input, option) =>
            (option?.label as string)?.toLowerCase().includes(input.toLowerCase())
          }
        />
        <Button
          icon={<BranchesOutlined />}
          onClick={() => {
            if (datasourceId) discoverMutation.mutate(datasourceId);
          }}
          loading={discoverMutation.isPending}
          disabled={!datasourceId}
        >
          {discoverMutation.isPending
            ? t('management.joinPaths.rediscovering')
            : t('management.joinPaths.rediscover')}
        </Button>
        <Button
          icon={<SyncOutlined />}
          onClick={() => { refetchPaths(); setDiscoverMsg(null); }}
          disabled={!datasourceId}
        >
          {t('management.joinPaths.refreshList')}
        </Button>
        <Button
          type="primary"
          icon={<PlusOutlined />}
          onClick={handleOpenCreate}
          disabled={!datasourceId}
        >
          {t('management.joinPaths.newPath')}
        </Button>
      </div>

      {discoverMsg && (
        <Alert message={discoverMsg} type="success" showIcon closable
          onClose={() => setDiscoverMsg(null)} />
      )}

      {/* Paths table */}
      <Card
        size="small"
        style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
        styles={{ body: { padding: 0 } }}
      >
        {pathsLoading ? (
          <div style={{ padding: 48, textAlign: 'center' }}><Spin /></div>
        ) : !datasourceId ? (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={t('management.joinPaths.selectDatasource')}
            style={{ padding: 48 }}
          />
        ) : joinPathRows.length === 0 ? (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={t('management.joinPaths.noPaths')}
            style={{ padding: 48 }}
          />
        ) : (
          <Table
            dataSource={joinPathRows}
            columns={tableColumns}
            rowKey={(row) => `${row.id}-${row.source}-${row.sourceTable ?? ''}-${row.targetTable ?? ''}`}
            size="small"
            pagination={{ pageSize: 20, size: 'small' }}
            style={{ fontSize: 12 }}
          />
        )}
      </Card>

      {/* Create / Edit Modal */}
      <Modal
        open={editModal.open}
        title={editModal.path ? t('management.joinPaths.editTitle') : t('management.joinPaths.createTitle')}
        onCancel={() => { setEditModal({ open: false, path: null }); form.resetFields(); }}
        onOk={handleSave}
        okText={t('management.domains.save')}
        cancelText={t('management.domains.cancel')}
        confirmLoading={isModalLoading}
        width={520}
        destroyOnHidden
      >
        <Form form={form} layout="vertical" style={{ marginTop: 16 }}>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 12px' }}>
            <Form.Item
              name="sourceTable"
              label={t('management.joinPaths.sourceTable')}
              rules={[{ required: true, message: t('common.required') }]}
            >
              <Input placeholder={t('management.joinPaths.placeholderSourceTable')} />
            </Form.Item>
            <Form.Item
              name="targetTable"
              label={t('management.joinPaths.targetTable')}
              rules={[{ required: true, message: t('common.required') }]}
            >
              <Input placeholder={t('management.joinPaths.placeholderTargetTable')} />
            </Form.Item>
          </div>

          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 12px' }}>
            <Form.Item
              name="sourceColumn"
              label={t('management.joinPaths.sourceColumn')}
              rules={[{ required: true, message: t('common.required') }]}
            >
              <Input placeholder={t('management.joinPaths.placeholderSourceColumn')} />
            </Form.Item>
            <Form.Item
              name="targetColumn"
              label={t('management.joinPaths.targetColumn')}
              rules={[{ required: true, message: t('common.required') }]}
            >
              <Input placeholder={t('management.joinPaths.placeholderTargetColumn')} />
            </Form.Item>
          </div>

          <Form.Item
            name="joinType"
            label={t('management.joinPaths.joinType')}
            initialValue="INNER"
          >
            <Select options={getJoinTypeOptions()} />
          </Form.Item>

          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 12px' }}>
            <Form.Item
              name="cardinality"
              label={t('management.joinPaths.cardinality')}
              rules={[{ required: true, message: t('common.required') }]}
            >
              <Select options={[
                { value: '1:1', label: '1:1' },
                { value: '1:N', label: '1:N' },
                { value: 'N:1', label: 'N:1' },
                { value: 'N:N', label: 'N:N' },
              ]} />
            </Form.Item>
            <Form.Item name="nullable" label={t('management.joinPaths.nullable')} valuePropName="checked">
              <Switch />
            </Form.Item>
          </div>

          <Form.Item name="dedupStrategy" label={t('management.joinPaths.dedupStrategy')}>
            <Input placeholder={t('management.joinPaths.dedupStrategyPlaceholder')} />
          </Form.Item>

          <Form.Item name="temporalCondition" label={t('management.joinPaths.temporalCondition')}>
            <Input placeholder={t('management.joinPaths.temporalConditionPlaceholder')} />
          </Form.Item>

          <Form.Item name="allowedGrains" label={t('management.joinPaths.allowedGrains')}>
            <Select mode="multiple" options={[
              { value: 'entity', label: 'Entity' },
              { value: 'hour', label: 'Hour' },
              { value: 'day', label: 'Day' },
              { value: 'week', label: 'Week' },
              { value: 'month', label: 'Month' },
            ]} />
          </Form.Item>

          <Form.Item
            name="notes"
            label={t('management.joinPaths.notes')}
          >
            <Input.TextArea
              placeholder={t('management.joinPaths.notesPlaceholder')}
              rows={2}
            />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
