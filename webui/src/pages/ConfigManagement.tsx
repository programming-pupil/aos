import { useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Card,
  Col,
  Form,
  Input,
  InputNumber,
  Row,
  Select,
  Space,
  Table,
  Tabs,
  Tag,
  Typography,
  Upload,
  message,
} from 'antd';
import { DownloadOutlined, UploadOutlined } from '@ant-design/icons';
import type { UploadProps } from 'antd';
import type { ColumnsType } from 'antd/es/table';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  configApi,
  type ConfigEnvEntry,
  type ConfigManagementTab,
} from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { useTranslation } from 'react-i18next';

const { Title, Text } = Typography;

const sourceTagColor = (source: string): string => {
  if (source === 'env') return 'green';
  if (source.includes('default')) return 'blue';
  if (source === 'table') return 'gold';
  return 'default';
};

type ConfigImportableEnvEntry = {
  key: string;
  value?: string;
  valueType?: string;
  source?: string;
  clear?: boolean;
};

type ConfigManagementExportPayload = {
  schemaVersion: number;
  exportedAt: string;
  data: {
    operations: ConfigManagementTab;
    analytics: ConfigManagementTab;
    engineering: ConfigManagementTab;
  };
};

function EnvTable({
  env,
  editingEnv,
  setEditingEnv,
  onSave,
  onReset,
  loadingKey,
  t,
}: {
  env: ConfigEnvEntry[];
  editingEnv: Record<string, string>;
  setEditingEnv: React.Dispatch<React.SetStateAction<Record<string, string>>>;
  onSave: (entry: ConfigEnvEntry) => void;
  onReset: (entry: ConfigEnvEntry) => void;
  loadingKey: string | null;
  t: (key: string) => string;
}) {
  const [pagination, setPagination] = useState({ current: 1, pageSize: 20 });
  useEffect(() => {
    const maxPage = Math.max(1, Math.ceil(env.length / pagination.pageSize));
    if (pagination.current > maxPage) {
      setPagination((prev) => ({ ...prev, current: maxPage }));
    }
  }, [env.length, pagination.current, pagination.pageSize]);

  const columns: ColumnsType<ConfigEnvEntry> = [
    {
      title: t('configManagement.columns.key'),
      dataIndex: 'key',
      key: 'key',
      width: 280,
      render: (v: string) => <Text code>{v}</Text>,
    },
    {
      title: t('configManagement.columns.desc'),
      dataIndex: 'description',
      key: 'description',
      width: 320,
      render: (_: string, row) => (
        <div>
          <div style={{ fontWeight: 600 }}>{row.label}</div>
          <Text type="secondary">{row.description}</Text>
        </div>
      ),
    },
    {
      title: t('configManagement.columns.type'),
      dataIndex: 'valueType',
      key: 'valueType',
      width: 110,
      render: (v: string) => <Tag>{v}</Tag>,
    },
    {
      title: t('configManagement.columns.value'),
      key: 'value',
      width: 340,
      render: (_, row) => {
        const value = editingEnv[row.key] ?? row.value;
        if (row.valueType === 'bool') {
          return (
            <Select
              value={value}
              style={{ width: '100%' }}
              options={[
                { label: 'true', value: 'true' },
                { label: 'false', value: 'false' },
              ]}
              onChange={(next) => setEditingEnv((prev) => ({ ...prev, [row.key]: next }))}
            />
          );
        }
        if (row.valueType === 'secret') {
          return (
            <Input.Password
              value={editingEnv[row.key] ?? ''}
              onChange={(e) => setEditingEnv((prev) => ({ ...prev, [row.key]: e.target.value }))}
              placeholder={row.value || t('configManagement.secretPlaceholder')}
            />
          );
        }
        return (
          <Input
            value={value}
            onChange={(e) => setEditingEnv((prev) => ({ ...prev, [row.key]: e.target.value }))}
            placeholder={row.defaultValue}
          />
        );
      },
    },
    {
      title: t('configManagement.columns.defaultValue'),
      dataIndex: 'defaultValue',
      key: 'defaultValue',
      width: 160,
      render: (v: string) => <Text code>{v || '-'}</Text>,
    },
    {
      title: t('configManagement.columns.source'),
      dataIndex: 'source',
      key: 'source',
      width: 140,
      render: (v: string) => <Tag color={sourceTagColor(v)}>{v}</Tag>,
    },
    {
      title: t('common.actions'),
      key: 'actions',
      fixed: 'right',
      width: 170,
      render: (_, row) => {
        const busy = loadingKey === row.key;
        return (
          <Space>
            <Button size="small" type="primary" loading={busy} onClick={() => onSave(row)}>
              {t('common.save')}
            </Button>
            <Button size="small" loading={busy} onClick={() => onReset(row)}>
              {t('common.reset')}
            </Button>
          </Space>
        );
      },
    },
  ];

  return (
    <Table
      rowKey="key"
      columns={columns}
      dataSource={env}
      pagination={{
        current: pagination.current,
        pageSize: pagination.pageSize,
        total: env.length,
        showSizeChanger: true,
        pageSizeOptions: ['10', '20', '50', '100'],
        showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
        onChange: (current, pageSize) => {
          setPagination((prev) => ({
            current: pageSize !== prev.pageSize ? 1 : current,
            pageSize,
          }));
        },
        onShowSizeChange: (_current, pageSize) => {
          setPagination({ current: 1, pageSize });
        },
      }}
      scroll={{ x: 1600 }}
      size="middle"
    />
  );
}

export default function ConfigManagement() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [editingEnv, setEditingEnv] = useState<Record<string, string>>({});
  const [budgetForm] = Form.useForm();
  const [importing, setImporting] = useState(false);

  const { data, isLoading, isFetching } = useQuery({
    queryKey: queryKeys.config.management(),
    queryFn: () => configApi.getManagementOverview(),
  });

  useEffect(() => {
    if (!data) return;
    const next: Record<string, string> = {};
    [...data.operations.env, ...data.analytics.env, ...data.engineering.env].forEach((entry) => {
      if (entry.valueType !== 'secret') next[entry.key] = entry.value;
    });
    setEditingEnv(next);
  }, [data]);

  useEffect(() => {
    const budget = data?.operations.pmBudgetProfile;
    if (budget) {
      budgetForm.setFieldsValue({
        profileKey: budget.profileKey,
        enabled: budget.enabled,
        isDefault: budget.isDefault,
        priority: budget.priority,
        pipelineTimeoutSecs: budget.pipelineTimeoutSecs,
        maxAttempts: budget.maxAttempts,
        retrieveMaxToolCalls: budget.retrieveMaxToolCalls,
        maxCallsPerSource: budget.maxCallsPerSource,
        sourceSlotSearchSecs: budget.sourceSlotSearchSecs,
        sourceSlotBrowserSecs: budget.sourceSlotBrowserSecs,
        sourceSlotApiFetchSecs: budget.sourceSlotApiFetchSecs,
        preflightModelTimeoutSecs: budget.preflightModelTimeoutSecs,
        preflightProbeTimeoutSecs: budget.preflightProbeTimeoutSecs,
        preflightOverallTimeoutSecs: budget.preflightOverallTimeoutSecs,
        retryStepBudgetSecs: budget.retryStepBudgetSecs,
        retryTotalBudgetSecs: budget.retryTotalBudgetSecs,
      });
    }
  }, [budgetForm, data]);

  const refreshOverview = () => queryClient.invalidateQueries({ queryKey: queryKeys.config.management() });

  const updateEnvMutation = useMutation({
    mutationFn: (payload: { key: string; value?: string | null; clear?: boolean }) =>
      configApi.updateManagementEnv(payload),
    onSuccess: (_data, variables) => {
      message.success(t('configManagement.messages.envUpdated'));
      refreshOverview();
      if (variables.key === 'AOSD_GITHUB_TOKEN') {
        queryClient.invalidateQueries({ queryKey: queryKeys.skills.all });
      }
    },
    onError: () => message.error(t('configManagement.messages.updateFailed')),
  });

  const updateBudgetMutation = useMutation({
    mutationFn: configApi.updateManagementPmBudgetProfile,
    onSuccess: () => {
      message.success(t('configManagement.messages.budgetUpdated'));
      refreshOverview();
    },
    onError: () => message.error(t('configManagement.messages.updateFailed')),
  });

  const envLoadingKey = useMemo(() => {
    const key = updateEnvMutation.variables?.key;
    return updateEnvMutation.isPending && key ? key : null;
  }, [updateEnvMutation.isPending, updateEnvMutation.variables]);

  const saveEnv = (entry: ConfigEnvEntry) => {
    const nextValue = editingEnv[entry.key] ?? entry.value;
    if (entry.valueType === 'secret' && editingEnv[entry.key] === undefined) {
      message.warning(t('configManagement.messages.secretUnchanged'));
      return;
    }
    updateEnvMutation.mutate({ key: entry.key, value: nextValue });
  };

  const resetEnv = (entry: ConfigEnvEntry) => {
    updateEnvMutation.mutate({ key: entry.key, clear: true });
  };

  const operationsTab: ConfigManagementTab = data?.operations ?? { env: [] };
  const analyticsTab: ConfigManagementTab = data?.analytics ?? { env: [] };
  const engineeringTab: ConfigManagementTab = data?.engineering ?? { env: [] };

  const exportConfig = () => {
    if (!data) {
      message.warning(t('configManagement.messages.noDataToExport'));
      return;
    }

    const withLatestValues = (tab: ConfigManagementTab): ConfigManagementTab => ({
      ...tab,
      env: tab.env.map((entry) => ({
        ...entry,
        value:
          entry.valueType === 'secret'
            ? ''
            : editingEnv[entry.key] ?? entry.value,
      })),
    });

    const payload: ConfigManagementExportPayload = {
      schemaVersion: 1,
      exportedAt: new Date().toISOString(),
      data: {
        operations: withLatestValues(operationsTab),
        analytics: withLatestValues(analyticsTab),
        engineering: withLatestValues(engineeringTab),
      },
    };

    const blob = new Blob([JSON.stringify(payload, null, 2)], {
      type: 'application/json',
    });
    const url = URL.createObjectURL(blob);
    const filename = `config-management-${new Date().toISOString().replace(/[:.]/g, '-')}.json`;
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
    message.success(t('configManagement.messages.exportSuccess'));
  };

  const applyEnvImportEntry = async (item: ConfigImportableEnvEntry): Promise<boolean> => {
    const key = String(item?.key ?? '').trim();
    if (!key) return false;
    if (item.valueType === 'secret' && !String(item.value ?? '').trim()) return false;
    const clear = item.clear === true || (item.source != null && item.source !== 'env');
    if (clear) {
      await configApi.updateManagementEnv({ key, clear: true });
      return true;
    }
    await configApi.updateManagementEnv({
      key,
      value: item.value == null ? '' : String(item.value),
    });
    return true;
  };

  const importConfigJson = async (file: File) => {
    setImporting(true);
    try {
      const rawText = await file.text();
      const parsed = JSON.parse(rawText) as Partial<ConfigManagementExportPayload> & {
        operations?: ConfigManagementTab;
        analytics?: ConfigManagementTab;
        engineering?: ConfigManagementTab;
      };
      const content = parsed.data ?? parsed;
      const operations = content.operations;
      const analytics = content.analytics;
      const engineering = content.engineering;
      if (!operations || !analytics) {
        throw new Error(t('configManagement.messages.invalidImportFile'));
      }

      let envApplied = 0;
      const envErrors: string[] = [];

      const importEnvList = async (list: ConfigImportableEnvEntry[]) => {
        for (const item of list) {
          try {
            if (await applyEnvImportEntry(item)) envApplied += 1;
          } catch (error) {
            envErrors.push(`${item.key}: ${error instanceof Error ? error.message : String(error)}`);
          }
        }
      };

      await importEnvList((operations.env ?? []) as ConfigImportableEnvEntry[]);
      await importEnvList((analytics.env ?? []) as ConfigImportableEnvEntry[]);
      if (engineering?.env?.length) {
        await importEnvList((engineering.env ?? []) as ConfigImportableEnvEntry[]);
      }

      const sectionErrors: string[] = [];

      if (operations.pmBudgetProfile) {
        try {
          await configApi.updateManagementPmBudgetProfile({
            profileKey: operations.pmBudgetProfile.profileKey,
            enabled: operations.pmBudgetProfile.enabled,
            isDefault: operations.pmBudgetProfile.isDefault,
            priority: operations.pmBudgetProfile.priority,
            pipelineTimeoutSecs: operations.pmBudgetProfile.pipelineTimeoutSecs,
            maxAttempts: operations.pmBudgetProfile.maxAttempts,
            retrieveMaxToolCalls: operations.pmBudgetProfile.retrieveMaxToolCalls,
            maxCallsPerSource: operations.pmBudgetProfile.maxCallsPerSource,
            sourceSlotSearchSecs: operations.pmBudgetProfile.sourceSlotSearchSecs,
            sourceSlotBrowserSecs: operations.pmBudgetProfile.sourceSlotBrowserSecs,
            sourceSlotApiFetchSecs: operations.pmBudgetProfile.sourceSlotApiFetchSecs,
            preflightModelTimeoutSecs: operations.pmBudgetProfile.preflightModelTimeoutSecs,
            preflightProbeTimeoutSecs: operations.pmBudgetProfile.preflightProbeTimeoutSecs,
            preflightOverallTimeoutSecs: operations.pmBudgetProfile.preflightOverallTimeoutSecs,
            retryStepBudgetSecs: operations.pmBudgetProfile.retryStepBudgetSecs,
            retryTotalBudgetSecs: operations.pmBudgetProfile.retryTotalBudgetSecs,
          });
        } catch (error) {
          sectionErrors.push(error instanceof Error ? error.message : String(error));
        }
      }

      await refreshOverview();

      if (envErrors.length || sectionErrors.length) {
        message.warning(
          t('configManagement.messages.importPartial', {
            applied: envApplied,
            errors: envErrors.length + sectionErrors.length,
          }),
        );
      } else {
        message.success(t('configManagement.messages.importSuccess', { applied: envApplied }));
      }
    } catch (error) {
      message.error(
        `${t('configManagement.messages.importFailed')}: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setImporting(false);
    }
  };

  const importUploadProps: UploadProps = {
    accept: '.json,application/json',
    showUploadList: false,
    beforeUpload: (file) => {
      void importConfigJson(file as File);
      return false;
    },
  };

  return (
    <div style={{ padding: 20 }}>
      <Space direction="vertical" size={16} style={{ width: '100%' }}>
        <Row justify="space-between" align="middle" gutter={16}>
          <Col>
            <Title level={3} style={{ marginBottom: 6 }}>
              {t('configManagement.title')}
            </Title>
            <Text type="secondary">{t('configManagement.subtitle')}</Text>
          </Col>
          <Col>
            <Space>
              <Button icon={<DownloadOutlined />} onClick={exportConfig}>
                {t('configManagement.actions.exportJson')}
              </Button>
              <Upload {...importUploadProps}>
                <Button icon={<UploadOutlined />} loading={importing}>
                  {t('configManagement.actions.importJson')}
                </Button>
              </Upload>
            </Space>
          </Col>
        </Row>

        <Alert
          type="info"
          showIcon
          message={t('configManagement.noticeTitle')}
          description={t('configManagement.noticeDesc')}
        />

        <Tabs
          items={[
            {
              key: 'operations',
              label: t('configManagement.tabs.operations'),
              children: (
                <Space direction="vertical" size={16} style={{ width: '100%' }}>
                  <Card title={t('configManagement.envSectionTitle')}>
                    <EnvTable
                      env={operationsTab.env}
                      editingEnv={editingEnv}
                      setEditingEnv={setEditingEnv}
                      onSave={saveEnv}
                      onReset={resetEnv}
                      loadingKey={envLoadingKey}
                      t={t}
                    />
                  </Card>

                  <Card title={t('configManagement.budgetProfileTitle')}>
                    <Form layout="vertical" form={budgetForm} onFinish={(values) => updateBudgetMutation.mutate(values)}>
                      <Row gutter={16}>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.profileKey')} name="profileKey" rules={[{ required: true }]}>
                            <Input />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.enabled')} name="enabled">
                            <Select options={[{ label: 'true', value: true }, { label: 'false', value: false }]} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.isDefault')} name="isDefault">
                            <Select options={[{ label: 'true', value: true }, { label: 'false', value: false }]} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.priority')} name="priority">
                            <InputNumber min={0} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                      </Row>

                      <Row gutter={16}>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.pipelineTimeoutSecs')} name="pipelineTimeoutSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.maxAttempts')} name="maxAttempts">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.retrieveMaxToolCalls')} name="retrieveMaxToolCalls">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.maxCallsPerSource')} name="maxCallsPerSource">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                      </Row>

                      <Row gutter={16}>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.sourceSlotSearchSecs')} name="sourceSlotSearchSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.sourceSlotBrowserSecs')} name="sourceSlotBrowserSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.sourceSlotApiFetchSecs')} name="sourceSlotApiFetchSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.preflightModelTimeoutSecs')} name="preflightModelTimeoutSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                      </Row>

                      <Row gutter={16}>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.preflightProbeTimeoutSecs')} name="preflightProbeTimeoutSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.preflightOverallTimeoutSecs')} name="preflightOverallTimeoutSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.retryStepBudgetSecs')} name="retryStepBudgetSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                        <Col span={6}>
                          <Form.Item label={t('configManagement.retryTotalBudgetSecs')} name="retryTotalBudgetSecs">
                            <InputNumber min={1} style={{ width: '100%' }} />
                          </Form.Item>
                        </Col>
                      </Row>
                      <Button type="primary" htmlType="submit" loading={updateBudgetMutation.isPending}>
                        {t('common.save')}
                      </Button>
                    </Form>
                  </Card>
                </Space>
              ),
            },
            {
              key: 'analytics',
              label: t('configManagement.tabs.analytics'),
              children: (
                <Card title={t('configManagement.envSectionTitle')}>
                  <EnvTable
                    env={analyticsTab.env}
                    editingEnv={editingEnv}
                    setEditingEnv={setEditingEnv}
                    onSave={saveEnv}
                    onReset={resetEnv}
                    loadingKey={envLoadingKey}
                    t={t}
                  />
                </Card>
              ),
            },
            {
              key: 'engineering',
              label: t('configManagement.tabs.engineering'),
              children: engineeringTab.env.length > 0 ? (
                <Card title={t('configManagement.envSectionTitle')}>
                  <EnvTable
                    env={engineeringTab.env}
                    editingEnv={editingEnv}
                    setEditingEnv={setEditingEnv}
                    onSave={saveEnv}
                    onReset={resetEnv}
                    loadingKey={envLoadingKey}
                    t={t}
                  />
                </Card>
              ) : (
                <Alert type="warning" showIcon message={t('configManagement.engineeringPlaceholder')} />
              ),
            },
          ]}
        />

        {(isLoading || isFetching) && <Text type="secondary">{t('common.loading')}</Text>}
      </Space>
    </div>
  );
}
