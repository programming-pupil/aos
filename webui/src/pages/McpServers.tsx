import React, { useState, useMemo, useCallback } from 'react';
import {
  Card,
  Table,
  Tag,
  Typography,
  Button,
  Space,
  message,
  Popconfirm,
  Switch,
  Drawer,
  Form,
  Input,
  Select,
  Divider,
  Badge,
  InputNumber,
  Collapse,
  Empty,
  Alert,
  Tabs,
  Descriptions,
  Tooltip,
  Row,
  Col,
  Statistic,
} from 'antd';
import {
  DeleteOutlined,
  PlusOutlined,
  EditOutlined,
  ReloadOutlined,
  CheckCircleOutlined,
  CloseCircleOutlined,
  DisconnectOutlined,
  ThunderboltOutlined,
  HolderOutlined,
  FileTextOutlined,
  InfoCircleOutlined,
  SearchOutlined,
  LoadingOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useQuery, useMutation, useQueryClient, useQueries } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { mcpApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { useSystemEvents } from '@/api/systemEvents';
import { usePermissions } from '@/store/permissions';
import type { McpServerInfo, McpToolInfo, McpResourceInfo, McpPromptInfo } from '@/types';
import { useTabRefresh } from '@/hooks/useTabRefresh';
import {
  McpStdioConfigError,
  formatMcpStdioConfig,
  parseMcpStdioConfig,
} from './mcpStdioConfig';

const { Title, Text } = Typography;

const AUTH_TYPE_NONE = 'none';
const AUTH_TYPE_BEARER = 'bearer_token';
const AUTH_TYPE_OAUTH = 'oauth';

interface McpFormValues {
  name?: string;
  transport: 'stdio' | 'http' | 'sse';
  stdio_config?: string;
  url?: string;
  auth_type?: string;
  auth_token?: string;
  extra_headers?: string;
  timeout_ms?: number;
}

function stdioConfigErrorMessage(
  error: unknown,
  t: ReturnType<typeof useTranslation>['t'],
) {
  if (error instanceof McpStdioConfigError) {
    return t(`mcp.form.stdioJsonErrors.${error.code}`, { fields: error.detail });
  }
  return t('mcp.form.stdioJsonErrors.invalidJson');
}

function parseExtraHeaders(raw?: string): Record<string, string> | undefined {
  if (!raw?.trim()) return undefined;
  try {
    return JSON.parse(raw);
  } catch {
    return undefined;
  }
}

function extraHeadersToString(headers?: Record<string, string>): string {
  if (!headers) return '';
  return JSON.stringify(headers, null, 2);
}

export default function McpServers() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { hasPermission } = usePermissions();
  const canWrite = hasPermission('mcp:write');
  const canDelete = hasPermission('mcp:delete');
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [editingServer, setEditingServer] = useState<McpServerInfo | null>(null);
  const [form] = Form.useForm<McpFormValues>();
  const [testingServer, setTestingServer] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<string>('all');
  const [expandedServers, setExpandedServers] = useState<string[]>([]);
  const [toolsDrawerServer, setToolsDrawerServer] = useState<McpServerInfo | null>(null);
  const [detailTabKey, setDetailTabKey] = useState<string>('tools');
  const [selectedRowKeys, setSelectedRowKeys] = useState<React.Key[]>([]);
  const [searchText, setSearchText] = useState('');
  const [filterSource, setFilterSource] = useState<string>('all');

  // ── Data fetching ───────────────────────────────────────────────────────────

  const { data: listData, isLoading: listLoading, refetch, isRefetching } = useQuery({
    queryKey: queryKeys.mcp.list(),
    queryFn: () => mcpApi.list(),
    select: (d) => ({ servers: d.servers ?? [], total: d.total ?? 0 }),
    refetchInterval: (query) => query.state.data?.servers?.some((server) =>
      ['queued', 'installing', 'starting', 'discovering'].includes(server.status)) ? 2000 : false,
  });
  const onActiveTabRefresh = useCallback(() => {
    void refetch();
  }, [refetch]);
  const handleTabClick = useTabRefresh(activeTab, onActiveTabRefresh);
  const onActiveDetailTabRefresh = useCallback(() => {
    if (!toolsDrawerServer) return;
    queryClient.invalidateQueries({ queryKey: ['mcp', 'serverMeta', toolsDrawerServer.name] });
  }, [queryClient, toolsDrawerServer]);
  const handleDetailTabClick = useTabRefresh(detailTabKey, onActiveDetailTabRefresh);

  // ── Global tool/resource cache (fetched once per server on mount) ───────────
  // useQueries fetches the full tool+resource lists for every server.
  // All callers (table, accordion, drawer) read from the same cache keyed by
  // server name, so there is only one network request per server.
  // stdio servers are excluded since the backend requires a URL to reach them.
  const serversList = listData?.servers ?? [];
  const serversCache = useQueries({
    queries: serversList.map((server) => ({
      queryKey: ['mcp', 'serverMeta', server.name] as const,
      queryFn: async (): Promise<{ name: string; tools: McpToolInfo[]; resources: McpResourceInfo[]; prompts: McpPromptInfo[] }> => {
        const [toolsResult, resourcesResult, promptsResult] = await Promise.allSettled([
          mcpApi.listTools(server.name),
          mcpApi.listResources(server.name),
          mcpApi.listPrompts(server.name),
        ]);
        return {
          name: server.name,
          tools: toolsResult.status === 'fulfilled' ? (toolsResult.value.tools ?? []) : [],
          resources: resourcesResult.status === 'fulfilled' ? (resourcesResult.value.resources ?? []) : [],
          prompts: promptsResult.status === 'fulfilled' ? (promptsResult.value.prompts ?? []) : [],
        };
      },
      staleTime: 5 * 60 * 1000,
      enabled: server.enabled && (server.transport !== 'stdio' || server.status === 'healthy'),
      retry: 1,
    })),
  });

  // Build a { serverName -> { tools, resources } } map for O(1) lookups.
  const serverMetaMap = useMemo(() => {
    const map = new Map<string, { tools: McpToolInfo[]; resources: McpResourceInfo[]; prompts: McpPromptInfo[] }>();
    serversCache.forEach((q) => {
      if (q.isSuccess && q.data) {
        map.set(q.data.name, { tools: q.data.tools, resources: q.data.resources, prompts: q.data.prompts });
      }
    });
    return map;
  }, [serversCache]);

  // ── WebSocket — invalidate on MCP events ──────────────────────────────────

  const qc = useQueryClient();
  const { connected: wsConnected } = useSystemEvents({
    autoConnect: true,
    onMcpUpdated: () => {
      qc.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      qc.invalidateQueries({ queryKey: queryKeys.mcp.stats() });
    },
  });

  const addMutation = useMutation({
    mutationFn: (values: McpFormValues) => {
      const stdio = values.transport === 'stdio'
        ? parseMcpStdioConfig(values.stdio_config ?? '')
        : null;
      return mcpApi.add({
        name: stdio?.name ?? values.name ?? '',
        transport: values.transport,
        command: stdio?.command,
        args: stdio?.args,
        env: stdio?.env,
        url: values.url,
        auth_type: values.auth_type,
        auth_token: values.auth_token,
        extra_headers: parseExtraHeaders(values.extra_headers),
        timeout_ms: values.timeout_ms,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.stats() });
      message.success(t('mcp.addSuccess'));
      closeDrawer();
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operationFailed'));
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({ name, values }: { name: string; values: McpFormValues }) => {
      const stdio = values.transport === 'stdio'
        ? parseMcpStdioConfig(values.stdio_config ?? '')
        : null;
      return mcpApi.update(name, {
        name: stdio?.name,
        transport: values.transport,
        command: stdio?.command,
        args: stdio?.args,
        env: stdio?.env,
        url: values.url,
        auth_type: values.auth_type,
        // Only send auth_token if user actually typed something; empty string means
        // "clear the token" — send undefined to preserve the existing value instead.
        auth_token: values.auth_token?.trim() ? values.auth_token.trim() : undefined,
        extra_headers: parseExtraHeaders(values.extra_headers),
        timeout_ms: values.timeout_ms,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      message.success(t('mcp.editSuccess'));
      closeDrawer();
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operationFailed'));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (name: string) => mcpApi.remove(name),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.stats() });
      message.success(t('mcp.deleteSuccess'));
      setSelectedRowKeys((prev) => prev.filter((k) => prev.includes(k)));
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operationFailed'));
    },
  });

  const toggleMutation = useMutation({
    mutationFn: ({ name, enabled }: { name: string; enabled: boolean }) =>
      mcpApi.toggle(name, enabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operationFailed'));
    },
  });

  const batchToggleMutation = useMutation({
    mutationFn: ({ names, enabled }: { names: string[]; enabled: boolean }) =>
      Promise.all(names.map((name) => mcpApi.toggle(name, enabled))),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      setSelectedRowKeys([]);
      message.success(t('mcp.batchSuccess'));
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operationFailed'));
    },
  });

  const testConnectionMutation = useMutation({
    mutationFn: (name: string) => mcpApi.testConnection(name),
    onSuccess: (result, name) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      if (result.success) {
        message.success(t('mcp.testSuccessLatency', { latency: result.latency_ms }));
      } else {
        message.error(t('mcp.testFailed', { error: result.error ?? 'unknown' }));
      }
    },
    onError: (err: Error) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      message.error(t('mcp.testFailed', { error: err.message ?? 'unknown' }));
    },
  });

  const retryMutation = useMutation({
    mutationFn: (name: string) => mcpApi.retry(name),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      message.success(t('mcp.retryQueued'));
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operationFailed'));
    },
  });

  // ── Handlers ───────────────────────────────────────────────────────────────

  const openAddDrawer = () => {
    setEditingServer(null);
    form.resetFields();
    form.setFieldsValue({
      transport: 'stdio',
      stdio_config: formatMcpStdioConfig(),
      auth_type: AUTH_TYPE_NONE,
      timeout_ms: 60000,
    });
    setDrawerOpen(true);
  };

  const openEditDrawer = (server: McpServerInfo) => {
    setEditingServer(server);
    form.setFieldsValue({
      name: server.name,
      transport: server.transport as McpFormValues['transport'],
      stdio_config: server.transport === 'stdio' ? formatMcpStdioConfig(server) : undefined,
      url: server.url ?? '',
      auth_type: server.auth?.auth_type ?? AUTH_TYPE_NONE,
      auth_token: '',
      extra_headers: extraHeadersToString(server.auth?.extra_headers),
      timeout_ms: server.auth?.timeout_ms ?? 60000,
    });
    setDrawerOpen(true);
  };

  const closeDrawer = () => {
    setDrawerOpen(false);
    setEditingServer(null);
    form.resetFields();
  };

  const handleDrawerSubmit = async () => {
    try {
      const values = await form.validateFields();
      if (editingServer) {
        updateMutation.mutate({ name: editingServer.name, values });
      } else {
        addMutation.mutate(values);
      }
    } catch {
      // Validation failed — antd will highlight the fields
    }
  };

  // ── Derived data ───────────────────────────────────────────────────────────

  const servers: McpServerInfo[] = listData?.servers ?? [];

  const filteredServers = useMemo(() => {
    let result = servers;
    if (searchText) {
      const q = searchText.toLowerCase();
      result = result.filter(
        (s) =>
          s.name.toLowerCase().includes(q) ||
          s.command?.toLowerCase().includes(q) ||
          s.url?.toLowerCase().includes(q),
      );
    }
    if (filterSource !== 'all') {
      result = result.filter((s) => {
        if (filterSource === 'builtin') return s.name.startsWith('builtin:');
        if (filterSource === 'custom') return !s.name.startsWith('builtin:');
        return true;
      });
    }
    if (activeTab === 'enabled') result = result.filter((s) => s.enabled);
    if (activeTab === 'connected') {
      result = result.filter(
        (s) =>
          s.enabled &&
          s.connection_status === 'connected',
      );
    }
    if (activeTab === 'disconnected') {
      result = result.filter(
        (s) =>
          s.enabled &&
          s.connection_status !== 'connected',
      );
    }
    if (activeTab === 'disabled') result = result.filter((s) => !s.enabled);
    return result;
  }, [servers, searchText, filterSource, activeTab]);

  const totalTools = Array.from(serverMetaMap.values()).reduce((acc, meta) => acc + meta.tools.length, 0);
  const totalResources = Array.from(serverMetaMap.values()).reduce((acc, meta) => acc + meta.resources.length, 0);
  const totalPrompts = Array.from(serverMetaMap.values()).reduce((acc, meta) => acc + meta.prompts.length, 0);
  const enabledCount = servers.filter((s) => s.enabled).length;
  const runtimeReadyCount = servers.filter(
    (s) => s.enabled && s.connection_status === 'connected',
  ).length;
  const remoteConnectedCount = servers.filter(
    (s) => s.enabled && s.transport !== 'stdio' && s.connection_status === 'connected',
  ).length;
  const failedServers = servers.filter((s) => s.enabled && s.connection_status === 'error');

  const connectionStatusTag = (server: McpServerInfo) => {
    const { connection_status: status, enabled } = server;
    if (!enabled) {
      return <Tag icon={<CloseCircleOutlined />} color="default">{t('mcp.status.disabled')}</Tag>;
    }
    const lifecycle = server.status;
    if (['queued', 'installing', 'starting', 'discovering'].includes(lifecycle)) {
      return (
        <Tag icon={<LoadingOutlined spin />} color="processing">
          {t(`mcp.status.${lifecycle}`)}
        </Tag>
      );
    }
    if (lifecycle === 'failed') {
      return <Tag icon={<CloseCircleOutlined />} color="error">{t('mcp.status.failed')}</Tag>;
    }
    const map: Record<McpServerInfo['connection_status'], { color: string; icon: React.ReactNode; labelKey: string }> = {
      connected: { color: 'success', icon: <CheckCircleOutlined />, labelKey: 'mcp.status.connected' },
      disconnected: { color: 'error', icon: <DisconnectOutlined />, labelKey: 'mcp.status.disconnected' },
      error: { color: 'error', icon: <CloseCircleOutlined />, labelKey: 'mcp.status.error' },
    };
    const entry = map[status] ?? { color: 'default', icon: null, labelKey: 'mcp.status.unknown' };
    return (
      <Tag icon={entry.icon} color={entry.color}>
        {t(entry.labelKey)}
      </Tag>
    );
  };

  const transportTag = (transport: string) => {
    const color = transport === 'stdio' ? 'blue' : transport === 'http' ? 'orange' : 'green';
    return <Tag color={color}>{transport}</Tag>;
  };

  // ── Columns ────────────────────────────────────────────────────────────────

  const columns: ColumnsType<McpServerInfo> = [
    {
      title: t('mcp.columns.name'),
      dataIndex: 'name',
      key: 'name',
      render: (v: string) => <b>{v}</b>,
    },
    {
      title: t('mcp.columns.transport'),
      dataIndex: 'transport',
      key: 'transport',
      render: transportTag,
    },
    {
      title: t('mcp.columns.auth'),
      key: 'auth',
      width: 100,
      render: (_: unknown, r: McpServerInfo) => {
        const auth = r.auth;
        if (!auth) return null;
        if (auth.auth_type === AUTH_TYPE_NONE) return <Tag>No Auth</Tag>;
        if (auth.auth_type === AUTH_TYPE_BEARER) {
          return (
            <Space size={4}>
              <Tag color="blue">Bearer</Tag>
              {auth.has_token && <Tag color="green">Token</Tag>}
            </Space>
          );
        }
        if (auth.auth_type === AUTH_TYPE_OAUTH) return <Tag color="purple">OAuth</Tag>;
        return <Tag>{auth.auth_type}</Tag>;
      },
    },
    {
      title: t('mcp.columns.command'),
      key: 'endpoint',
      render: (_: unknown, r: McpServerInfo) =>
        r.command ? (
          <code style={{ fontSize: 11 }}>{r.command} {r.args?.join(' ')}</code>
        ) : (
          <span style={{ color: 'var(--text-muted)', fontSize: 12 }}>{r.url}</span>
        ),
    },
    {
      title: t('mcp.columns.toolsCount'),
      dataIndex: 'tools_count',
      key: 'tools_count',
      render: (_: number, r: McpServerInfo) => {
        const meta = serverMetaMap.get(r.name);
        const toolsCount = meta?.tools.length ?? 0;
        const isLoading = r.enabled && meta === undefined;
        return toolsCount > 0 ? (
          <Tooltip title={t('mcp.viewTools')}>
            <Button
              type="link"
              size="small"
              style={{ padding: 0, height: 'auto', fontSize: 13 }}
              onClick={() => setToolsDrawerServer(r)}
            >
              {toolsCount}
            </Button>
          </Tooltip>
        ) : (
          <Text type="secondary" style={{ fontSize: 13 }}>
            {isLoading ? <LoadingOutlined /> : '0'}
          </Text>
        );
      },
      width: 80,
    },
    {
      title: t('mcp.columns.status'),
      key: 'connection_status',
      render: (_: unknown, r: McpServerInfo) => (
        <Space direction="vertical" size={0}>
          {connectionStatusTag(r)}
          {r.last_error && r.enabled && (
            <span style={{ fontSize: 10, color: 'var(--color-error)', maxWidth: 200 }} title={r.last_error}>
              {r.last_error.length > 40 ? `${r.last_error.slice(0, 40)}...` : r.last_error}
            </span>
          )}
        </Space>
      ),
    },
    {
      title: t('mcp.columns.actions'),
      key: 'action',
      width: 340,
      render: (_: unknown, r: McpServerInfo) => (
        <Space>
          <Tooltip title={!canWrite ? t('common.noPermission') : undefined}>
            <span>
              <Switch
                size="small"
                checked={r.enabled}
                disabled={!canWrite}
                loading={toggleMutation.isPending}
                onChange={(checked) => toggleMutation.mutate({ name: r.name, enabled: checked })}
              />
            </span>
          </Tooltip>
          <Button
            size="small"
            icon={<ThunderboltOutlined />}
            onClick={() => {
              setTestingServer(r.name);
              testConnectionMutation.mutate(r.name, { onSettled: () => setTestingServer(null) });
            }}
            loading={testingServer === r.name}
            disabled={!r.enabled}
            title={!r.enabled ? t('mcp.testDisabled') : t('mcp.testConnection')}
          />
          {canWrite && r.transport === 'stdio' && r.status === 'failed' && (
            <Button
              size="small"
              icon={<ReloadOutlined />}
              onClick={() => retryMutation.mutate(r.name)}
              loading={retryMutation.isPending}
            >
              {t('mcp.retry')}
            </Button>
          )}
          {canWrite && (
            <Button size="small" icon={<EditOutlined />} onClick={() => openEditDrawer(r)}>
              {t('mcp.edit')}
            </Button>
          )}
          {canDelete && (
            <Popconfirm
              title={t('mcp.deleteConfirm', { name: r.name })}
              onConfirm={() => deleteMutation.mutate(r.name)}
              okText={t('common.delete')}
              cancelText={t('common.cancel')}
              okButtonProps={{ danger: true }}
            >
              <Button size="small" danger icon={<DeleteOutlined />} loading={deleteMutation.isPending}>
                {t('mcp.delete')}
              </Button>
            </Popconfirm>
          )}
        </Space>
      ),
    },
  ];

  // ── Render ─────────────────────────────────────────────────────────────────

  const isSubmitting = addMutation.isPending || updateMutation.isPending;
  const rowSelection = canWrite ? { selectedRowKeys, onChange: setSelectedRowKeys } : undefined;
  const selectedServers = servers.filter((s) => selectedRowKeys.includes(s.name));
  const selectedEnabled = selectedServers.filter((s) => s.enabled);
  const selectedDisabled = selectedServers.filter((s) => !s.enabled);

  const tabItems = [
    {
      key: 'all',
      label: `${t('common.all')} (${servers.length})`,
      children: filteredServers,
    },
    {
      key: 'connected',
      label: `${t('mcp.status.runtimeReady')} (${runtimeReadyCount})`,
      children: servers.filter(
        (s) =>
          s.enabled &&
          s.connection_status === 'connected',
      ),
    },
    {
      key: 'enabled',
      label: `${t('common.enabled')} (${enabledCount})`,
      children: servers.filter((s) => s.enabled),
    },
    {
      key: 'disabled',
      label: `${t('common.disabled')} (${servers.filter((s) => !s.enabled).length})`,
      children: servers.filter((s) => !s.enabled),
    },
  ];

  const tabChildren = tabItems.find((item) => item.key === activeTab)?.children ?? servers;

  return (
    <div style={{ padding: '24px 24px 0' }}>
      {/* Header */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
        <Space direction="vertical" size={4}>
          <Title level={3} style={{ margin: 0 }}>{t('mcp.title')}</Title>
          <Space size="middle">
            <Badge
              status={runtimeReadyCount === servers.length && servers.length > 0 ? 'success' : servers.length > 0 ? 'warning' : 'default'}
              text={<span style={{ fontSize: 12, color: 'var(--text-muted)' }}>{servers.length} servers · {runtimeReadyCount}/{servers.length} {t('mcp.status.runtimeReady')} · {remoteConnectedCount} {t('mcp.status.connected')}</span>}
            />
            {wsConnected && <Badge status="success" text="WebSocket" />}
          </Space>
        </Space>
        <Space>
          <Button icon={<ReloadOutlined spin={listLoading || isRefetching} />} onClick={() => refetch()} loading={isRefetching}>
            {t('common.refresh')}
          </Button>
          {canWrite && (
            <Button type="primary" icon={<PlusOutlined />} onClick={openAddDrawer}>
              {t('mcp.add')}
            </Button>
          )}
        </Space>
      </div>

      {/* Stats cards */}
      {servers.length > 0 && (
        <Row gutter={12} style={{ marginBottom: 16 }}>
          <Col span={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic title={t('common.total')} value={servers.length} valueStyle={{ fontSize: 22 }} />
            </Card>
          </Col>
          <Col span={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic title={t('common.enabled')} value={enabledCount} valueStyle={{ fontSize: 22, color: '#3fb950' }} />
            </Card>
          </Col>
          <Col span={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('mcp.toolsTab')}
                value={totalTools}
                suffix={<span style={{ fontSize: 13, color: '#999' }}>/ {(servers.filter((s) => (s.tools?.length ?? 0) > 0)).length} servers</span>}
                valueStyle={{ fontSize: 22, color: '#0969da' }}
              />
            </Card>
          </Col>
          <Col span={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={`${t('mcp.resourcesTab')} / ${t('mcp.promptsTab')}`}
                value={`${totalResources} / ${totalPrompts}`}
                valueStyle={{ fontSize: 18, color: '#8250df' }}
              />
            </Card>
          </Col>
        </Row>
      )}

      {/* Degraded startup banner */}
      {failedServers.length > 0 && (
        <Alert
          type="warning"
          showIcon
          icon={<InfoCircleOutlined />}
          message={t('mcp.degradedStartup')}
          description={
            <Space wrap>
              {failedServers.map((s) => (
                <Tag key={s.name} color="error" icon={<CloseCircleOutlined />}>
                  {s.name}: {s.last_error?.slice(0, 60) ?? t('mcp.status.error')}
                </Tag>
              ))}
            </Space>
          }
          style={{ marginBottom: 16 }}
          closable
        />
      )}

      {/* Table */}
      <Card>
        {/* Batch operations bar */}
        {rowSelection && selectedRowKeys.length > 0 && (
          <Alert
            type="info"
            showIcon
            style={{ marginBottom: 12 }}
            message={
              <Space>
                <span>{t('common.selected')}: {selectedRowKeys.length}</span>
                {selectedDisabled.length > 0 && (
                  <Button size="small" onClick={() => batchToggleMutation.mutate({ names: selectedDisabled.map((s) => s.name), enabled: true })} loading={batchToggleMutation.isPending}>
                    {t('common.enable')} ({selectedDisabled.length})
                  </Button>
                )}
                {selectedEnabled.length > 0 && (
                  <Button size="small" onClick={() => batchToggleMutation.mutate({ names: selectedEnabled.map((s) => s.name), enabled: false })} loading={batchToggleMutation.isPending}>
                    {t('common.disable')} ({selectedEnabled.length})
                  </Button>
                )}
                {canDelete && (
                  <Popconfirm
                    title={t('mcp.deleteConfirm', { name: '' })}
                    description={`${t('mcp.batchDeleteConfirm', { count: selectedRowKeys.length })}`}
                    onConfirm={() => selectedRowKeys.forEach((k) => deleteMutation.mutate(k as string))}
                  >
                    <Button size="small" danger loading={deleteMutation.isPending}>
                      {t('common.delete')} ({selectedRowKeys.length})
                    </Button>
                  </Popconfirm>
                )}
                <Button size="small" onClick={() => setSelectedRowKeys([])}>{t('common.clear')}</Button>
              </Space>
            }
          />
        )}

        <Tabs
          activeKey={activeTab}
          onChange={(key) => { setActiveTab(key); setSelectedRowKeys([]); }}
          onTabClick={handleTabClick}
          tabBarExtraContent={
            <Space>
              <Input
                prefix={<SearchOutlined />}
                placeholder={t('common.search')}
                size="small"
                style={{ width: 200 }}
                value={searchText}
                onChange={(e) => setSearchText(e.target.value)}
                allowClear
              />
              <Select
                size="small"
                value={filterSource}
                onChange={setFilterSource}
                style={{ width: 120 }}
                options={[
                  { value: 'all', label: t('mcp.filterAll') },
                  { value: 'builtin', label: t('mcp.filterBuiltin') },
                  { value: 'custom', label: t('mcp.filterCustom') },
                ]}
              />
            </Space>
          }
          items={[
            {
              key: 'all',
              label: `${t('common.all')} (${servers.length})`,
              children: (
                <Table
                  columns={columns}
                  dataSource={filteredServers}
                  rowKey="name"
                  loading={listLoading}
                  pagination={{ pageSize: 20 }}
                  scroll={{ x: 'max-content' }}
                  rowSelection={rowSelection}
                  locale={{
                    emptyText: (
                      <Space direction="vertical" style={{ padding: '40px 0' }}>
                        <Title level={5} style={{ margin: 0 }}>{t('mcp.empty.title')}</Title>
                        <span style={{ color: 'var(--text-muted)' }}>{t('mcp.empty.description')}</span>
                        {canWrite && <Button type="primary" icon={<PlusOutlined />} onClick={openAddDrawer}>{t('mcp.add')}</Button>}
                      </Space>
                    ),
                  }}
                />
              ),
            },
            {
              key: 'connected',
              label: `${t('mcp.status.connected')} (${servers.filter((s) => s.enabled && s.connection_status === 'connected').length})`,
              children: <Table columns={columns} dataSource={filteredServers} rowKey="name" loading={listLoading} pagination={{ pageSize: 20 }} scroll={{ x: 'max-content' }} rowSelection={rowSelection} />,
            },
            {
              key: 'enabled',
              label: `${t('common.enabled')} (${enabledCount})`,
              children: <Table columns={columns} dataSource={filteredServers} rowKey="name" loading={listLoading} pagination={{ pageSize: 20 }} scroll={{ x: 'max-content' }} rowSelection={rowSelection} />,
            },
            {
              key: 'disabled',
              label: `${t('common.disabled')} (${servers.filter((s) => !s.enabled).length})`,
              children: <Table columns={columns} dataSource={filteredServers} rowKey="name" loading={listLoading} pagination={{ pageSize: 20 }} scroll={{ x: 'max-content' }} rowSelection={rowSelection} />,
            },
          ]}
        />
      </Card>

      {/* Tools & Resources panel (collapsed summary) */}
      {servers.length > 0 && (
        <Card style={{ marginTop: 16 }}>
          <Title level={5} style={{ margin: '0 0 16px' }}>{t('mcp.tools')}</Title>
          <Collapse
            accordion
            activeKey={expandedServers}
            onChange={(keys) => setExpandedServers(Array.isArray(keys) ? keys : [keys])}
            items={servers.map((server) => {
              const meta = serverMetaMap.get(server.name);
              const tools = meta?.tools ?? [];
              const resources = meta?.resources ?? [];
              const prompts = meta?.prompts ?? [];
              const isLoading = server.enabled && meta === undefined;
              return {
                key: server.name,
                label: (
                  <Space>
                    {connectionStatusTag(server)}
                    <b>{server.name}</b>
                    <Tag color="blue">{server.transport}</Tag>
                    <Tag>
                      {isLoading ? <LoadingOutlined /> : tools.length} {t('mcp.toolsTab')}
                    </Tag>
                    <Tag>
                      {isLoading ? <LoadingOutlined /> : resources.length} {t('mcp.resourcesTab')}
                    </Tag>
                    <Tag>
                      {isLoading ? <LoadingOutlined /> : prompts.length} {t('mcp.promptsTab')}
                    </Tag>
                    <Button
                      type="link"
                      size="small"
                      onClick={(e) => { e.stopPropagation(); setToolsDrawerServer(server); }}
                    >
                      {t('mcp.viewTools')}
                    </Button>
                  </Space>
                ),
                children: isLoading ? (
                  <Alert message={t('common.loading')} type="info" showIcon />
                ) : (
                  <Space direction="vertical" style={{ width: '100%' }}>
                    {/* Tools */}
                    <div>
                      <Space style={{ marginBottom: 8 }}>
                        <FileTextOutlined />
                        <Text strong>{t('mcp.toolsTab')} ({tools.length})</Text>
                      </Space>
                      {tools.length === 0 ? (
                        <Alert message={t('mcp.noTools')} type="info" showIcon />
                      ) : (
                        tools.map((tool: McpToolInfo) => (
                          <Alert key={tool.name} message={tool.name} description={tool.description} type="info" showIcon style={{ marginBottom: 6 }} />
                        ))
                      )}
                    </div>
                    {/* Resources */}
                    <div>
                      <Space style={{ marginBottom: 8 }}>
                        <HolderOutlined />
                        <Text strong>{t('mcp.resourcesTab')} ({resources.length})</Text>
                      </Space>
                      {resources.length === 0 ? (
                        <Alert message={t('mcp.noResources')} type="info" showIcon />
                      ) : (
                        <Table
                          dataSource={resources}
                          rowKey="uri"
                          size="small"
                          pagination={false}
                          columns={[
                            { title: t('mcp.resourceUri'), dataIndex: 'uri', key: 'uri', render: (v: string) => <code style={{ fontSize: 11 }}>{v}</code> },
                            { title: t('mcp.resourceName'), dataIndex: 'name', key: 'name' },
                            { title: t('mcp.resourceDescription'), dataIndex: 'description', key: 'description', ellipsis: true },
                          ]}
                        />
                      )}
                    </div>
                    {/* Prompts */}
                    <div>
                      <Space style={{ marginBottom: 8 }}>
                        <FileTextOutlined />
                        <Text strong>{t('mcp.promptsTab')} ({prompts.length})</Text>
                      </Space>
                      {prompts.length === 0 ? (
                        <Alert message={t('mcp.noPrompts')} type="info" showIcon />
                      ) : (
                        prompts.map((prompt: McpPromptInfo) => (
                          <Alert
                            key={prompt.name}
                            message={prompt.name}
                            description={prompt.description}
                            type="info"
                            showIcon
                            style={{ marginBottom: 6 }}
                          />
                        ))
                      )}
                    </div>
                  </Space>
                ),
              };
            })}
          />
        </Card>
      )}

      {/* Add/Edit Drawer */}
      <Drawer
        title={editingServer ? t('mcp.form.editTitle') : t('mcp.form.title')}
        open={drawerOpen}
        onClose={closeDrawer}
        width={480}
        footer={
          <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
            <Button onClick={closeDrawer}>{t('common.cancel')}</Button>
            <Button type="primary" loading={isSubmitting} onClick={handleDrawerSubmit}>
              {editingServer ? t('common.confirm') : t('mcp.add')}
            </Button>
          </Space>
        }
      >
        <Form form={form} layout="vertical" requiredMark="optional" disabled={isSubmitting}>
          <Form.Item name="transport" label={t('mcp.form.transport')} rules={[{ required: true, message: t('mcp.form.required') }]}>
            <Select
              placeholder={t('mcp.form.transport')}
              onChange={(transport) => {
                if (transport === 'stdio' && !form.getFieldValue('stdio_config')) {
                  form.setFieldValue('stdio_config', formatMcpStdioConfig(editingServer ?? undefined));
                }
                void form.validateFields(transport === 'stdio' ? ['stdio_config'] : ['name', 'url']);
              }}
            >
              <Select.Option value="stdio">{t('mcp.transportStdio')}</Select.Option>
              <Select.Option value="http">{t('mcp.transportHttp')}</Select.Option>
              <Select.Option value="sse">{t('mcp.transportSSE')}</Select.Option>
            </Select>
          </Form.Item>

          <Divider plain style={{ margin: '8px 0 16px' }}>{t('mcp.form.serverConfiguration')}</Divider>

          <Form.Item noStyle shouldUpdate={(prev, curr) => prev.transport !== curr.transport}>
            {({ getFieldValue }) =>
              getFieldValue('transport') === 'stdio' ? (
                <Form.Item
                  name="stdio_config"
                  label={t('mcp.form.stdioJson')}
                  extra={t('mcp.form.stdioJsonHelp')}
                  rules={[
                    { required: true, message: t('mcp.form.required') },
                    {
                      validator: (_, value?: string) => {
                        try {
                          parseMcpStdioConfig(value ?? '');
                          return Promise.resolve();
                        } catch (error) {
                          return Promise.reject(new Error(stdioConfigErrorMessage(error, t)));
                        }
                      },
                    },
                  ]}
                >
                  <Input.TextArea
                    rows={16}
                    spellCheck={false}
                    placeholder={formatMcpStdioConfig()}
                    style={{ fontFamily: 'monospace' }}
                  />
                </Form.Item>
              ) : (
                <>
                  <Form.Item
                    name="name"
                    label={t('mcp.form.name')}
                    rules={[
                      { required: true, message: t('mcp.form.required') },
                      { pattern: /^[a-zA-Z0-9_-]+$/, message: t('mcp.form.invalidName') },
                    ]}
                  >
                    <Input placeholder={t('mcp.form.namePlaceholder')} disabled={!!editingServer} />
                  </Form.Item>
                  <Form.Item name="url" label={t('mcp.form.url')} rules={[{ required: true, message: t('mcp.form.required') }]}>
                    <Input placeholder={t('mcp.form.urlPlaceholder')} />
                  </Form.Item>
                </>
              )
            }
          </Form.Item>

          <Form.Item noStyle shouldUpdate={(prev, curr) => prev.transport !== curr.transport || prev.auth_type !== curr.auth_type}>
            {({ getFieldValue }) => getFieldValue('transport') === 'stdio' ? null : (
              <>
                <Divider plain style={{ margin: '8px 0 16px' }}>{t('mcp.form.authentication')}</Divider>
                <Form.Item name="auth_type" label={t('mcp.form.authType')} tooltip={t('mcp.form.authTypeHelp')}>
                  <Select placeholder={t('mcp.form.authTypePlaceholder')}>
                    <Select.Option value={AUTH_TYPE_NONE}>{t('mcp.form.authNone')}</Select.Option>
                    <Select.Option value={AUTH_TYPE_BEARER}>Bearer Token</Select.Option>
                    <Select.Option value={AUTH_TYPE_OAUTH}>OAuth 2.0</Select.Option>
                  </Select>
                </Form.Item>
                {getFieldValue('auth_type') === AUTH_TYPE_BEARER ? (
                  <Form.Item
                    name="auth_token"
                    label={t('mcp.form.authToken')}
                    extra={editingServer?.auth?.has_token ? t('mcp.form.authTokenKeepExisting') : undefined}
                  >
                    <Input.Password placeholder={editingServer?.auth?.has_token ? '••••••••' : t('mcp.form.authTokenPlaceholder')} />
                  </Form.Item>
                ) : null}
                <Form.Item name="timeout_ms" label={t('mcp.form.timeout')} extra={t('mcp.form.timeoutHelp')}>
                  <InputNumber
                    min={1000}
                    max={300000}
                    step={1000}
                    style={{ width: '100%' }}
                    placeholder="60000"
                    formatter={(value) => `${value}ms`}
                    parser={(value) => Number.parseInt(value?.replace(/ms$/, '').trim() ?? '0', 10) as 1000 | 300000}
                  />
                </Form.Item>
                <Form.Item
                  name="extra_headers"
                  label={t('mcp.form.extraHeaders')}
                  extra={<Text type="secondary" style={{ fontSize: 11 }}>{t('mcp.form.extraHeadersHelp')}</Text>}
                >
                  <Input.TextArea placeholder='{"X-Api-Key": "..."}' rows={2} style={{ fontFamily: 'monospace' }} />
                </Form.Item>
              </>
            )}
          </Form.Item>
        </Form>
      </Drawer>

      {/* Tools & Resources & Prompts detail Drawer */}
      <Drawer
        title={
          <Space>
            <FileTextOutlined />
            <span>{toolsDrawerServer?.name} — {t('mcp.tools')}</span>
          </Space>
        }
        open={!!toolsDrawerServer}
        onClose={() => {
          setToolsDrawerServer(null);
          setDetailTabKey('tools');
        }}
        width={640}
        destroyOnHidden
      >
        {toolsDrawerServer && (() => {
          const meta = toolsDrawerServer ? serverMetaMap.get(toolsDrawerServer.name) : undefined;
          const isLoading = toolsDrawerServer.enabled && meta === undefined;
          const tools = meta?.tools ?? [];
          const resources = meta?.resources ?? [];
          const prompts = meta?.prompts ?? [];
          return (
            <Tabs
              activeKey={detailTabKey}
              onChange={setDetailTabKey}
              onTabClick={handleDetailTabClick}
              items={[
                {
                  key: 'tools',
                  label: `${t('mcp.toolsTab')} (${isLoading ? '…' : tools.length})`,
                  children: (
                    <div>
                      <Descriptions size="small" column={2} style={{ marginBottom: 16 }}>
                        <Descriptions.Item label={t('mcp.form.transport')}><Tag>{toolsDrawerServer.transport}</Tag></Descriptions.Item>
                        <Descriptions.Item label={t('mcp.columns.status')}>{connectionStatusTag(toolsDrawerServer)}</Descriptions.Item>
                        <Descriptions.Item label="Auth" span={2}><Tag>{toolsDrawerServer.auth?.auth_type ?? 'none'}</Tag></Descriptions.Item>
                      </Descriptions>
                      {isLoading ? (
                        <Alert message={t('common.loading')} type="info" showIcon />
                      ) : tools.length === 0 ? (
                        <Alert message={t('mcp.noTools')} type="info" showIcon />
                      ) : (
                        tools.map((tool: McpToolInfo) => (
                          <Alert
                            key={tool.name}
                            message={<b>{tool.name}</b>}
                            description={
                              <Space direction="vertical" size={4}>
                                <Text type="secondary" style={{ fontSize: 12 }}>{tool.description}</Text>
                                {tool.inputSchema && (
                                  <details>
                                    <summary style={{ cursor: 'pointer', fontSize: 11, color: 'var(--text-muted)' }}>
                                      {t('skills.inputSchema')}
                                    </summary>
                                    <pre style={{ fontSize: 11, background: 'var(--bg-void)', padding: 8, borderRadius: 4, overflow: 'auto' }}>
                                      {JSON.stringify(tool.inputSchema, null, 2)}
                                    </pre>
                                  </details>
                                )}
                              </Space>
                            }
                            type="info"
                            showIcon
                            style={{ marginBottom: 8 }}
                          />
                        ))
                      )}
                    </div>
                  ),
                },
                {
                  key: 'resources',
                  label: `${t('mcp.resourcesTab')} (${isLoading ? '…' : resources.length})`,
                  children: isLoading ? (
                    <Alert message={t('common.loading')} type="info" showIcon />
                  ) : resources.length === 0 ? (
                    <Alert message={t('mcp.noResources')} type="info" showIcon />
                  ) : (
                    <Table
                      dataSource={resources}
                      rowKey="uri"
                      size="small"
                      pagination={false}
                      columns={[
                        { title: t('mcp.resourceUri'), dataIndex: 'uri', key: 'uri', render: (v: string) => <code style={{ fontSize: 11 }}>{v}</code> },
                        { title: t('mcp.resourceName'), dataIndex: 'name', key: 'name' },
                        { title: t('mcp.resourceDescription'), dataIndex: 'description', key: 'description', ellipsis: true },
                      ]}
                    />
                  ),
                },
                {
                  key: 'prompts',
                  label: `${t('mcp.promptsTab')} (${isLoading ? '…' : prompts.length})`,
                  children: isLoading ? (
                    <Alert message={t('common.loading')} type="info" showIcon />
                  ) : prompts.length === 0 ? (
                    <Alert message={t('mcp.noPrompts')} type="info" showIcon />
                  ) : (
                    <Space direction="vertical" style={{ width: '100%' }}>
                      {prompts.map((prompt: McpPromptInfo) => (
                        <Alert
                          key={prompt.name}
                          message={<b>{prompt.name}</b>}
                          description={
                            <Space direction="vertical" size={4}>
                              {prompt.description && (
                                <Text type="secondary" style={{ fontSize: 12 }}>{prompt.description}</Text>
                              )}
                              {prompt.arguments?.length ? (
                                <Space wrap>
                                  {prompt.arguments.map((arg) => (
                                    <Tag key={arg.name} color={arg.required ? 'orange' : 'default'}>
                                      {arg.name}{arg.required ? ' *' : ''}
                                    </Tag>
                                  ))}
                                </Space>
                              ) : null}
                            </Space>
                          }
                          type="info"
                          showIcon
                        />
                      ))}
                    </Space>
                  ),
                },
              ]}
            />
          );
        })()}
      </Drawer>
    </div>
  );
}
