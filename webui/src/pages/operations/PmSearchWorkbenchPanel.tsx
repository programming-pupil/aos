import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Alert,
  Button,
  Divider,
  Drawer,
  Empty,
  Form,
  Input,
  InputNumber,
  Popconfirm,
  Select,
  Space,
  Switch,
  Tabs,
  Tag,
  Tooltip,
  Typography,
  message,
} from 'antd';
import {
  CheckCircleOutlined,
  DeleteOutlined,
  DownOutlined,
  EditOutlined,
  ExperimentOutlined,
  HolderOutlined,
  PlusOutlined,
  ReloadOutlined,
  SearchOutlined,
  UpOutlined,
  WarningOutlined,
} from '@ant-design/icons';

import { pmApi, type PmSearchProviderPayload, type PmSearchProviderRecord } from '@/api';
import { queryKeys } from '@/api/queryKeys';

const { Text } = Typography;

type PmSearchWorkbenchPanelProps = {
  variant?: 'side' | 'page';
  showRunTabs?: boolean;
};

interface SearchProviderFormValues {
  name: string;
  providerType: string;
  enabled: boolean;
  baseUrl?: string;
  method?: string;
  authType?: string;
  authSecret?: string;
  timeoutSecs?: number;
  maxResults?: number;
  fetchContentEnabled?: boolean;
  contentExtractMode?: string;
  headersJsonText?: string;
  queryTemplateJsonText?: string;
  responseMappingJsonText?: string;
  domainAllowlistText?: string;
  domainBlocklistText?: string;
  rateLimitJsonText?: string;
}

function providerLabel(type?: string): string {
  switch (type) {
    case 'brave':
      return 'Brave';
    case 'tavily':
      return 'Tavily';
    case 'serper':
      return 'Serper';
    case 'exa':
      return 'Exa';
    case 'searxng':
      return 'SearXNG';
    case 'generic_json':
      return 'Generic JSON';
    case 'internal_http':
      return 'Internal HTTP';
    default:
      return type || '-';
  }
}

function healthColor(status?: string): string {
  if (status === 'healthy') return 'success';
  if (status === 'unhealthy') return 'error';
  if (status === 'testing') return 'processing';
  return 'default';
}

function layerTag(available: boolean) {
  return available ? <Tag color="success">ON</Tag> : <Tag>OFF</Tag>;
}

function jsonText(value: unknown): string | undefined {
  if (value == null) return undefined;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return undefined;
  }
}

function parseJsonText(text: string | undefined, label: string): unknown | undefined {
  const trimmed = text?.trim();
  if (!trimmed) return undefined;
  try {
    return JSON.parse(trimmed);
  } catch (error) {
    throw new Error(`${label}: ${(error as Error).message}`, { cause: error });
  }
}

function parseStringListText(text: string | undefined): string[] | undefined {
  const values = text
    ?.split(/[\n,]/)
    .map((item) => item.trim())
    .filter(Boolean);
  return values && values.length > 0 ? values : undefined;
}

function providerRecordToFormValues(row: PmSearchProviderRecord): SearchProviderFormValues {
  return {
    name: row.name,
    providerType: row.providerType,
    enabled: row.enabled,
    baseUrl: row.baseUrl || undefined,
    method: row.method || 'GET',
    authType: row.authType || 'api_key',
    authSecret: undefined,
    timeoutSecs: row.timeoutSecs,
    maxResults: row.maxResults,
    fetchContentEnabled: row.fetchContentEnabled,
    contentExtractMode: row.contentExtractMode || 'auto',
    headersJsonText: jsonText(row.headersJson),
    queryTemplateJsonText: jsonText(row.queryTemplateJson),
    responseMappingJsonText: jsonText(row.responseMappingJson),
    domainAllowlistText: Array.isArray(row.domainAllowlistJson)
      ? row.domainAllowlistJson.join('\n')
      : jsonText(row.domainAllowlistJson),
    domainBlocklistText: Array.isArray(row.domainBlocklistJson)
      ? row.domainBlocklistJson.join('\n')
      : jsonText(row.domainBlocklistJson),
    rateLimitJsonText: jsonText(row.rateLimitJson),
  };
}

const emptyProviderFormValues: Partial<SearchProviderFormValues> = {
  name: undefined,
  providerType: undefined,
  enabled: undefined,
  baseUrl: undefined,
  method: undefined,
  authType: undefined,
  authSecret: undefined,
  timeoutSecs: undefined,
  maxResults: undefined,
  fetchContentEnabled: undefined,
  contentExtractMode: undefined,
  headersJsonText: undefined,
  queryTemplateJsonText: undefined,
  responseMappingJsonText: undefined,
  domainAllowlistText: undefined,
  domainBlocklistText: undefined,
  rateLimitJsonText: undefined,
};

export function PmSearchWorkbenchPanel({ variant = 'side', showRunTabs }: PmSearchWorkbenchPanelProps) {
  const { t } = useTranslation();
  const isPage = variant === 'page';
  const includeRunTabs = showRunTabs ?? !isPage;
  const qc = useQueryClient();
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [editing, setEditing] = useState<PmSearchProviderRecord | null>(null);
  const [drawerFormVersion, setDrawerFormVersion] = useState(0);
  const [drawerInitialValues, setDrawerInitialValues] = useState<SearchProviderFormValues | undefined>();
  const [draggingProviderId, setDraggingProviderId] = useState<string | null>(null);
  const [dragOverProviderId, setDragOverProviderId] = useState<string | null>(null);
  const [form] = Form.useForm<SearchProviderFormValues>();

  const providersQ = useQuery({
    queryKey: queryKeys.pm.searchProviders(),
    queryFn: pmApi.listSearchProviders,
  });
  const doctorQ = useQuery({
    queryKey: queryKeys.pm.searchDoctor(),
    queryFn: pmApi.getSearchDoctor,
  });

  const providerItems = providersQ.data?.items ?? [];
  const templates = providersQ.data?.templates ?? [];
  const templateByType = useMemo(
    () => new Map(templates.map((template) => [template.providerType, template])),
    [templates],
  );

  useEffect(() => {
    if (!drawerOpen || !drawerInitialValues) return;
    form.setFieldsValue(drawerInitialValues);
  }, [drawerOpen, drawerInitialValues, form]);

  const applyDrawerFormValues = (values: SearchProviderFormValues) => {
    const nextValues = {
      ...emptyProviderFormValues,
      ...values,
    } as SearchProviderFormValues;
    setDrawerInitialValues(nextValues);
    setDrawerFormVersion((version) => version + 1);
    form.setFieldsValue(nextValues);
  };

  const refreshAll = () => {
    qc.invalidateQueries({ queryKey: queryKeys.pm.searchProviders() });
    qc.invalidateQueries({ queryKey: queryKeys.pm.searchDoctor() });
  };

  const createMut = useMutation({
    mutationFn: (payload: PmSearchProviderPayload) => pmApi.createSearchProvider(payload),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      setDrawerOpen(false);
      setEditing(null);
      form.resetFields();
      refreshAll();
    },
    onError: (err: Error) => message.error(err.message || t('common.operateFailed')),
  });

  const updateMut = useMutation({
    mutationFn: ({ id, payload }: { id: string; payload: PmSearchProviderPayload }) =>
      pmApi.updateSearchProvider(id, payload),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      setDrawerOpen(false);
      setEditing(null);
      form.resetFields();
      refreshAll();
    },
    onError: (err: Error) => message.error(err.message || t('common.operateFailed')),
  });

  const deleteMut = useMutation({
    mutationFn: (id: string) => pmApi.deleteSearchProvider(id),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      refreshAll();
    },
    onError: (err: Error) => message.error(err.message || t('common.operateFailed')),
  });

  const testMut = useMutation({
    mutationFn: (id: string) => pmApi.testSearchProvider(id),
    onSuccess: (result) => {
      if (result.ok) {
        message.success(t('operations.searchProviderTestOk', 'Provider test passed'));
      } else {
        message.error(result.error || t('operations.searchProviderTestFailed', 'Provider test failed'));
      }
      refreshAll();
    },
    onError: (err: Error) => message.error(err.message || t('common.operateFailed')),
  });

  const reorderMut = useMutation({
    mutationFn: (providerIds: string[]) => pmApi.reorderSearchProviders(providerIds),
    onSuccess: refreshAll,
    onError: (err: Error) => message.error(err.message || t('common.operateFailed')),
  });

  const openCreate = () => {
    setEditing(null);
    const first = templates[0];
    applyDrawerFormValues({
      providerType: first?.providerType ?? 'brave',
      name: first?.label ?? 'Brave Search',
      enabled: true,
      baseUrl: first?.defaultBaseUrl,
      method: first?.defaultMethod ?? 'GET',
      authType: 'api_key',
      timeoutSecs: 12,
      maxResults: 10,
      fetchContentEnabled: true,
      contentExtractMode: 'auto',
    });
    setDrawerOpen(true);
  };

  const openEdit = (row: PmSearchProviderRecord) => {
    setEditing(row);
    applyDrawerFormValues(providerRecordToFormValues(row));
    setDrawerOpen(true);
  };

  const onProviderTypeChange = (type: string) => {
    const template = templateByType.get(type as never);
    if (!template) return;
    form.setFieldsValue({
      name: form.getFieldValue('name') || template.label,
      baseUrl: template.defaultBaseUrl || undefined,
      method: template.defaultMethod,
      authType: type === 'searxng' ? 'none' : 'api_key',
    });
  };

  const submit = async () => {
    const values = await form.validateFields();
    let headersJson: unknown | undefined;
    let queryTemplateJson: unknown | undefined;
    let responseMappingJson: unknown | undefined;
    let rateLimitJson: unknown | undefined;
    try {
      headersJson = parseJsonText(values.headersJsonText, 'headersJson');
      queryTemplateJson = parseJsonText(values.queryTemplateJsonText, 'queryTemplateJson');
      responseMappingJson = parseJsonText(values.responseMappingJsonText, 'responseMappingJson');
      rateLimitJson = parseJsonText(values.rateLimitJsonText, 'rateLimitJson');
    } catch (error) {
      message.error((error as Error).message);
      return;
    }
    const payload: PmSearchProviderPayload = {
      ...values,
      authSecret: values.authSecret?.trim() || undefined,
      baseUrl: values.baseUrl?.trim() || undefined,
      contentExtractMode: values.contentExtractMode || 'auto',
      headersJson: headersJson as Record<string, unknown> | undefined,
      queryTemplateJson: queryTemplateJson as Record<string, unknown> | undefined,
      responseMappingJson: responseMappingJson as Record<string, unknown> | undefined,
      domainAllowlistJson: parseStringListText(values.domainAllowlistText),
      domainBlocklistJson: parseStringListText(values.domainBlocklistText),
      rateLimitJson,
    };
    delete (payload as Record<string, unknown>).headersJsonText;
    delete (payload as Record<string, unknown>).queryTemplateJsonText;
    delete (payload as Record<string, unknown>).responseMappingJsonText;
    delete (payload as Record<string, unknown>).domainAllowlistText;
    delete (payload as Record<string, unknown>).domainBlocklistText;
    delete (payload as Record<string, unknown>).rateLimitJsonText;
    if (editing) {
      updateMut.mutate({ id: editing.id, payload });
    } else {
      createMut.mutate(payload);
    }
  };

  const moveProvider = (providerId: string, direction: -1 | 1) => {
    const ids = providerItems.map((provider) => provider.id);
    const index = ids.indexOf(providerId);
    const nextIndex = index + direction;
    if (index < 0 || nextIndex < 0 || nextIndex >= ids.length) return;
    const nextIds = [...ids];
    [nextIds[index], nextIds[nextIndex]] = [nextIds[nextIndex], nextIds[index]];
    reorderMut.mutate(nextIds);
  };

  const reorderProviderTo = (sourceId: string, targetId: string) => {
    if (sourceId === targetId) return;
    const ids = providerItems.map((provider) => provider.id);
    const sourceIndex = ids.indexOf(sourceId);
    const targetIndex = ids.indexOf(targetId);
    if (sourceIndex < 0 || targetIndex < 0) return;
    const nextIds = [...ids];
    const [source] = nextIds.splice(sourceIndex, 1);
    nextIds.splice(targetIndex, 0, source);
    reorderMut.mutate(nextIds);
  };

  const providerList = (
    <Space direction="vertical" size={10} style={{ width: '100%' }}>
      <Space style={{ width: '100%', justifyContent: 'space-between' }}>
        <Text strong>{t('operations.searchProviders', 'Search Extensions')}</Text>
        <Space size={6}>
          <Tooltip title={t('common.refresh')}>
            <Button size="small" icon={<ReloadOutlined />} onClick={refreshAll} />
          </Tooltip>
          <Button size="small" type="primary" icon={<PlusOutlined />} onClick={openCreate}>
            {t('common.add')}
          </Button>
        </Space>
      </Space>
      {providerItems.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={t('operations.noSearchProviders', 'No Search Extension configured')}
        />
      ) : (
        providerItems.map((provider, index) => (
          <div
            key={provider.id}
            draggable={!reorderMut.isPending}
            onDragStart={(event) => {
              setDraggingProviderId(provider.id);
              event.dataTransfer.effectAllowed = 'move';
              event.dataTransfer.setData('text/plain', provider.id);
            }}
            onDragOver={(event) => {
              event.preventDefault();
              if (dragOverProviderId !== provider.id) {
                setDragOverProviderId(provider.id);
              }
            }}
            onDragLeave={() => {
              setDragOverProviderId((current) => (current === provider.id ? null : current));
            }}
            onDrop={(event) => {
              event.preventDefault();
              const sourceId = event.dataTransfer.getData('text/plain') || draggingProviderId;
              setDraggingProviderId(null);
              setDragOverProviderId(null);
              if (sourceId) reorderProviderTo(sourceId, provider.id);
            }}
            onDragEnd={() => {
              setDraggingProviderId(null);
              setDragOverProviderId(null);
            }}
            style={{
              border: '1px solid var(--border-default)',
              borderRadius: 8,
              padding: 10,
              background: dragOverProviderId === provider.id ? 'var(--bg-hover)' : 'var(--bg-surface)',
              cursor: reorderMut.isPending ? 'default' : 'grab',
              opacity: draggingProviderId === provider.id ? 0.62 : 1,
            }}
          >
            <Space direction="vertical" size={8} style={{ width: '100%' }}>
              <Space style={{ width: '100%', justifyContent: 'space-between' }} align="start">
                <Space size={8} align="start" style={{ minWidth: 0 }}>
                  <Tooltip title={t('operations.dragToReorder', 'Drag to reorder')}>
                    <HolderOutlined style={{ color: 'var(--text-tertiary)', marginTop: 3 }} />
                  </Tooltip>
                  <Space direction="vertical" size={2} style={{ minWidth: 0 }}>
                  <Space size={6} wrap>
                    <Text strong ellipsis style={{ maxWidth: 170 }}>{provider.name}</Text>
                    <Tag>{providerLabel(provider.providerType)}</Tag>
                    <Tag color={provider.enabled ? 'success' : 'default'}>
                      {provider.enabled ? t('common.enabled') : t('common.disabled')}
                    </Tag>
                  </Space>
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {provider.baseUrl || t('operations.providerBaseUrlUnset', 'Base URL not set')}
                  </Text>
                  </Space>
                </Space>
                <Tag color={healthColor(provider.healthStatus)} style={{ marginRight: 0 }}>
                  {provider.healthStatus || '-'}
                </Tag>
              </Space>
              <Space size={6} wrap>
                <Tag>{provider.method}</Tag>
                <Tag>{provider.hasSecret ? `${t('operations.secretSet', 'Secret set')}${provider.keyHint ? ` · ****${provider.keyHint}` : ''}` : t('operations.secretNotSet', 'No secret')}</Tag>
                <Tag>{t('operations.maxResultsShort', 'max')} {provider.maxResults}</Tag>
              </Space>
              {provider.lastError ? (
                <Alert type="error" showIcon message={provider.lastError} style={{ padding: '6px 8px' }} />
              ) : null}
              <Space size={6}>
                <Tooltip title={t('operations.moveUp', 'Move up')}>
                  <Button
                    size="small"
                    icon={<UpOutlined />}
                    disabled={index === 0 || reorderMut.isPending}
                    onClick={() => moveProvider(provider.id, -1)}
                  />
                </Tooltip>
                <Tooltip title={t('operations.moveDown', 'Move down')}>
                  <Button
                    size="small"
                    icon={<DownOutlined />}
                    disabled={index === providerItems.length - 1 || reorderMut.isPending}
                    onClick={() => moveProvider(provider.id, 1)}
                  />
                </Tooltip>
                <Button size="small" icon={<ExperimentOutlined />} loading={testMut.isPending && testMut.variables === provider.id} onClick={() => testMut.mutate(provider.id)}>
                  {t('operations.testProvider', 'Test')}
                </Button>
                <Button size="small" icon={<EditOutlined />} onClick={() => openEdit(provider)}>
                  {t('common.edit')}
                </Button>
                <Popconfirm title={t('common.deleteConfirm')} onConfirm={() => deleteMut.mutate(provider.id)}>
                  <Button size="small" danger icon={<DeleteOutlined />}>
                    {t('common.delete')}
                  </Button>
                </Popconfirm>
              </Space>
            </Space>
          </div>
        ))
      )}
    </Space>
  );

  const doctor = doctorQ.data;
  const doctorPanel = (
    <Space direction="vertical" size={10} style={{ width: '100%' }}>
      <Space style={{ width: '100%', justifyContent: 'space-between' }}>
        <Text strong>{t('operations.searchDoctor', 'Search Doctor')}</Text>
        <Button size="small" icon={<ReloadOutlined />} onClick={refreshAll}>
          {t('common.refresh')}
        </Button>
      </Space>
      {doctor?.degradedReason ? (
        <Alert type="warning" showIcon message={doctor.degradedReason} />
      ) : (
        <Alert type="success" showIcon message={t('operations.searchAvailable', 'External search path is available')} />
      )}
      <Space direction="vertical" size={8} style={{ width: '100%' }}>
        {(doctor?.orchestrator?.layers ?? []).length > 0 ? (
          doctor?.orchestrator?.layers.map((layer) => (
            <div key={layer.key}>
              <div>{layerTag(layer.available)} <Text>{layer.label}</Text></div>
              <Text type="secondary" style={{ fontSize: 12 }}>{layer.detail || '-'}</Text>
            </div>
          ))
        ) : (
          <>
            <div>{layerTag(!!doctor?.builtinWebSearch?.available)} <Text>{t('operations.builtinSearch', 'AOS built-in web search')}</Text></div>
            <Text type="secondary" style={{ fontSize: 12 }}>{doctor?.builtinWebSearch?.detail || '-'}</Text>
            <div>{layerTag(!!doctor?.nativeSearch.available)} <Text>{t('operations.nativeSearch', 'Model native search')}</Text></div>
            <Text type="secondary" style={{ fontSize: 12 }}>{doctor?.nativeSearch.detail || '-'}</Text>
            <div>{layerTag(!!doctor?.mcpSearch.available)} <Text>{t('operations.mcpSearch', 'MCP search')}</Text></div>
            <Text type="secondary" style={{ fontSize: 12 }}>{doctor?.mcpSearch.detail || '-'}</Text>
            <div>{layerTag((doctor?.configuredProviders ?? []).some((p) => p.enabled))} <Text>{t('operations.configuredSearch', 'Configured Search Extensions')}</Text></div>
            <div>{layerTag(!!doctor?.ragLocal.available)} <Text>{t('operations.ragLocal', 'RAG/local fallback')}</Text></div>
          </>
        )}
      </Space>
      <Divider style={{ margin: '4px 0' }} />
      <Text strong>{t('operations.effectiveOrder', 'Effective order')}</Text>
      <Space size={[4, 4]} wrap>
        {(doctor?.orchestrator?.effectiveOrder ?? doctor?.effectiveOrder ?? []).map((item) => (
          <Tag key={item}>{item}</Tag>
        ))}
      </Space>
      {(doctor?.orchestrator?.adapters ?? []).length > 0 ? (
        <>
          <Text strong>{t('operations.searchAdapters', 'Adapters')}</Text>
          <Space size={[4, 4]} wrap>
            {doctor?.orchestrator?.adapters.map((item) => (
              <Tag key={item}>{item}</Tag>
            ))}
          </Space>
        </>
      ) : null}
    </Space>
  );

  const scrollMaxHeight = isPage ? 'calc(100vh - 240px)' : 'calc(100vh - 160px)';
  const tabItems = [
    ...(includeRunTabs
      ? [
          {
            key: 'evidence',
            label: t('operations.evidence', 'Evidence'),
            children: (
              <div style={{ padding: 12 }}>
                <Alert
                  type="info"
                  showIcon
                  icon={<SearchOutlined />}
                  message={t('operations.evidencePanelHint', 'Evidence from PM runs appears with each answer. Search configuration lives here.')}
                />
              </div>
            ),
          },
        ]
      : []),
    {
      key: 'providers',
      label: t('operations.searchProvidersShort', 'Search Extensions'),
      children: <div style={{ padding: 12, overflow: 'auto', maxHeight: scrollMaxHeight }}>{providerList}</div>,
    },
    {
      key: 'doctor',
      label: t('operations.searchDoctorShort', 'Doctor'),
      children: <div style={{ padding: 12, overflow: 'auto', maxHeight: scrollMaxHeight }}>{doctorPanel}</div>,
    },
    ...(includeRunTabs
      ? [
          {
            key: 'quality',
            label: t('operations.qualityGate', 'Quality'),
            children: (
              <div style={{ padding: 12 }}>
                <Alert
                  type="info"
                  showIcon
                  icon={<CheckCircleOutlined />}
                  message={t('operations.qualityGateHint', 'PM strategy answers are checked for evidence, segment mapping, experiments, and protected metrics.')}
                />
              </div>
            ),
          },
          {
            key: 'trace',
            label: t('operations.trace', 'Trace'),
            children: (
              <div style={{ padding: 12 }}>
                <Alert
                  type="warning"
                  showIcon
                  icon={<WarningOutlined />}
                  message={t('operations.traceHint', 'Provider debug and fallback reasons stay in trace, not in the final business answer.')}
                />
              </div>
            ),
          },
        ]
      : []),
  ];

  return (
    <div
      style={{
        width: isPage ? '100%' : 360,
        minWidth: isPage ? 0 : 320,
        borderLeft: isPage ? undefined : '1px solid var(--border-default)',
        border: isPage ? '1px solid var(--border-default)' : undefined,
        borderRadius: isPage ? 8 : undefined,
        background: 'var(--bg-elevated)',
        height: '100%',
        minHeight: 0,
        overflow: 'hidden',
        display: 'flex',
        flexDirection: 'column',
      }}
    >
      <Tabs
        size="small"
        tabBarGutter={12}
        style={{ minWidth: 0, flex: 1, overflow: 'hidden' }}
        tabBarStyle={{ paddingInline: 12, marginBottom: 0, minWidth: 0 }}
        items={tabItems}
      />
      <Drawer
        width={440}
        open={drawerOpen}
        forceRender
        title={editing ? t('operations.editSearchProvider', 'Edit Search Extension Configuration') : t('operations.addSearchProvider', 'Add Search Extension Configuration')}
        onClose={() => setDrawerOpen(false)}
        afterOpenChange={(open) => {
          if (open && drawerInitialValues) {
            form.setFieldsValue(drawerInitialValues);
          }
        }}
        extra={
          <Button type="primary" onClick={submit} loading={createMut.isPending || updateMut.isPending}>
            {t('common.save')}
          </Button>
        }
      >
        <Form
          key={drawerFormVersion}
          form={form}
          layout="vertical"
          initialValues={drawerInitialValues}
        >
          <Form.Item name="providerType" label={t('common.type')} rules={[{ required: true }]}>
            <Select
              onChange={onProviderTypeChange}
              options={templates.map((template) => ({
                label: template.label,
                value: template.providerType,
              }))}
            />
          </Form.Item>
          <Form.Item name="name" label={t('common.name')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked">
            <Switch checkedChildren={t('common.enabled')} unCheckedChildren={t('common.disabled')} />
          </Form.Item>
          <Form.Item name="baseUrl" label={t('common.url')}>
            <Input placeholder="https://search.company.com/api/search" />
          </Form.Item>
          <Space size={12} style={{ width: '100%' }} align="start">
            <Form.Item name="method" label="Method" style={{ flex: 1 }}>
              <Select options={['GET', 'POST', 'PUT'].map((value) => ({ label: value, value }))} />
            </Form.Item>
            <Form.Item name="authType" label="Auth" style={{ flex: 1 }}>
              <Select
                options={[
                  { label: 'API Key', value: 'api_key' },
                  { label: 'Bearer', value: 'bearer' },
                  { label: 'X-API-KEY', value: 'x_api_key' },
                  { label: 'None', value: 'none' },
                ]}
              />
            </Form.Item>
          </Space>
          <Form.Item
            name="authSecret"
            label={editing?.hasSecret ? t('operations.replaceSecret', 'Replace secret') : 'API Key'}
            extra={editing?.hasSecret ? t('operations.secretStoredHint', 'Leave blank to keep the existing secret.') : undefined}
          >
            <Input.Password autoComplete="new-password" />
          </Form.Item>
          <Space size={12} style={{ width: '100%' }} align="start">
            <Form.Item name="timeoutSecs" label={t('operations.timeoutSecs', 'Timeout')} style={{ flex: 1 }}>
              <InputNumber min={1} max={60} style={{ width: '100%' }} addonAfter="s" />
            </Form.Item>
            <Form.Item name="maxResults" label={t('operations.maxResults', 'Max results')} style={{ flex: 1 }}>
              <InputNumber min={1} max={40} style={{ width: '100%' }} />
            </Form.Item>
          </Space>
          <Form.Item name="fetchContentEnabled" label={t('operations.fetchContent', 'Fetch page content')} valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item name="contentExtractMode" label={t('operations.contentExtractMode', 'Content extract mode')}>
            <Select
              options={[
                { label: 'Auto', value: 'auto' },
                { label: 'Snippet only', value: 'snippet' },
                { label: 'Readable text', value: 'readable_text' },
              ]}
            />
          </Form.Item>
          <Divider style={{ margin: '8px 0 12px' }} />
          <Text strong>{t('operations.advancedProviderConfig', 'Advanced mapping')}</Text>
          <Form.Item
            name="headersJsonText"
            label={t('operations.headersJson', 'Headers JSON')}
            extra={t('operations.jsonTemplateHint', 'Use {{query}}, {{maxResults}}, {{locale}}, and {{secret}} placeholders when needed.')}
          >
            <Input.TextArea rows={3} placeholder='{"Authorization":"Bearer {{secret}}"}' />
          </Form.Item>
          <Form.Item name="queryTemplateJsonText" label={t('operations.queryTemplateJson', 'Query template JSON')}>
            <Input.TextArea rows={4} placeholder='{"q":"{{query}}","limit":"{{maxResults}}"}' />
          </Form.Item>
          <Form.Item name="responseMappingJsonText" label={t('operations.responseMappingJson', 'Response mapping JSON')}>
            <Input.TextArea rows={5} placeholder='{"itemsPath":"$.results","titlePath":"$.title","urlPath":"$.url","snippetPath":"$.snippet"}' />
          </Form.Item>
          <Form.Item name="domainAllowlistText" label={t('operations.domainAllowlist', 'Domain allowlist')}>
            <Input.TextArea rows={2} placeholder="example.com" />
          </Form.Item>
          <Form.Item name="domainBlocklistText" label={t('operations.domainBlocklist', 'Domain blocklist')}>
            <Input.TextArea rows={2} placeholder="spam.example" />
          </Form.Item>
          <Form.Item name="rateLimitJsonText" label={t('operations.rateLimitJson', 'Rate limit JSON')}>
            <Input.TextArea rows={3} placeholder='{"rpm":60}' />
          </Form.Item>
          <Alert
            type="info"
            showIcon
            message={t('operations.advancedProviderHint', 'Templates are optional for built-in provider types. Generic/Internal providers can be fully configured here without environment variables.')}
          />
        </Form>
      </Drawer>
    </div>
  );
}
