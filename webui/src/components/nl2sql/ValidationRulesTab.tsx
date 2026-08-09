import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Form, Input, Table, Select, Tag, Button, Space, Modal, message,
  Popconfirm, Typography, Card, Spin, Empty, Switch, InputNumber,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined,
  SafetyOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  ValidationRule,
  CreateValidationRuleRequest,
  UpdateValidationRuleRequest,
  ListValidationRulesResponse,
} from '@/types';

const { Text } = Typography;

function SeverityTag({ severity }: { severity: string }) {
  const { t } = useTranslation();
  return severity === 'error'
    ? <Tag color="red">{t('management.validationRules.error')}</Tag>
    : <Tag color="orange">{t('management.validationRules.warning')}</Tag>;
}

const RULE_TYPES = (t: (key: string) => string) => [
  { label: t('management.validationRules.ruleTypes.range'), value: 'range' },
  { label: t('management.validationRules.ruleTypes.nullRatio'), value: 'null_ratio' },
  { label: t('management.validationRules.ruleTypes.rowCount'), value: 'row_count' },
  { label: t('management.validationRules.ruleTypes.freshness'), value: 'freshness' },
  { label: t('management.validationRules.ruleTypes.cardinality'), value: 'cardinality' },
];

function buildConfig(type: string): Record<string, unknown> {
  switch (type) {
    case 'range': return { min: 0 };
    case 'null_ratio': return { max_pct: 80 };
    case 'row_count': return { min: 1, max: 1000000 };
    case 'freshness': return { max_staleness_hours: 24 };
    case 'cardinality': return { distinct_min: 1 };
    default: return {};
  }
}

function normalizeRuleConfig(
  type: string,
  cfg: Record<string, unknown> | undefined,
): Record<string, unknown> {
  return {
    ...buildConfig(type),
    ...(cfg ?? {}),
  };
}

function configToDisplay(
  t: (key: string) => string,
  type: string,
  cfg: Record<string, unknown>,
): string {
  switch (type) {
    case 'range': return `${t('management.validationRules.configRange')}: ${cfg.min ?? '?'} ~ ${cfg.max ?? t('management.validationRules.unlimited')}`;
    case 'null_ratio': return `${t('management.validationRules.configMaxNull')}: ${cfg.max_pct ?? 80}%`;
    case 'row_count': return `${t('management.validationRules.configRows')}: ${cfg.min ?? 1} ~ ${cfg.max ?? t('management.validationRules.unlimited')}`;
    case 'freshness': return `${t('management.validationRules.configMaxStaleness')}: ${cfg.max_staleness_hours ?? 24}h`;
    case 'cardinality': return `${t('management.validationRules.configMinDistinct')}: ${cfg.distinct_min ?? 1}`;
    default: return JSON.stringify(cfg);
  }
}

export function ValidationRulesTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [selectedDs, setSelectedDs] = useState<string | undefined>(undefined);
  const [createOpen, setCreateOpen] = useState(false);
  const [editRule, setEditRule] = useState<ValidationRule | null>(null);
  const [form] = Form.useForm();
  const [editForm] = Form.useForm();
  const createRuleType = Form.useWatch('ruleType', form);
  const editRuleType = Form.useWatch('ruleType', editForm);

  useEffect(() => {
    if (!editRule) {
      editForm.resetFields();
      return;
    }
    editForm.resetFields();
    editForm.setFieldsValue({
      tableName: editRule.tableName,
      columnName: editRule.columnName,
      ruleType: editRule.ruleType,
      severity: editRule.severity,
      description: editRule.description,
      ruleConfig: normalizeRuleConfig(editRule.ruleType, editRule.ruleConfig),
    });
  }, [editRule, editForm]);

  const closeEditModal = () => {
    setEditRule(null);
    editForm.resetFields();
  };

  const { data: dsList, isLoading: dsLoading } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list({ per_page: 200 }),
  });

  useEffect(() => {
    const firstDatasourceId = dsList?.data_sources?.[0]?.id;
    if (!selectedDs && firstDatasourceId) {
      setSelectedDs(firstDatasourceId);
    }
  }, [dsList?.data_sources, selectedDs]);

  const datasourceId = selectedDs ?? '';

  const { data, isLoading } = useQuery<ListValidationRulesResponse>({
    queryKey: queryKeys.nl2sql.validationRules(datasourceId),
    queryFn: () => nl2sqlApi.listValidationRules(datasourceId),
    enabled: !!datasourceId,
  });

  const create = useMutation({
    mutationFn: (payload: CreateValidationRuleRequest) =>
      nl2sqlApi.createValidationRule(datasourceId, payload),
    onSuccess: () => {
      message.success(t('management.validationRules.createSuccess'));
      setCreateOpen(false);
      form.resetFields();
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.validationRules(datasourceId) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const updateMut = useMutation({
    mutationFn: ({ ruleId, data: vals }: { ruleId: number; data: UpdateValidationRuleRequest }) =>
      nl2sqlApi.updateValidationRule(datasourceId, ruleId, vals),
    onSuccess: () => {
      message.success(t('management.validationRules.updateSuccess'));
      closeEditModal();
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.validationRules(datasourceId) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const deleteMut = useMutation({
    mutationFn: (ruleId: number) => nl2sqlApi.deleteValidationRule(datasourceId, ruleId),
    onSuccess: () => {
      message.success(t('management.validationRules.deleteSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.validationRules(datasourceId) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const toggle = useMutation({
    mutationFn: ({ ruleId, enabled }: { ruleId: number; enabled: boolean }) =>
      nl2sqlApi.updateValidationRule(datasourceId, ruleId, { enabled }),
    onSuccess: () => {
      message.success(t('common.success'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.validationRules(datasourceId) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const columns = [
    {
      title: t('management.validationRules.enabled'),
      key: 'enabled',
      width: 80,
      render: (_: unknown, record: ValidationRule) => (
        <Switch
          size="small"
          checked={record.enabled}
          onChange={(checked) => toggle.mutate({ ruleId: record.id, enabled: checked })}
        />
      ),
    },
    {
      title: t('management.validationRules.severity'),
      dataIndex: 'severity',
      key: 'severity',
      width: 90,
      render: (s: string) => <SeverityTag severity={s} />,
    },
    {
      title: t('management.validationRules.tableName'),
      dataIndex: 'tableName',
      key: 'tableName',
      width: 140,
      render: (v: string) => (
        <Tag icon={<SafetyOutlined />} color="default">{v}</Tag>
      ),
    },
    {
      title: t('management.validationRules.columnName'),
      dataIndex: 'columnName',
      key: 'columnName',
      width: 140,
      render: (v: string) => <Text code style={{ fontSize: 12 }}>{v}</Text>,
    },
    {
      title: t('management.validationRules.ruleType'),
      dataIndex: 'ruleType',
      key: 'ruleType',
      width: 140,
      render: (v: string) => {
        const m = RULE_TYPES(t).find((r) => r.value === v);
        return <Text>{m?.label ?? v}</Text>;
      },
    },
    {
      title: t('management.validationRules.config'),
      dataIndex: 'ruleConfig',
      key: 'ruleConfig',
      ellipsis: true,
      render: (cfg: Record<string, unknown>, record: ValidationRule) => (
        <Text type="secondary" style={{ fontSize: 12 }}>
          {configToDisplay(t, record.ruleType, cfg)}
        </Text>
      ),
    },
    {
      title: t('management.validationRules.description'),
      dataIndex: 'description',
      key: 'description',
      ellipsis: true,
      render: (d: string) => d ? <Text type="secondary">{d}</Text> : <Text type="secondary">-</Text>,
    },
    {
      title: '',
      key: 'actions',
      width: 100,
      render: (_: unknown, record: ValidationRule) => (
        <Space size="small">
          <Button
            size="small"
            icon={<EditOutlined />}
            onClick={() => setEditRule(record)}
          />
          <Popconfirm
            title={t('management.validationRules.deleteConfirm')}
            onConfirm={() => deleteMut.mutate(record.id)}
          >
            <Button size="small" danger icon={<DeleteOutlined />} />
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const rules = data?.rules ?? [];

  return (
    <Card
      title={t('management.validationRules.newRule')}
      extra={
        <Space>
          <Select
            style={{ width: 200 }}
            placeholder={t('management.validationRules.tablePlaceholder')}
            allowClear
            value={selectedDs}
            onChange={(v) => setSelectedDs(v)}
            loading={dsLoading}
            options={dsList?.data_sources?.map((ds) => ({
              label: ds.name,
              value: ds.id,
            }))}
          />
          <Button
            type="primary"
            icon={<PlusOutlined />}
            onClick={() => setCreateOpen(true)}
            disabled={!datasourceId}
          >
            {t('management.validationRules.newRule')}
          </Button>
        </Space>
      }
      style={{ marginBottom: 0 }}
    >
      <Text type="secondary" style={{ display: 'block', marginBottom: 12, fontSize: 12 }}>
        {t('management.validationRules.hint')}
      </Text>

      {!datasourceId ? (
        <div style={{ textAlign: 'center', padding: 32 }}>
          <Empty description={t('management.domains.selectDatasource')} />
        </div>
      ) : isLoading ? (
        <div style={{ textAlign: 'center', padding: 32 }}>
          <Spin />
        </div>
      ) : (
        <Table
          dataSource={rules}
          columns={columns}
          rowKey="id"
          pagination={{ pageSize: 20, showSizeChanger: true, showTotal: (total: number) => `${total}` }}
          size="small"
          locale={{ emptyText: <Empty description={t('management.validationRules.noRules')} /> }}
        />
      )}

      <Modal
        title={t('management.validationRules.newRule')}
        open={createOpen}
        onCancel={() => { setCreateOpen(false); form.resetFields(); }}
        footer={null}
        destroyOnHidden
      >
        <Form
          form={form}
          layout="vertical"
          initialValues={{
            severity: 'warning',
            ruleType: 'range',
            ruleConfig: buildConfig('range'),
          }}
          onFinish={(values) => {
            const type = values.ruleType as string;
            create.mutate({
              ...values,
              ruleConfig: normalizeRuleConfig(type, values.ruleConfig),
            });
          }}
        >
          <Space style={{ width: '100%' }} size="middle">
            <Form.Item
              name="tableName"
              label={t('management.validationRules.tableName')}
              rules={[{ required: true }]}
              style={{ flex: 1 }}
            >
              <Input placeholder={t('management.validationRules.tablePlaceholder')} />
            </Form.Item>
            <Form.Item
              name="columnName"
              label={t('management.validationRules.columnName')}
              rules={[{ required: true }]}
              style={{ flex: 1 }}
            >
              <Input placeholder={t('management.validationRules.columnPlaceholder')} />
            </Form.Item>
          </Space>
          <Space style={{ width: '100%' }} size="middle">
            <Form.Item
              name="ruleType"
              label={t('management.validationRules.ruleType')}
              rules={[{ required: true }]}
              style={{ flex: 1 }}
            >
              <Select
                options={RULE_TYPES(t)}
                style={{ width: '100%' }}
                dropdownStyle={{ minWidth: 220 }}
                popupMatchSelectWidth={false}
                onChange={(v) => form.setFieldsValue({ ruleConfig: buildConfig(v) })}
              />
            </Form.Item>
            <Form.Item
              name="severity"
              label={t('management.validationRules.severity')}
              style={{ width: 120 }}
            >
              <Select
                options={[
                  { label: t('management.validationRules.warning'), value: 'warning' },
                  { label: t('management.validationRules.error'), value: 'error' },
                ]}
              />
            </Form.Item>
          </Space>
          <RuleConfigFields t={t} ruleType={(createRuleType as string) || 'range'} />
          <Form.Item
            name="description"
            label={t('management.validationRules.description')}
          >
            <Input.TextArea rows={2} />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Space>
              <Button
                type="primary"
                htmlType="submit"
                loading={create.isPending}
              >
                {t('management.validationRules.create')}
              </Button>
              <Button onClick={() => { setCreateOpen(false); form.resetFields(); }}>
                {t('management.validationRules.cancel')}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={t('management.validationRules.editTitle')}
        open={!!editRule}
        onCancel={closeEditModal}
        footer={null}
        destroyOnHidden
      >
        {editRule && (
          <Form
            key={editRule.id}
            form={editForm}
            layout="vertical"
            onFinish={(values) => {
              const type = values.ruleType as string;
              const cfg = normalizeRuleConfig(type, values.ruleConfig);
              updateMut.mutate({
                ruleId: editRule.id,
                data: { ...values, ruleConfig: cfg },
              });
            }}
          >
            <Space style={{ width: '100%' }} size="middle">
              <Form.Item
                name="tableName"
                label={t('management.validationRules.tableName')}
                rules={[{ required: true }]}
                style={{ flex: 1 }}
              >
                <Input />
              </Form.Item>
              <Form.Item
                name="columnName"
                label={t('management.validationRules.columnName')}
                rules={[{ required: true }]}
                style={{ flex: 1 }}
              >
                <Input />
              </Form.Item>
            </Space>
            <Space style={{ width: '100%' }} size="middle">
              <Form.Item
                name="ruleType"
                label={t('management.validationRules.ruleType')}
                rules={[{ required: true }]}
                style={{ flex: 1 }}
              >
                <Select
                  options={RULE_TYPES(t)}
                  style={{ width: '100%' }}
                  dropdownStyle={{ minWidth: 220 }}
                  popupMatchSelectWidth={false}
                  onChange={(v) => editForm.setFieldsValue({ ruleConfig: buildConfig(v) })}
                />
              </Form.Item>
              <Form.Item
                name="severity"
                label={t('management.validationRules.severity')}
                style={{ width: 120 }}
              >
                <Select
                  options={[
                    { label: t('management.validationRules.warning'), value: 'warning' },
                    { label: t('management.validationRules.error'), value: 'error' },
                  ]}
                />
              </Form.Item>
            </Space>
            <RuleConfigFields t={t} ruleType={(editRuleType as string) || editRule.ruleType} />
            <Form.Item
              name="description"
              label={t('management.validationRules.description')}
            >
              <Input.TextArea rows={2} />
            </Form.Item>
            <Form.Item style={{ marginBottom: 0 }}>
              <Space>
                <Button
                  type="primary"
                  htmlType="submit"
                  loading={updateMut.isPending}
                >
                  {t('common.save')}
                </Button>
                <Button onClick={closeEditModal}>
                  {t('common.cancel')}
                </Button>
              </Space>
            </Form.Item>
          </Form>
        )}
      </Modal>
    </Card>
  );
}

function RuleConfigFields({
  t,
  ruleType,
}: {
  t: (key: string) => string;
  ruleType: string;
}) {
  if (ruleType === 'range') {
    return (
      <Space style={{ width: '100%' }} size="middle">
        <Form.Item
          name={['ruleConfig', 'min']}
          label={t('management.validationRules.minValue')}
          rules={[{ required: true }]}
          style={{ flex: 1 }}
        >
          <InputNumber style={{ width: '100%' }} />
        </Form.Item>
        <Form.Item
          name={['ruleConfig', 'max']}
          label={t('management.validationRules.maxValue')}
          style={{ flex: 1 }}
        >
          <InputNumber style={{ width: '100%' }} placeholder={t('management.validationRules.unlimited')} />
        </Form.Item>
      </Space>
    );
  }

  if (ruleType === 'null_ratio') {
    return (
      <Form.Item
        name={['ruleConfig', 'max_pct']}
        label={t('management.validationRules.configMaxNull')}
        rules={[{ required: true }]}
      >
        <InputNumber min={0} max={100} style={{ width: '100%' }} addonAfter="%" />
      </Form.Item>
    );
  }

  if (ruleType === 'row_count') {
    return (
      <Space style={{ width: '100%' }} size="middle">
        <Form.Item
          name={['ruleConfig', 'min']}
          label={t('management.validationRules.minValue')}
          rules={[{ required: true }]}
          style={{ flex: 1 }}
        >
          <InputNumber min={0} style={{ width: '100%' }} />
        </Form.Item>
        <Form.Item
          name={['ruleConfig', 'max']}
          label={t('management.validationRules.maxValue')}
          style={{ flex: 1 }}
        >
          <InputNumber min={0} style={{ width: '100%' }} placeholder={t('management.validationRules.unlimited')} />
        </Form.Item>
      </Space>
    );
  }

  if (ruleType === 'freshness') {
    return (
      <Form.Item
        name={['ruleConfig', 'max_staleness_hours']}
        label={t('management.validationRules.configMaxStaleness')}
        rules={[{ required: true }]}
      >
        <InputNumber min={1} style={{ width: '100%' }} addonAfter="h" />
      </Form.Item>
    );
  }

  if (ruleType === 'cardinality') {
    return (
      <Form.Item
        name={['ruleConfig', 'distinct_min']}
        label={t('management.validationRules.configMinDistinct')}
        rules={[{ required: true }]}
      >
        <InputNumber min={0} style={{ width: '100%' }} />
      </Form.Item>
    );
  }

  return null;
}
