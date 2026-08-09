// Tenant-wide column masking rules (R-7 / R-8).
//
// Mirrors the ValidationRulesTab shape: TanStack Query + Ant Design Table +
// Modal-based create/edit. All copy goes through useTranslation against
// `management.maskingRules.*` to stay i18n-clean.

import { useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Form,
  Input,
  InputNumber,
  Modal,
  Popconfirm,
  Select,
  Space,
  Switch,
  Table,
  Tag,
  Typography,
  message,
} from 'antd';
import { PlusOutlined, EditOutlined, DeleteOutlined, SafetyOutlined } from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  MaskingRule,
  CreateMaskingRuleRequest,
  UpdateMaskingRuleRequest,
  MaskingRuleType,
} from '@/types';

const { Text } = Typography;

const MASK_TYPES: MaskingRuleType[] = [
  'redact',
  'null',
  'constant',
  'hash',
  'tokenize',
  'partial',
];

export function MaskingRulesTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [editing, setEditing] = useState<MaskingRule | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  const [form] = Form.useForm<CreateMaskingRuleRequest>();

  const { data: rulesResp, isLoading } = useQuery({
    queryKey: queryKeys.nl2sql.maskingRules(),
    queryFn: () => nl2sqlApi.listMaskingRules(),
  });
  const rules: MaskingRule[] = rulesResp?.rules ?? [];

  const { data: dsResp } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list(),
  });
  const datasources = useMemo(
    () => (Array.isArray(dsResp) ? dsResp : dsResp?.data_sources ?? []),
    [dsResp]
  );

  const createMu = useMutation({
    mutationFn: (data: CreateMaskingRuleRequest) => nl2sqlApi.createMaskingRule(data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.maskingRules() });
      message.success(t('management.maskingRules.createSuccess'));
      setModalOpen(false);
      form.resetFields();
    },
  });

  const updateMu = useMutation({
    mutationFn: ({ id, data }: { id: number; data: UpdateMaskingRuleRequest }) =>
      nl2sqlApi.updateMaskingRule(id, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.maskingRules() });
      message.success(t('management.maskingRules.updateSuccess'));
      setModalOpen(false);
      setEditing(null);
      form.resetFields();
    },
  });

  const deleteMu = useMutation({
    mutationFn: (id: number) => nl2sqlApi.deleteMaskingRule(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.maskingRules() });
      message.success(t('management.maskingRules.deleteSuccess'));
    },
  });

  const openCreate = () => {
    setEditing(null);
    form.resetFields();
    form.setFieldsValue({
      mask_type: 'redact',
      priority: 100,
      enabled: true,
      table_name: '%',
      column_name: '',
    } as CreateMaskingRuleRequest);
    setModalOpen(true);
  };

  const openEdit = (rule: MaskingRule) => {
    setEditing(rule);
    form.setFieldsValue({
      datasource_id: rule.datasource_id,
      table_name: rule.table_name,
      column_name: rule.column_name,
      mask_type: rule.mask_type,
      constant_value: rule.constant_value,
      priority: rule.priority,
      description: rule.description,
      enabled: rule.enabled,
    });
    setModalOpen(true);
  };

  const onSubmit = async () => {
    const values = await form.validateFields();
    // Coerce empty datasource_id (string '') to null so the backend treats it
    // as a tenant-wide rule.
    const payload: CreateMaskingRuleRequest = {
      ...values,
      datasource_id: values.datasource_id || null,
    };
    if (editing) {
      updateMu.mutate({ id: editing.id, data: payload });
    } else {
      createMu.mutate(payload);
    }
  };

  return (
    <div>
      <Alert
        showIcon
        type="info"
        icon={<SafetyOutlined />}
        message={t('management.maskingRules.pageTitle')}
        description={
          <>
            <div>{t('management.maskingRules.hint')}</div>
            <div style={{ marginTop: 4, color: 'var(--text-tertiary)' }}>
              {t('management.maskingRules.wildcardHint')}
            </div>
          </>
        }
        style={{ marginBottom: 16 }}
      />
      <Space style={{ marginBottom: 12 }}>
        <Button type="primary" icon={<PlusOutlined />} onClick={openCreate}>
          {t('management.maskingRules.newRule')}
        </Button>
      </Space>
      <Table<MaskingRule>
        rowKey="id"
        loading={isLoading}
        dataSource={rules}
        pagination={{ pageSize: 20, showSizeChanger: true }}
        columns={[
          {
            title: t('management.maskingRules.priority'),
            dataIndex: 'priority',
            width: 90,
            sorter: (a, b) => a.priority - b.priority,
          },
          {
            title: t('management.maskingRules.datasource'),
            dataIndex: 'datasource_id',
            render: (v: string | null) => {
              if (!v) {
                return <Tag>{t('management.maskingRules.datasourceAllPlaceholder')}</Tag>;
              }
              const ds = datasources.find((d) => d.id === v);
              return ds ? ds.name : v;
            },
          },
          {
            title: t('management.maskingRules.tableName'),
            dataIndex: 'table_name',
            render: (v: string) => <Text code>{v}</Text>,
          },
          {
            title: t('management.maskingRules.columnName'),
            dataIndex: 'column_name',
            render: (v: string) => <Text code>{v}</Text>,
          },
          {
            title: t('management.maskingRules.maskType'),
            dataIndex: 'mask_type',
            render: (v: MaskingRuleType) => <Tag color="orange">{v}</Tag>,
          },
          {
            title: t('management.maskingRules.enabled'),
            dataIndex: 'enabled',
            width: 90,
            render: (v: boolean) =>
              v
                ? <Tag color="green">{t('management.maskingRules.statusOn')}</Tag>
                : <Tag>{t('management.maskingRules.statusOff')}</Tag>,
          },
          {
            title: t('common.actions'),
            key: 'actions',
            width: 160,
            render: (_, record) => (
              <Space>
                <Button
                  size="small"
                  icon={<EditOutlined />}
                  onClick={() => openEdit(record)}
                />
                <Popconfirm
                  title={t('management.maskingRules.deleteConfirm')}
                  onConfirm={() => deleteMu.mutate(record.id)}
                  okButtonProps={{ danger: true }}
                >
                  <Button size="small" danger icon={<DeleteOutlined />} />
                </Popconfirm>
              </Space>
            ),
          },
        ]}
      />

      <Modal
        open={modalOpen}
        title={
          editing
            ? t('management.maskingRules.editRule')
            : t('management.maskingRules.newRule')
        }
        onCancel={() => {
          setModalOpen(false);
          setEditing(null);
          form.resetFields();
        }}
        onOk={onSubmit}
        confirmLoading={createMu.isPending || updateMu.isPending}
        destroyOnHidden
      >
        <Form form={form} layout="vertical">
          <Form.Item
            label={t('management.maskingRules.datasource')}
            name="datasource_id"
          >
            <Select
              allowClear
              placeholder={t('management.maskingRules.datasourceAllPlaceholder')}
              options={datasources.map((d) => ({ label: d.name, value: d.id }))}
            />
          </Form.Item>
          <Form.Item
            label={t('management.maskingRules.tableName')}
            name="table_name"
            rules={[{ required: true }]}
          >
            <Input placeholder={t('management.maskingRules.tablePlaceholder')} />
          </Form.Item>
          <Form.Item
            label={t('management.maskingRules.columnName')}
            name="column_name"
            rules={[{ required: true }]}
          >
            <Input placeholder={t('management.maskingRules.columnPlaceholder')} />
          </Form.Item>
          <Form.Item
            label={t('management.maskingRules.maskType')}
            name="mask_type"
            rules={[{ required: true }]}
          >
            <Select
              options={MASK_TYPES.map((m) => ({
                value: m,
                label: t(
                  `management.maskingRules.maskType${m.charAt(0).toUpperCase()}${m.slice(1)}`
                ),
              }))}
            />
          </Form.Item>
          <Form.Item
            noStyle
            shouldUpdate={(prev, curr) => prev.mask_type !== curr.mask_type}
          >
            {({ getFieldValue }) =>
              getFieldValue('mask_type') === 'constant' ? (
                <Form.Item
                  label={t('management.maskingRules.constantValue')}
                  name="constant_value"
                  rules={[{ required: true }]}
                >
                  <Input />
                </Form.Item>
              ) : null
            }
          </Form.Item>
          <Form.Item
            label={t('management.maskingRules.priority')}
            name="priority"
            tooltip={t('management.maskingRules.priorityHint')}
          >
            <InputNumber min={1} max={10000} style={{ width: '100%' }} />
          </Form.Item>
          <Form.Item
            label={t('management.maskingRules.description')}
            name="description"
          >
            <Input.TextArea rows={2} />
          </Form.Item>
          <Form.Item
            label={t('management.maskingRules.enabled')}
            name="enabled"
            valuePropName="checked"
          >
            <Switch />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
