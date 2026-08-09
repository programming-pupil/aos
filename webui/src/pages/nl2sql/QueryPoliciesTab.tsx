import React, { useState } from 'react';
import {
  Table, Tag, Button, Space, Modal, Form, Input,
  message, Popconfirm, Select, Switch, Card, Typography, Tooltip,
} from 'antd';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { PlusOutlined, DeleteOutlined, EditOutlined, LockOutlined } from '@ant-design/icons';
import { nl2sqlApi, dataSourcesApi, usersApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { Nl2sqlQueryPolicy } from '@/types';

const { Text } = Typography;

interface PolicyFormValues {
  datasource_id: string;
  user_id: string;
  allowed_tables: string[];
  denied_tables: string[];
  allowed_columns: string[];
  denied_columns: string[];
  row_filter_expr?: string | null;
  description?: string | null;
  enabled: boolean;
}

export function QueryPoliciesTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [modalOpen, setModalOpen] = useState(false);
  const [editingPolicy, setEditingPolicy] = useState<Nl2sqlQueryPolicy | null>(null);
  const [form] = Form.useForm<PolicyFormValues>();
  const [page, setPage] = useState(1);
  const pageSize = 20;

  const { data, isLoading } = useQuery({
    queryKey: queryKeys.nl2sql.queryPolicies(page, pageSize),
    queryFn: () => nl2sqlApi.listQueryPolicies({ page, per_page: pageSize }),
    enabled: true,
  });

  const { data: dataSourcesData } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list(),
    staleTime: 60_000,
  });

  const { data: usersData } = useQuery({
    queryKey: queryKeys.users.list(),
    queryFn: () => usersApi.list(),
    staleTime: 60_000,
  });

  const createMutation = useMutation({
    mutationFn: (values: PolicyFormValues) => nl2sqlApi.createQueryPolicy({
      datasource_id: values.datasource_id,
      user_id: values.user_id,
      allowed_tables: values.allowed_tables,
      denied_tables: values.denied_tables,
      allowed_columns: values.allowed_columns,
      denied_columns: values.denied_columns,
      row_filter_expr: values.row_filter_expr ?? undefined,
      description: values.description ?? undefined,
      enabled: values.enabled,
    }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.queryPolicies() });
      message.success(t('management.queryPolicies.createSuccess'));
      setModalOpen(false);
      form.resetFields();
    },
    onError: (err) => {
      const msg = err && typeof err === 'object' && 'message' in err ? String((err as { message: unknown }).message) : String(err);
      message.error(msg);
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({ id, values }: { id: number; values: Partial<PolicyFormValues> }) =>
      nl2sqlApi.updateQueryPolicy(id, {
        allowed_tables: values.allowed_tables,
        denied_tables: values.denied_tables,
        allowed_columns: values.allowed_columns,
        denied_columns: values.denied_columns,
        row_filter_expr: values.row_filter_expr ?? undefined,
        description: values.description ?? undefined,
        enabled: values.enabled,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.queryPolicies() });
      message.success(t('management.queryPolicies.updateSuccess'));
      setModalOpen(false);
      setEditingPolicy(null);
      form.resetFields();
    },
    onError: (err) => {
      const msg = err && typeof err === 'object' && 'message' in err ? String((err as { message: unknown }).message) : String(err);
      message.error(msg);
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => nl2sqlApi.deleteQueryPolicy(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.queryPolicies() });
      message.success(t('management.queryPolicies.deleteSuccess'));
    },
    onError: (err) => {
      const msg = err && typeof err === 'object' && 'message' in err ? String((err as { message: unknown }).message) : String(err);
      message.error(msg);
    },
  });

  const openEdit = (policy: Nl2sqlQueryPolicy) => {
    setEditingPolicy(policy);
    form.setFieldsValue({
      datasource_id: policy.datasource_id,
      user_id: policy.user_id,
      allowed_tables: policy.allowed_tables,
      denied_tables: policy.denied_tables,
      allowed_columns: policy.allowed_columns,
      denied_columns: policy.denied_columns,
      row_filter_expr: policy.row_filter_expr,
      description: policy.description,
      enabled: policy.enabled,
    });
    setModalOpen(true);
  };

  const handleFinish = (values: PolicyFormValues) => {
    if (editingPolicy) {
      updateMutation.mutate({ id: editingPolicy.id, values });
    } else {
      createMutation.mutate(values);
    }
  };

  const dsMap = Object.fromEntries(
    (dataSourcesData?.data_sources ?? []).map(ds => [ds.id, ds.name])
  );

  const columns = [
    {
      title: t('management.queryPolicies.datasource'),
      dataIndex: 'datasource_id',
      render: (id: string) => dsMap[id] ?? id,
      width: 160,
    },
    {
      title: t('management.queryPolicies.user'),
      dataIndex: 'user_id',
      width: 180,
      render: (_id: string, record: Nl2sqlQueryPolicy) => {
        const label = record.user_name
          ? `${record.user_name} (${record.user_email ?? record.user_id})`
          : (record.user_email ?? record.user_id);
        return <Tag color="blue">{label}</Tag>;
      },
    },
    {
      title: t('management.queryPolicies.allowedTables'),
      dataIndex: 'allowed_tables',
      render: (arr: string[]) => arr.length === 0
        ? <Text type="secondary">*</Text>
        : arr.map(t => <Tag key={t} color="green">{t}</Tag>).slice(0, 3),
    },
    {
      title: t('management.queryPolicies.deniedTables'),
      dataIndex: 'denied_tables',
      render: (arr: string[]) => arr.length === 0
        ? <Text type="secondary">-</Text>
        : arr.map(t => <Tag key={t} color="red">{t}</Tag>).slice(0, 3),
    },
    {
      title: t('management.queryPolicies.allowedColumns'),
      dataIndex: 'allowed_columns',
      width: 150,
      render: (arr: string[]) => arr.length === 0
        ? <Text type="secondary">*</Text>
        : arr.map(t => <Tag key={t} color="purple">{t}</Tag>).slice(0, 3),
    },
    {
      title: t('management.queryPolicies.deniedColumns'),
      dataIndex: 'denied_columns',
      width: 150,
      render: (arr: string[]) => arr.length === 0
        ? <Text type="secondary">-</Text>
        : arr.map(t => <Tag key={t} color="volcano">{t}</Tag>).slice(0, 3),
    },
    {
      title: t('management.queryPolicies.rowFilter'),
      dataIndex: 'row_filter_expr',
      ellipsis: true,
      render: (expr: string | null) => expr
        ? <Tooltip title={expr}><Text code style={{ fontSize: 11 }}>{expr}</Text></Tooltip>
        : <Text type="secondary">-</Text>,
    },
    {
      title: t('management.queryPolicies.description'),
      dataIndex: 'description',
      ellipsis: true,
      render: (d: string | null) => d ?? '-',
    },
    {
      title: t('common.enabled'),
      dataIndex: 'enabled',
      render: (enabled: boolean) => <Switch checked={enabled} size="small" disabled />,
      width: 80,
    },
    {
      title: t('common.actions'),
      render: (_: unknown, record: Nl2sqlQueryPolicy) => (
        <Space>
          <Button size="small" icon={<EditOutlined />} onClick={() => openEdit(record)} />
          <Popconfirm
            title={t('management.queryPolicies.deleteConfirm')}
            onConfirm={() => deleteMutation.mutate(record.id)}
          >
            <Button size="small" danger icon={<DeleteOutlined />} />
          </Popconfirm>
        </Space>
      ),
      width: 100,
    },
  ];

  return (
    <div>
      {/* Hint */}
      <Card size="small" style={{ marginBottom: 16, background: 'var(--bg-secondary)' }}>
        <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
          <LockOutlined style={{ marginRight: 6 }} />
          {t('management.queryPolicies.hint')}
        </Text>
      </Card>

      {/* Actions */}
      <div style={{ marginBottom: 12, display: 'flex', justifyContent: 'flex-end' }}>
        <Button
          type="primary"
          icon={<PlusOutlined />}
          onClick={() => { setEditingPolicy(null); form.resetFields(); setModalOpen(true); }}
        >
          {t('management.queryPolicies.newPolicy')}
        </Button>
      </div>

      {/* Table */}
      <Table
        columns={columns}
        dataSource={data?.items ?? []}
        rowKey="id"
        loading={isLoading}
        pagination={{
          current: page,
          pageSize,
          total: data?.total ?? 0,
          onChange: setPage,
          showSizeChanger: false,
        }}
        size="small"
      />

      {/* Create/Edit Modal */}
      <Modal
        open={modalOpen}
        title={editingPolicy
          ? (t('management.queryPolicies.editPolicy'))
          : (t('management.queryPolicies.newPolicy'))}
        onCancel={() => { setModalOpen(false); setEditingPolicy(null); form.resetFields(); }}
        footer={null}
        width={600}
      >
        <Form
          form={form}
          layout="vertical"
          onFinish={handleFinish}
          initialValues={{ enabled: true, allowed_tables: [], denied_tables: [], allowed_columns: [], denied_columns: [] }}
        >
          <Form.Item
            name="datasource_id"
            label={t('management.queryPolicies.datasource')}
            rules={[{ required: true }]}
          >
            <Select
              showSearch
              placeholder={t('management.queryPolicies.selectDatasource')}
              optionFilterProp="label"
              options={dataSourcesData?.data_sources.map(ds => ({
                label: ds.name,
                value: ds.id,
              })) ?? []}
            />
          </Form.Item>

          <Form.Item
            name="user_id"
            label={t('management.queryPolicies.user')}
            rules={[{ required: true }]}
          >
            <Select
              showSearch
              allowClear
              placeholder={t('management.queryPolicies.selectUserPlaceholder')}
              optionFilterProp="label"
              options={usersData?.users?.map(u => ({
                label: u.name ? `${u.name} (${u.email})` : u.email,
                value: u.id,
              })) ?? []}
            />
          </Form.Item>

          <Form.Item
            name="allowed_tables"
            label={t('management.queryPolicies.allowedTables')}
            extra={t('management.queryPolicies.tableHint')}
          >
            <Select
              mode="tags"
              placeholder={t('management.queryPolicies.tablePlaceholder')}
              tokenSeparators={[',', ';']}
            />
          </Form.Item>

          <Form.Item
            name="denied_tables"
            label={t('management.queryPolicies.deniedTables')}
            extra={t('management.queryPolicies.denyHint')}
          >
            <Select
              mode="tags"
              placeholder={t('management.queryPolicies.tablePlaceholder')}
              tokenSeparators={[',', ';']}
            />
          </Form.Item>

          <Form.Item
            name="allowed_columns"
            label={t('management.queryPolicies.allowedColumns')}
            extra={t('management.queryPolicies.columnHint')}
          >
            <Select
              mode="tags"
              placeholder={t('management.queryPolicies.columnPlaceholder')}
              tokenSeparators={[',', ';']}
            />
          </Form.Item>

          <Form.Item
            name="denied_columns"
            label={t('management.queryPolicies.deniedColumns')}
            extra={t('management.queryPolicies.denyColumnHint')}
          >
            <Select
              mode="tags"
              placeholder={t('management.queryPolicies.columnPlaceholder')}
              tokenSeparators={[',', ';']}
            />
          </Form.Item>

          <Form.Item
            name="row_filter_expr"
            label={t('management.queryPolicies.rowFilter')}
            extra={t('management.queryPolicies.rowFilterHint')}
          >
            <Input placeholder={t('management.queryPolicies.rowFilterPlaceholder')} />
          </Form.Item>

          <Form.Item
            name="description"
            label={t('common.description')}
          >
            <Input.TextArea placeholder={t('management.queryPolicies.descPlaceholder')} rows={2} />
          </Form.Item>

          <Form.Item
            name="enabled"
            label={t('common.status')}
            valuePropName="checked"
          >
            <Switch />
          </Form.Item>

          <Form.Item style={{ marginBottom: 0 }}>
            <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
              <Button onClick={() => { setModalOpen(false); setEditingPolicy(null); }}>
                {t('common.cancel')}
              </Button>
              <Button
                type="primary"
                htmlType="submit"
                loading={createMutation.isPending || updateMutation.isPending}
              >
                {editingPolicy ? t('common.save') : t('common.create')}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
