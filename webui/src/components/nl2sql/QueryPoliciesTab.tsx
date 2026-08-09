import { useEffect, useMemo, useState } from 'react';
import {
  Table, Button, Modal, Form, Input, Select, Space,
  Tag, message, Popconfirm, Typography, Card, Empty, Tooltip, Alert,
} from 'antd';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { PlusOutlined, DeleteOutlined, EditOutlined, LockOutlined } from '@ant-design/icons';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { ApiError } from '@/api/errors';
import type { Nl2sqlQueryPolicy } from '@/types';

const { Text } = Typography;

interface PolicyFormValues {
  datasource_id: string;
  user_id: string;
  allowed_tables: string[];
  denied_tables: string[];
  allowed_columns: string[];
  denied_columns: string[];
  row_filter_expr?: string;
  description?: string;
}

export function QueryPoliciesTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [modalOpen, setModalOpen] = useState(false);
  const [editingPolicy, setEditingPolicy] = useState<Nl2sqlQueryPolicy | null>(null);
  const [selectedDsId, setSelectedDsId] = useState<string | undefined>(undefined);
  const [form] = Form.useForm<PolicyFormValues>();

  const showPolicyError = (err: unknown) => {
    const apiErr = err instanceof ApiError ? err : null;
    const rawMsg = (apiErr?.message ?? (err as { message?: string })?.message ?? '').trim();
    const lowerMsg = rawMsg.toLowerCase();
    const isDuplicate =
      apiErr?.status === 409
      || lowerMsg.includes('already exists')
      || lowerMsg.includes('duplicate entry')
      || lowerMsg.includes('uk_tenant_ds_user');

    if (isDuplicate) {
      message.error(t('management.queryPolicies.duplicatePolicy'));
      return;
    }

    if (apiErr?.status === 500 && lowerMsg === 'database error') {
      message.error(t('management.queryPolicies.databaseError'));
      return;
    }

    if (rawMsg) {
      message.error(rawMsg);
      return;
    }

    message.error(t('common.failed'));
  };

  const { data: dsData, isLoading: dsLoading } = useQuery({
    queryKey: queryKeys.dataSources.all(),
    queryFn: () => dataSourcesApi.list(),
    staleTime: 60_000,
  });

  const { data: policiesData, isLoading: policiesLoading } = useQuery({
    queryKey: [...queryKeys.nl2sql.queryPolicies(), selectedDsId ?? ''],
    queryFn: () => nl2sqlApi.listQueryPolicies(),
    enabled: !!selectedDsId,
    staleTime: 30_000,
  });

  const createMutation = useMutation({
    mutationFn: (values: PolicyFormValues) => nl2sqlApi.createQueryPolicy({
      datasource_id: values.datasource_id,
      user_id: values.user_id,
      allowed_tables: values.allowed_tables,
      denied_tables: values.denied_tables,
      allowed_columns: values.allowed_columns,
      denied_columns: values.denied_columns,
      row_filter_expr: values.row_filter_expr,
      description: values.description,
    }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.queryPolicies() });
      message.success(t('management.queryPolicies.createSuccess'));
      closeModal();
    },
    onError: (err: unknown) => {
      showPolicyError(err);
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({ id, values }: { id: number; values: Partial<PolicyFormValues> }) =>
      nl2sqlApi.updateQueryPolicy(id, {
        user_id: values.user_id,
        allowed_tables: values.allowed_tables,
        denied_tables: values.denied_tables,
        allowed_columns: values.allowed_columns,
        denied_columns: values.denied_columns,
        row_filter_expr: values.row_filter_expr,
        description: values.description,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.queryPolicies() });
      message.success(t('management.queryPolicies.updateSuccess'));
      closeModal();
    },
    onError: (err: unknown) => {
      showPolicyError(err);
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: number) => nl2sqlApi.deleteQueryPolicy(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.queryPolicies() });
      message.success(t('management.queryPolicies.deleteSuccess'));
    },
    onError: (err: unknown) => {
      showPolicyError(err);
    },
  });

  const closeModal = () => {
    setModalOpen(false);
    setEditingPolicy(null);
    form.resetFields();
  };

  const openCreate = () => {
    if (!selectedDsId) {
      message.warning(t('management.queryPolicies.selectDatasource'));
      return;
    }
    setEditingPolicy(null);
    form.setFieldsValue({ datasource_id: selectedDsId });
    setModalOpen(true);
  };

  const openEdit = (policy: Nl2sqlQueryPolicy) => {
    setEditingPolicy(policy);
    form.setFieldsValue({
      datasource_id: policy.datasource_id,
      user_id: policy.user_email || policy.user_id,
      allowed_tables: policy.allowed_tables,
      denied_tables: policy.denied_tables,
      allowed_columns: policy.allowed_columns,
      denied_columns: policy.denied_columns,
      row_filter_expr: policy.row_filter_expr ?? undefined,
      description: policy.description ?? undefined,
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

  const dsOptions = useMemo(() => (dsData?.data_sources ?? []).map(ds => ({
    label: ds.name,
    value: ds.id,
  })), [dsData?.data_sources]);
  useEffect(() => {
    if (!selectedDsId && dsOptions.length > 0) {
      setSelectedDsId(dsOptions[0].value);
    }
  }, [dsOptions, selectedDsId]);

  const modalDatasourceId = Form.useWatch('datasource_id', form) || selectedDsId;
  const selectedDatasource = (dsData?.data_sources ?? []).find(
    (item) => item.id === modalDatasourceId,
  );
  const schemaTables = useMemo(() => {
    const tables = selectedDatasource?.schema_info?.tables;
    return Array.isArray(tables)
      ? tables.map((table) => table.table_name || table.name).filter((name): name is string => !!name)
      : [];
  }, [selectedDatasource]);
  const tableOptions = schemaTables.map((table) => ({ label: table, value: table }));

  const columns = [
    {
      title: t('management.queryPolicies.user'),
      dataIndex: 'user_id',
      width: 140,
      render: (_: unknown, record: Nl2sqlQueryPolicy) => {
        const displayName = record.user_name || record.user_email || record.user_id;
        return <Tag color="blue">{displayName}</Tag>;
      },
    },
    {
      title: t('management.queryPolicies.allowedTables'),
      dataIndex: 'allowed_tables',
      render: (arr: string[]) => arr.length === 0
        ? <Text type="secondary">*</Text>
        : arr.slice(0, 3).map(tbl => <Tag key={tbl} color="green">{tbl}</Tag>),
    },
    {
      title: t('management.queryPolicies.deniedTables'),
      dataIndex: 'denied_tables',
      render: (arr: string[]) => arr.length === 0
        ? <Text type="secondary">-</Text>
        : arr.slice(0, 3).map(tbl => <Tag key={tbl} color="red">{tbl}</Tag>),
    },
    {
      title: t('management.queryPolicies.rowFilter'),
      dataIndex: 'row_filter_expr',
      ellipsis: { showTitle: false },
      width: 180,
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
      title: '',
      render: (_: unknown, record: Nl2sqlQueryPolicy) => (
        <Space size={4}>
          <Button size="small" icon={<EditOutlined />} onClick={() => openEdit(record)} />
          <Popconfirm
            title={t('management.queryPolicies.deleteConfirm')}
            onConfirm={() => deleteMutation.mutate(record.id)}
          >
            <Button size="small" danger icon={<DeleteOutlined />} />
          </Popconfirm>
        </Space>
      ),
      width: 90,
    },
  ];

  const visiblePolicies = (policiesData?.items ?? []).filter((item) => item.datasource_id === selectedDsId);

  return (
    <div>
      <Card size="small" style={{ marginBottom: 12, background: 'var(--bg-secondary)' }}>
        <Space size={12} wrap>
          <LockOutlined style={{ color: 'var(--text-secondary)' }} />
          <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
            {t('management.queryPolicies.hint')}
          </Text>
        </Space>
      </Card>

      <div style={{ marginBottom: 12, display: 'flex', gap: 12, alignItems: 'center', flexWrap: 'wrap' }}>
        <Select
          allowClear
          showSearch
          style={{ minWidth: 220 }}
          placeholder={t('management.queryPolicies.selectDatasource')}
          optionFilterProp="label"
          loading={dsLoading}
          value={selectedDsId}
          onChange={val => setSelectedDsId(val)}
          options={dsOptions}
        />
        <Button
          type="primary"
          icon={<PlusOutlined />}
          onClick={openCreate}
          disabled={!selectedDsId}
        >
          {t('management.queryPolicies.newPolicy')}
        </Button>
      </div>

      <Table
        columns={columns}
        dataSource={visiblePolicies}
        rowKey="id"
        loading={policiesLoading}
        pagination={false}
        size="small"
        locale={visiblePolicies.length === 0 ? {
          emptyText: (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description={!selectedDsId
                ? t('management.queryPolicies.selectDatasource')
                : t('management.queryPolicies.noPolicies')
              }
            />
          ),
        } : undefined}
      />

      <Modal
        open={modalOpen}
        title={editingPolicy
          ? t('management.queryPolicies.editPolicy')
          : t('management.queryPolicies.newPolicy')}
        onCancel={closeModal}
        footer={null}
        width={560}
      >
        <Form
          form={form}
          layout="vertical"
          onFinish={handleFinish}
          initialValues={{
            allowed_tables: [],
            denied_tables: [],
            allowed_columns: [],
            denied_columns: [],
          }}
        >
          {modalDatasourceId && schemaTables.length === 0 && (
            <Alert
              type="info"
              showIcon
              style={{ marginBottom: 16 }}
              message={t('management.queryPolicies.tableLoadFallback')}
            />
          )}
          <Form.Item
            name="datasource_id"
            label={t('management.queryPolicies.datasource')}
            rules={[{ required: true }]}
          >
            <Select
              showSearch
              optionFilterProp="label"
              disabled={!!editingPolicy}
              placeholder={t('management.queryPolicies.selectDatasource')}
              options={dsOptions}
            />
          </Form.Item>

          <Form.Item
            name="user_id"
            label={t('management.queryPolicies.user')}
            rules={[{ required: true }]}
          >
            <Input placeholder={t('management.queryPolicies.userPlaceholder')} />
          </Form.Item>

          <Form.Item
            name="allowed_tables"
            label={t('management.queryPolicies.allowedTables')}
            extra={t('management.queryPolicies.tableHint')}
          >
            <Select
              mode="tags"
              options={tableOptions}
              showSearch
              optionFilterProp="label"
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
              options={tableOptions}
              showSearch
              optionFilterProp="label"
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
            label={t('management.queryPolicies.description')}
          >
            <Input.TextArea
              placeholder={t('management.queryPolicies.descPlaceholder')}
              rows={2}
            />
          </Form.Item>

          <Form.Item style={{ marginBottom: 0 }}>
            <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
              <Button onClick={closeModal}>{t('common.cancel')}</Button>
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
