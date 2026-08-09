// ── Cross-Domain Clusters Tab ────────────────────────────────────────────────

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Table, Button, Modal, Form, Input, Select, Space, Tag,
  message, Popconfirm, Typography, Card, Spin, Empty, Tooltip,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined, ClusterOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { ApiError } from '@/api/errors';
import type {
  CrossDomainClusterItem,
  CreateCrossDomainClusterRequest,
  UpdateCrossDomainClusterRequest,
  ListCrossDomainClustersResponse,
} from '@/types';

const { Text } = Typography;

export function ClustersTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const [editingCluster, setEditingCluster] = useState<CrossDomainClusterItem | null>(null);

  const { data, isLoading } = useQuery<ListCrossDomainClustersResponse>({
    queryKey: queryKeys.nl2sql.crossDomainClusters.all(),
    queryFn: () => nl2sqlApi.listCrossDomainClusters(),
    staleTime: 30_000,
  });

  const clusters = data?.clusters ?? [];
  const showClusterError = (err: unknown, fallbackKey: string) => {
    if (err instanceof ApiError && err.message) {
      message.error(err.message);
      return;
    }
    const msg = (err as { message?: string })?.message;
    if (msg) {
      message.error(msg);
      return;
    }
    message.error(t(fallbackKey));
  };

  const create = useMutation({
    mutationFn: (payload: CreateCrossDomainClusterRequest) =>
      nl2sqlApi.createCrossDomainCluster(payload),
    onSuccess: () => {
      message.success(t('management.clusters.createSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.crossDomainClusters.all() });
      setCreateOpen(false);
    },
    onError: (err) => showClusterError(err, 'management.clusters.createFailed'),
  });

  const update = useMutation({
    mutationFn: ({ id, data: payload }: { id: number; data: UpdateCrossDomainClusterRequest }) =>
      nl2sqlApi.updateCrossDomainCluster(id, payload),
    onSuccess: () => {
      message.success(t('management.clusters.updateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.crossDomainClusters.all() });
      setEditingCluster(null);
    },
    onError: (err) => showClusterError(err, 'management.clusters.updateFailed'),
  });

  const del = useMutation({
    mutationFn: (id: number) => nl2sqlApi.deleteCrossDomainCluster(id),
    onSuccess: () => {
      message.success(t('management.clusters.deleteSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.crossDomainClusters.all() });
    },
    onError: (err) => showClusterError(err, 'management.clusters.deleteFailed'),
  });

  const columns = [
    {
      title: t('management.clusters.clusterName'),
      dataIndex: 'clusterName',
      key: 'clusterName',
      width: 170,
      render: (v: string) => (
        <Space>
          <ClusterOutlined style={{ color: '#7c3aed' }} />
          <Text strong>{v}</Text>
        </Space>
      ),
    },
    {
      title: t('management.clusters.description'),
      dataIndex: 'description',
      key: 'description',
      width: 180,
      ellipsis: true,
      render: (v: string | null) =>
        v ? <Text type="secondary" style={{ fontSize: 12 }}>{v}</Text> : <Text type="secondary">-</Text>,
    },
    {
      title: t('management.clusters.datasourceIds'),
      key: 'datasources',
      width: 220,
      render: (_: unknown, r: CrossDomainClusterItem) => (
        <Space direction="vertical" size={4} style={{ width: '100%' }}>
          {(r.datasourceIds ?? []).map((ds) => (
            <Text
              key={ds}
              style={{
                display: 'block',
                maxWidth: '100%',
                fontSize: 11,
                lineHeight: '16px',
                color: 'var(--text-secondary)',
                whiteSpace: 'normal',
                wordBreak: 'break-word',
                overflowWrap: 'anywhere',
              }}
            >
              {ds}
            </Text>
          ))}
        </Space>
      ),
    },
    {
      title: t('management.clusters.autoDiscovered'),
      dataIndex: 'autoDiscovered',
      key: 'autoDiscovered',
      width: 120,
      render: (v: boolean | number) => (
        <Tag color={v ? 'green' : 'default'}>
          {v ? t('management.clusters.autoDiscovered') : t('management.clusters.manual')}
        </Tag>
      ),
    },
    {
      title: t('management.clusters.createdBy'),
      dataIndex: 'createdBy',
      key: 'createdBy',
      width: 120,
      render: (v: string | null) =>
        v ? <Text type="secondary" style={{ fontSize: 12 }}>{v}</Text> : <Text type="secondary">-</Text>,
    },
    {
      title: t('management.clusters.createdAt'),
      dataIndex: 'createdAt',
      key: 'createdAt',
      width: 150,
      render: (v: string) => <Text type="secondary" style={{ fontSize: 12 }}>{v}</Text>,
    },
    {
      title: t('management.clusters.actions'),
      key: 'actions',
      width: 96,
      render: (_: unknown, r: CrossDomainClusterItem) => (
        <Space size="small">
          <Tooltip title={t('management.clusters.editCluster')}>
            <Button size="small" icon={<EditOutlined />} onClick={() => setEditingCluster(r)} />
          </Tooltip>
          <Popconfirm
            title={t('management.clusters.deleteConfirm')}
            onConfirm={() => del.mutate(r.id)}
          >
            <Tooltip title={t('common.delete')}>
              <Button size="small" danger icon={<DeleteOutlined />} />
            </Tooltip>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  return (
    <Card bodyStyle={{ padding: '16px 16px 0' }}>
      <div style={{ marginBottom: 12, display: 'flex', gap: 8, alignItems: 'center' }}>
        <Button
          type="primary"
          icon={<PlusOutlined />}
          onClick={() => setCreateOpen(true)}
        >
          {t('management.clusters.newCluster')}
        </Button>
      </div>

      {isLoading ? (
        <div style={{ textAlign: 'center', padding: 40 }}>
          <Spin />
        </div>
      ) : (
        <Table
          dataSource={clusters}
          columns={columns}
          rowKey="id"
          size="small"
          tableLayout="fixed"
          scroll={{ x: 980 }}
          pagination={{ pageSize: 10 }}
          locale={{ emptyText: <Empty description={t('management.clusters.noClusters')} /> }}
        />
      )}

      <ClusterModal
        open={createOpen}
        initialData={undefined}
        onClose={() => setCreateOpen(false)}
        onSubmit={(payload) => create.mutate(payload)}
        loading={create.isPending}
      />

      {editingCluster && (
        <ClusterModal
          open={true}
          initialData={editingCluster}
          onClose={() => setEditingCluster(null)}
          onSubmit={(payload) => update.mutate({ id: editingCluster.id, data: payload })}
          loading={update.isPending}
        />
      )}
    </Card>
  );
}

function ClusterModal({
  open,
  initialData,
  onClose,
  onSubmit,
  loading,
}: {
  open: boolean;
  initialData?: CrossDomainClusterItem;
  onClose: () => void;
  onSubmit: (payload: CreateCrossDomainClusterRequest) => void;
  loading?: boolean;
}) {
  const { t } = useTranslation();
  const { data: dsList } = useQuery({
    queryKey: ['datasources'],
    queryFn: () => dataSourcesApi.list(),
    staleTime: 60_000,
  });

  const [form] = Form.useForm();
  const datasourceOptions = (dsList?.data_sources ?? []).map((d) => ({
    label: d.name,
    value: d.id,
  }));

  return (
    <Modal
      title={initialData ? t('management.clusters.editCluster') : t('management.clusters.newCluster')}
      open={open}
      onCancel={() => { form.resetFields(); onClose(); }}
      footer={null}
      width={520}
    >
      <Form
        form={form}
        layout="vertical"
        initialValues={
          initialData
            ? {
                clusterName: initialData.clusterName,
                description: initialData.description,
                datasourceIds: initialData.datasourceIds,
                tables: (initialData.tables ?? []).join(', '),
              }
            : {}
        }
        onFinish={(values) => {
          const tables =
            typeof values.tables === 'string'
              ? values.tables.split(',').map((v: string) => v.trim()).filter(Boolean)
              : (values.tables ?? []);
          onSubmit({ ...values, tables } as CreateCrossDomainClusterRequest);
        }}
      >
        <div style={{ marginBottom: 10 }}>
          <Text type="secondary" style={{ fontSize: 12 }}>
            {t('management.clusters.usageHint')}
          </Text>
        </div>

        <Form.Item
          name="clusterName"
          label={t('management.clusters.clusterName')}
          rules={[{ required: true, message: t('management.clusters.clusterNamePlaceholder') }]}
        >
          <Input placeholder={t('management.clusters.clusterNamePlaceholder')} />
        </Form.Item>

        <Form.Item
          name="description"
          label={t('management.clusters.description')}
          extra={t('management.clusters.descriptionHint')}
        >
          <Input.TextArea
            placeholder={t('management.clusters.descriptionPlaceholder')}
            rows={2}
          />
        </Form.Item>

        <Form.Item
          name="datasourceIds"
          label={t('management.clusters.datasourceIds')}
          rules={[{ required: true }]}
        >
          <Select
            mode="multiple"
            placeholder={t('management.clusters.selectDatasources')}
            options={datasourceOptions}
          />
        </Form.Item>

        <Form.Item style={{ marginBottom: 0 }}>
          <Space>
            <Button type="primary" htmlType="submit" loading={loading}>
              {t('management.clusters.confirm')}
            </Button>
            <Button onClick={() => { form.resetFields(); onClose(); }}>
              {t('management.clusters.cancel')}
            </Button>
          </Space>
        </Form.Item>
      </Form>
    </Modal>
  );
}
