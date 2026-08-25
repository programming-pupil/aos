import { useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  Alert,
  AutoComplete,
  Button,
  Card,
  Drawer,
  Empty,
  Form,
  Input,
  InputNumber,
  Popconfirm,
  Space,
  Table,
  Tag,
  Tooltip,
  Switch,
  Typography,
  message,
} from 'antd';
import {
  BranchesOutlined,
  CaretDownOutlined,
  CaretRightOutlined,
  CheckCircleOutlined,
  CodeOutlined,
  DeleteOutlined,
  FileOutlined,
  FileTextOutlined,
  FolderOpenOutlined,
  LinkOutlined,
  PlusOutlined,
  ReloadOutlined,
  SearchOutlined,
  SyncOutlined,
  EditOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { languageFromPath, LazyCodeHighlighter } from '@/components/code/LazyCodeHighlighter';
import type { RdFileNode, RdRepository, RdRepositoryListResponse } from '@/types';

dayjs.extend(relativeTime);

const { Text, Title } = Typography;

function isGitRepositoryUrl(value?: string): boolean {
  const normalized = value?.trim() ?? '';
  if (!normalized) return false;
  return (
    /^https?:\/\/\S+$/i.test(normalized) ||
    /^ssh:\/\/\S+$/i.test(normalized) ||
    /^git@[^:]+:.+$/i.test(normalized)
  );
}

function repositoryNameFromUrl(value?: string): string | undefined {
  const normalized = value?.trim();
  if (!normalized) return undefined;
  const scpLikePath = normalized.match(/^git@[^:]+:(.+)$/)?.[1];
  if (scpLikePath) {
    return scpLikePath.split(/[/?#]/).filter(Boolean).pop()?.replace(/\.git$/i, '').trim() || undefined;
  }
  try {
    const parsed = new URL(normalized.includes('://') ? normalized : `https://${normalized}`);
    const segment = parsed.pathname.split('/').filter(Boolean).pop()?.replace(/\.git$/i, '');
    return segment?.trim() || undefined;
  } catch {
    const segment = normalized.split(/[?#]/)[0].split('/').filter(Boolean).pop();
    return segment?.replace(/\.git$/i, '').trim() || undefined;
  }
}

const FILE_ICON: Record<string, ReactNode> = {
  rust: <CodeOutlined style={{ color: '#dea584' }} />,
  typescript: <CodeOutlined style={{ color: '#3178c6' }} />,
  javascript: <CodeOutlined style={{ color: '#f7df1e' }} />,
  java: <CodeOutlined style={{ color: '#f97316' }} />,
  vue: <CodeOutlined style={{ color: '#42b883' }} />,
  svelte: <CodeOutlined style={{ color: '#ff3e00' }} />,
  html: <CodeOutlined style={{ color: '#e34c26' }} />,
  css: <CodeOutlined style={{ color: '#2563eb' }} />,
  scss: <CodeOutlined style={{ color: '#c6538c' }} />,
  less: <CodeOutlined style={{ color: '#1d365d' }} />,
  python: <CodeOutlined style={{ color: '#3776ab' }} />,
  go: <CodeOutlined style={{ color: '#00add8' }} />,
  sql: <CodeOutlined style={{ color: '#38bdf8' }} />,
  json: <FileTextOutlined style={{ color: '#d4a72c' }} />,
  markdown: <FileTextOutlined style={{ color: '#60a5fa' }} />,
  default: <FileOutlined style={{ color: '#8a8a8a' }} />,
};

function getFileIcon(language?: string | null) {
  return FILE_ICON[language ?? ''] ?? FILE_ICON.default;
}

function formatSize(bytes?: number | null): string {
  if (bytes == null) return '';
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

function flattenTree(nodes: RdFileNode[]): RdFileNode[] {
  const out: RdFileNode[] = [];
  const walk = (items: RdFileNode[]) => {
    items.forEach((item) => {
      out.push({ ...item, children: undefined });
      if (item.children) walk(item.children);
    });
  };
  walk(nodes);
  return out;
}

function RepositoryDetectionSummary({ repository }: { repository: RdRepository }) {
  const { t } = useTranslation();
  const testCommand = repository.defaultTestCommand ?? repository.detectedTestCommand;
  const buildCommand = repository.defaultBuildCommand ?? repository.detectedBuildCommand;
  const stacks = repository.detectedStack ?? [];
  const languages = repository.detectedLanguages ?? [];

  if (!stacks.length && !languages.length && !testCommand && !buildCommand) {
    return <Text type="secondary">{t('projects.noDetection')}</Text>;
  }

  return (
    <Space direction="vertical" size={4} style={{ maxWidth: 340 }}>
      {stacks.length ? (
        <Space size={4} wrap>
          {stacks.slice(0, 6).map((item) => <Tag key={item} color="cyan">{item}</Tag>)}
          {stacks.length > 6 ? <Tag>+{stacks.length - 6}</Tag> : null}
        </Space>
      ) : null}
      {languages.length ? (
        <Space size={4} wrap>
          {languages.slice(0, 5).map((item) => (
            <Tag key={item.language}>
              {item.language} · {item.fileCount}
            </Tag>
          ))}
        </Space>
      ) : null}
      <Space direction="vertical" size={0}>
        <Text type="secondary" style={{ fontSize: 12 }}>
          {t('projects.testCommandShort')}: {testCommand ? <Text code>{testCommand}</Text> : '-'}
        </Text>
        <Text type="secondary" style={{ fontSize: 12 }}>
          {t('projects.buildCommandShort')}: {buildCommand ? <Text code>{buildCommand}</Text> : '-'}
        </Text>
      </Space>
    </Space>
  );
}

function FileTreeItem({
  node,
  depth,
  selectedPath,
  onSelect,
}: {
  node: RdFileNode;
  depth: number;
  selectedPath?: string;
  onSelect: (node: RdFileNode) => void;
}) {
  const isFile = node.nodeType === 'file';
  const selected = selectedPath === node.path;
  const [expanded, setExpanded] = useState(true);
  const hasChildren = !isFile && Boolean(node.children?.length);

  const handleClick = () => {
    if (isFile) {
      onSelect(node);
      return;
    }
    if (hasChildren) setExpanded((value) => !value);
  };
  return (
    <>
      <div
        onClick={handleClick}
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 8,
          cursor: isFile || hasChildren ? 'pointer' : 'default',
          padding: '6px 10px',
          paddingLeft: 10 + depth * 16,
          borderRadius: 8,
          background: selected ? 'rgba(20, 184, 166, 0.14)' : 'transparent',
          color: selected ? '#0f766e' : 'var(--text-primary)',
          fontSize: 13,
        }}
      >
        {isFile ? getFileIcon(node.language) : (
          <Space size={2}>
            {hasChildren ? (expanded ? <CaretDownOutlined /> : <CaretRightOutlined />) : null}
            <FolderOpenOutlined style={{ color: '#d6a62a' }} />
          </Space>
        )}
        <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{node.name}</span>
        {isFile ? <span style={{ fontSize: 11, color: 'var(--text-muted)' }}>{formatSize(node.sizeBytes)}</span> : null}
      </div>
      {expanded && node.children?.map((child) => (
        <FileTreeItem key={child.path} node={child} depth={depth + 1} selectedPath={selectedPath} onSelect={onSelect} />
      ))}
    </>
  );
}

function RepositoryBrowser({ repository, open, onClose }: { repository: RdRepository | null; open: boolean; onClose: () => void }) {
  const { t } = useTranslation();
  const [search, setSearch] = useState('');
  const [selectedPath, setSelectedPath] = useState<string | undefined>();

  useEffect(() => {
    setSearch('');
    setSelectedPath(undefined);
  }, [repository?.id]);

  const treeQuery = useQuery({
    queryKey: repository ? queryKeys.rd.repositoryTree(repository.id) : ['rd', 'tree', 'none'],
    queryFn: () => rdApi.repositoryTree(repository!.id),
    enabled: open && !!repository?.id,
  });

  const fileQuery = useQuery({
    queryKey: repository && selectedPath ? queryKeys.rd.repositoryFile(repository.id, selectedPath) : ['rd', 'file', 'none'],
    queryFn: () => rdApi.repositoryFile(repository!.id, selectedPath!),
    enabled: open && !!repository?.id && !!selectedPath,
  });

  const nodes = treeQuery.data ?? [];
  const visibleNodes = useMemo(() => {
    const keyword = search.trim().toLowerCase();
    if (!keyword) return nodes;
    return flattenTree(nodes).filter((node) => node.path.toLowerCase().includes(keyword) || node.name.toLowerCase().includes(keyword));
  }, [nodes, search]);

  return (
    <Drawer
      title={
        <Space>
          <FolderOpenOutlined />
          <span>{repository?.name}</span>
          {repository?.branch ? <Tag color="blue">{repository.branch}</Tag> : null}
        </Space>
      }
      placement="right"
      width={980}
      onClose={onClose}
      open={open}
      styles={{ body: { padding: 0, display: 'flex', flexDirection: 'column' } }}
    >
      <div style={{ padding: 12, borderBottom: '1px solid var(--border-subtle)' }}>
        <Input
          allowClear
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          prefix={<SearchOutlined />}
          placeholder={t('projects.searchPlaceholder')}
        />
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '330px minmax(0, 1fr)', minHeight: 0, flex: 1 }}>
        <div style={{ borderRight: '1px solid var(--border-subtle)', overflow: 'auto', padding: 8 }}>
          {treeQuery.isLoading ? (
            <div style={{ padding: 24 }}>{t('common.loading')}</div>
          ) : visibleNodes.length === 0 ? (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={search ? t('projects.noMatch') : t('projects.emptyDir')} />
          ) : search ? (
            visibleNodes.map((node) => (
              <FileTreeItem key={node.path} node={node} depth={0} selectedPath={selectedPath} onSelect={(item) => item.nodeType === 'file' && setSelectedPath(item.path)} />
            ))
          ) : (
            nodes.map((node) => (
              <FileTreeItem key={node.path} node={node} depth={0} selectedPath={selectedPath} onSelect={(item) => item.nodeType === 'file' && setSelectedPath(item.path)} />
            ))
          )}
        </div>
        <div style={{ minWidth: 0, overflow: 'hidden', display: 'flex', flexDirection: 'column' }}>
          {selectedPath ? (
            <>
              <div style={{ padding: '10px 14px', borderBottom: '1px solid var(--border-subtle)', display: 'flex', justifyContent: 'space-between' }}>
                <Text strong>{selectedPath}</Text>
                {fileQuery.data ? <Text type="secondary">{formatSize(fileQuery.data.sizeBytes)} · {fileQuery.data.language ?? 'text'}</Text> : null}
              </div>
              <div style={{ flex: 1, minHeight: 0, overflow: 'auto', background: '#0b1220' }}>
                {fileQuery.isLoading ? (
                  <div style={{ padding: 16, color: '#dbeafe' }}>{t('common.loading')}</div>
                ) : (
                  <LazyCodeHighlighter
                    code={fileQuery.data?.content ?? ''}
                    language={languageFromPath(fileQuery.data?.language, selectedPath)}
                    showLineNumbers
                    style={{
                      minHeight: '100%',
                      margin: 0,
                      padding: 16,
                      background: '#0b1220',
                      fontSize: 12,
                      lineHeight: 1.65,
                    }}
                    lineNumberStyle={{
                      color: 'rgba(219, 234, 254, 0.38)',
                      minWidth: '3em',
                      paddingRight: 12,
                    }}
                    codeTagStyle={{
                      fontFamily: 'var(--font-code, "JetBrains Mono", monospace)',
                    }}
                    wrapLongLines={false}
                  />
                )}
              </div>
            </>
          ) : (
            <div style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('projects.selectFileToPreview')} />
            </div>
          )}
        </div>
      </div>
    </Drawer>
  );
}

export default function Projects() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [addDrawerOpen, setAddDrawerOpen] = useState(false);
  const [editDrawerOpen, setEditDrawerOpen] = useState(false);
  const [editingRepository, setEditingRepository] = useState<RdRepository | null>(null);
  const [selectedRepository, setSelectedRepository] = useState<RdRepository | null>(null);
  const [syncingId, setSyncingId] = useState<string | null>(null);
  const [form] = Form.useForm();
  const [editForm] = Form.useForm();
  const watchedRepositoryUrl = Form.useWatch('url', form);
  const lastAutoRepositoryName = useRef<string | undefined>(undefined);

  useEffect(() => {
    const candidate = repositoryNameFromUrl(watchedRepositoryUrl);
    if (!candidate) return;
    const currentName = form.getFieldValue('name') as string | undefined;
    const previousAutoName = lastAutoRepositoryName.current;
    if (!form.isFieldTouched('name') || !currentName || currentName === previousAutoName) {
      form.setFieldValue('name', candidate);
      lastAutoRepositoryName.current = candidate;
    }
  }, [form, watchedRepositoryUrl]);

  const repositoriesQuery = useQuery({
    queryKey: queryKeys.rd.repositories(),
    queryFn: rdApi.listRepositories,
    refetchInterval: (query) => (query.state.data?.repositories ?? []).some((repository) => repository.indexStatus === 'syncing') ? 2500 : false,
  });
  const repositories = repositoriesQuery.data?.repositories ?? [];

  const branchesQuery = useQuery({
    queryKey: editingRepository ? queryKeys.rd.branches(editingRepository.id) : ['rd', 'repositories', 'none', 'branches'],
    queryFn: () => rdApi.repositoryBranches(editingRepository!.id),
    enabled: editDrawerOpen && !!editingRepository?.id && editingRepository.isCloned,
  });
  const branchOptions = useMemo(() => {
    const values = new Set<string>();
    if (editingRepository?.branch) values.add(editingRepository.branch);
    (branchesQuery.data?.branches ?? []).forEach((branch) => values.add(branch));
    return Array.from(values).map((branch) => ({ value: branch }));
  }, [branchesQuery.data?.branches, editingRepository?.branch]);

  const openEditDrawer = (repository: RdRepository) => {
    setEditingRepository(repository);
    editForm.setFieldsValue({
      url: repository.url,
      name: repository.name,
      branch: repository.branch,
      description: repository.description ?? '',
      default_test_command: repository.defaultTestCommand ?? '',
      default_build_command: repository.defaultBuildCommand ?? '',
      auto_sync_enabled: repository.autoSyncEnabled,
      auto_sync_interval_minutes: repository.autoSyncIntervalMinutes || 60,
      gitlab_token: '',
    });
    setEditDrawerOpen(true);
  };

  const closeEditDrawer = () => {
    setEditDrawerOpen(false);
    setEditingRepository(null);
    editForm.resetFields();
  };

  const createMutation = useMutation({
    mutationFn: rdApi.createRepository,
    onSuccess: (repository) => {
      message.success(t('projects.addSuccess'));
      setAddDrawerOpen(false);
      form.resetFields();
      queryClient.setQueryData<RdRepositoryListResponse>(queryKeys.rd.repositories(), (current) => {
        if (!current) return { repositories: [repository], total: 1 };
        const repositories = [repository, ...current.repositories.filter((item) => item.id !== repository.id)];
        return { ...current, repositories, total: Math.max(current.total, repositories.length) };
      });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
    },
    onError: (error: Error) => message.error(error.message || t('projects.syncFailed')),
  });

  const updateMutation = useMutation({
    mutationFn: ({ id, data }: {
      id: string;
      data: {
        name?: string;
        url?: string;
        branch?: string;
        gitlab_token?: string;
        description?: string;
        default_test_command?: string;
        default_build_command?: string;
        auto_sync_enabled?: boolean;
        auto_sync_interval_minutes?: number;
      };
    }) => rdApi.updateRepository(id, data),
    onSuccess: (repository) => {
      message.success(t('projects.updateSuccess'));
      closeEditDrawer();
      if (selectedRepository?.id === repository.id) {
        setSelectedRepository(repository);
      }
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositoryTree(repository.id) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.branches(repository.id) });
    },
    onError: (error: Error) => message.error(error.message || t('projects.updateFailed')),
  });

  const syncMutation = useMutation({
    mutationFn: rdApi.syncRepository,
    onMutate: async (id) => {
      setSyncingId(id);
      await queryClient.cancelQueries({ queryKey: queryKeys.rd.repositories() });
      const previous = queryClient.getQueryData<RdRepositoryListResponse>(queryKeys.rd.repositories());
      queryClient.setQueryData<RdRepositoryListResponse>(queryKeys.rd.repositories(), (current) => current ? {
        ...current,
        repositories: current.repositories.map((repository) => repository.id === id
          ? { ...repository, indexStatus: 'syncing', lastSyncError: null }
          : repository),
      } : current);
      return { previous };
    },
    onSuccess: (_, id) => {
      message.success(t('projects.syncStarted', '同步任务已启动'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositoryTree(id) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.branches(id) });
    },
    onError: (error: Error, _id, context) => {
      if (context?.previous) {
        queryClient.setQueryData(queryKeys.rd.repositories(), context.previous);
      }
      message.error(error.message || t('projects.syncFailed'));
    },
    onSettled: () => {
      setSyncingId(null);
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: rdApi.deleteRepository,
    onSuccess: () => {
      message.success(t('projects.deleteSuccess'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
    },
    onError: (error: Error) => message.error(error.message || t('projects.syncFailed')),
  });

  const columns: ColumnsType<RdRepository> = [
    {
      title: t('projects.columns.cloneStatus'),
      dataIndex: 'isCloned',
      width: 92,
      render: (cloned: boolean) => cloned ? <CheckCircleOutlined style={{ color: 'var(--color-success)' }} /> : <Tag>{t('projects.notCloned')}</Tag>,
    },
    {
      title: t('projects.columns.name'),
      dataIndex: 'name',
      width: 260,
      render: (name: string, record) => (
        <Space direction="vertical" size={0}>
          <Space><FolderOpenOutlined style={{ color: '#d6a62a' }} /><Text strong>{name}</Text></Space>
          {record.description ? <Text type="secondary" style={{ fontSize: 12 }}>{record.description}</Text> : null}
        </Space>
      ),
    },
    {
      title: t('projects.columns.repoUrl'),
      dataIndex: 'url',
      width: 320,
      render: (url: string) => <a href={url} target="_blank" rel="noopener noreferrer"><LinkOutlined /> {url}</a>,
    },
    {
      title: t('projects.columns.branch'),
      dataIndex: 'branch',
      width: 120,
      render: (branch: string) => <Tag color="blue"><BranchesOutlined /> {branch}</Tag>,
    },
    {
      title: t('rd.indexedFiles', '索引文件'),
      dataIndex: 'indexedFileCount',
      width: 120,
      render: (count: number, record) => (
        <Space direction="vertical" size={0}>
          <Text>{count} · {record.indexStatus ?? 'idle'}</Text>
          <Text type="secondary" style={{ fontSize: 12 }}>{t('projects.symbolsCount', { count: record.indexedSymbolCount ?? 0 })}</Text>
          <Text type="secondary" style={{ fontSize: 12 }}>{t('projects.importsCount', { count: record.indexedImportCount ?? 0 })}</Text>
        </Space>
      ),
    },
    {
      title: t('projects.columns.detectedStack'),
      key: 'detectedStack',
      width: 360,
      render: (_, record) => <RepositoryDetectionSummary repository={record} />,
    },
    {
      title: t('projects.columns.lastSync'),
      dataIndex: 'lastSyncAt',
      width: 150,
      render: (dt?: string | null) => dt ? <Text type="secondary">{dayjs(dt).fromNow()}</Text> : <Text type="secondary">{t('projects.neverSynced')}</Text>,
    },
    {
      title: t('projects.columns.autoSync'),
      key: 'autoSync',
      width: 220,
      render: (_, record) => (
        <Space direction="vertical" size={0} style={{ maxWidth: 200 }}>
          <Space size={6}>
            <Tag color={record.autoSyncEnabled ? 'success' : 'default'}>
              {record.autoSyncEnabled ? t('common.enabled') : t('common.disabled')}
            </Tag>
            {record.autoSyncEnabled ? <Text type="secondary">{t('projects.autoSyncEvery', { count: record.autoSyncIntervalMinutes })}</Text> : null}
          </Space>
          {record.lastAutoSyncAt ? (
            <Text type="secondary" style={{ fontSize: 12 }}>{t('projects.autoSyncLastAttempt', { time: dayjs(record.lastAutoSyncAt).fromNow() })}</Text>
          ) : null}
          {record.lastSyncError ? (
            <Tooltip title={record.lastSyncError}>
              <Text type="danger" ellipsis style={{ maxWidth: 200, fontSize: 12 }}>{t('projects.autoSyncFailed')}</Text>
            </Tooltip>
          ) : null}
        </Space>
      ),
    },
    {
      title: t('common.actions'),
      key: 'actions',
      fixed: 'right',
      width: 360,
      render: (_, record) => (
        <Space size={4}>
          <Button
            size="small"
            type="primary"
            icon={<FolderOpenOutlined />}
            disabled={!record.isCloned}
            onClick={() => setSelectedRepository(record)}
          >
            {t('projects.browseFiles')}
          </Button>
          <Button
            size="small"
            icon={<EditOutlined />}
            onClick={() => openEditDrawer(record)}
          >
            {t('common.edit')}
          </Button>
          <Button
            size="small"
            icon={<SyncOutlined spin={syncingId === record.id || record.indexStatus === 'syncing'} />}
            loading={syncingId === record.id || record.indexStatus === 'syncing'}
            disabled={!!syncingId || record.indexStatus === 'syncing'}
            onClick={() => syncMutation.mutate(record.id)}
          >
            {t('projects.sync')}
          </Button>
          <Popconfirm
            title={t('projects.deleteConfirm')}
            description={t('projects.localFilesSafe')}
            onConfirm={() => deleteMutation.mutate(record.id)}
            okText={t('common.delete')}
            cancelText={t('common.cancel')}
            okButtonProps={{ danger: true }}
          >
            <Tooltip title={t('projects.delete')}>
              <Button size="small" danger icon={<DeleteOutlined />} />
            </Tooltip>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const gitUrlRules = [
    { required: true, message: t('common.required') },
    {
      validator: (_: unknown, value?: string) =>
        isGitRepositoryUrl(value) ? Promise.resolve() : Promise.reject(new Error(t('projects.form.urlInvalid'))),
    },
  ];

  return (
    <div style={{ padding: 24 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: 16, marginBottom: 16 }}>
        <div>
          <Title level={4} style={{ margin: 0 }}>{t('projects.title')}</Title>
          <Text type="secondary">{t('projects.subtitle')}</Text>
        </div>
        <Space>
          <Button icon={<ReloadOutlined />} loading={repositoriesQuery.isFetching} onClick={() => repositoriesQuery.refetch()}>{t('projects.refresh')}</Button>
          <Button type="primary" icon={<PlusOutlined />} onClick={() => setAddDrawerOpen(true)}>{t('projects.add')}</Button>
        </Space>
      </div>

      <Card styles={{ body: { padding: 0 } }}>
        <Table
          rowKey="id"
          columns={columns}
          dataSource={repositories}
          loading={repositoriesQuery.isLoading}
          pagination={{ pageSize: 20, size: 'small' }}
          locale={{ emptyText: t('projects.empty.title') }}
          scroll={{ x: 'max-content' }}
        />
      </Card>

      <Drawer
        title={t('projects.add')}
        open={addDrawerOpen}
        onClose={() => setAddDrawerOpen(false)}
        width={560}
        footer={
          <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
            <Button onClick={() => setAddDrawerOpen(false)}>{t('common.cancel')}</Button>
            <Button
              type="primary"
              loading={createMutation.isPending}
              onClick={async () => {
                const values = await form.validateFields();
                createMutation.mutate(values);
              }}
            >
              {t('common.create')}
            </Button>
          </Space>
        }
      >
        <Alert type="info" showIcon message={t('projects.addHint')} style={{ marginBottom: 16 }} />
        <Form form={form} layout="vertical" requiredMark="optional" initialValues={{ branch: 'main', auto_sync_enabled: true, auto_sync_interval_minutes: 60 }}>
          <Form.Item name="url" label={t('projects.form.repoUrl')} rules={gitUrlRules}>
            <Input prefix={<LinkOutlined />} placeholder={t('projects.form.urlPlaceholder')} />
          </Form.Item>
          <Form.Item name="name" label={t('projects.form.projectName')} rules={[{ required: true, message: t('common.required') }]}>
            <Input prefix={<FolderOpenOutlined />} placeholder={t('projects.form.projectNamePlaceholder')} />
          </Form.Item>
          <Form.Item name="branch" label={t('projects.form.branch')} extra={t('projects.form.branchExtra')}>
            <Input prefix={<BranchesOutlined />} placeholder={t('projects.form.branchPlaceholder')} />
          </Form.Item>
          <Form.Item name="description" label={t('projects.columns.desc')}>
            <Input.TextArea rows={2} placeholder={t('projects.form.descPlaceholder')} />
          </Form.Item>
          <Form.Item name="default_test_command" label={t('rd.defaultTestCommand', '默认测试命令')} extra={t('projects.form.commandExtra')}>
            <Input placeholder="npm test / cargo test --workspace" />
          </Form.Item>
          <Form.Item name="default_build_command" label={t('rd.defaultBuildCommand', '默认构建命令')} extra={t('projects.form.commandExtra')}>
            <Input placeholder="npm run build / cargo build" />
          </Form.Item>
          <Form.Item name="auto_sync_enabled" label={t('projects.form.autoSync')} valuePropName="checked" extra={t('projects.form.autoSyncExtra')}>
            <Switch />
          </Form.Item>
          <Form.Item
            name="auto_sync_interval_minutes"
            label={t('projects.form.autoSyncInterval')}
            rules={[{ required: true, message: t('common.required') }]}
          >
            <InputNumber min={5} max={10080} precision={0} addonAfter={t('projects.minutes')} style={{ width: '100%' }} />
          </Form.Item>
          <Form.Item name="gitlab_token" label={t('projects.form.token')} extra={t('projects.form.tokenExtra')}>
            <Input.Password prefix={<LinkOutlined />} placeholder={t('projects.form.tokenPlaceholder')} />
          </Form.Item>
        </Form>
      </Drawer>

      <Drawer
        title={t('projects.editTitle')}
        open={editDrawerOpen}
        onClose={closeEditDrawer}
        width={560}
        footer={
          <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
            <Button onClick={closeEditDrawer}>{t('common.cancel')}</Button>
            <Button
              type="primary"
              loading={updateMutation.isPending}
              onClick={async () => {
                if (!editingRepository) return;
                const values = await editForm.validateFields();
                updateMutation.mutate({ id: editingRepository.id, data: values });
              }}
            >
              {t('common.save')}
            </Button>
          </Space>
        }
      >
        <Alert
          type="info"
          showIcon
          message={t('projects.editHint')}
          style={{ marginBottom: 16 }}
        />
        {branchesQuery.isError ? (
          <Alert
            type="warning"
            showIcon
            message={t('projects.branchLoadFailed')}
            style={{ marginBottom: 16 }}
          />
        ) : null}
        <Form form={editForm} layout="vertical" requiredMark="optional">
          <Form.Item name="url" label={t('projects.form.repoUrl')} rules={gitUrlRules}>
            <Input prefix={<LinkOutlined />} placeholder={t('projects.form.urlPlaceholder')} />
          </Form.Item>
          <Form.Item name="name" label={t('projects.form.projectName')} rules={[{ required: true, message: t('common.required') }]}>
            <Input prefix={<FolderOpenOutlined />} placeholder={t('projects.form.projectNamePlaceholder')} />
          </Form.Item>
          <Form.Item name="branch" label={t('projects.form.branch')} extra={t('projects.form.branchSwitchExtra')}>
            <AutoComplete
              options={branchOptions}
              filterOption={(inputValue, option) =>
                String(option?.value ?? '').toLowerCase().includes(inputValue.toLowerCase())
              }
              placeholder={t('projects.form.branchPlaceholder')}
              disabled={!editingRepository}
            >
              <Input prefix={<BranchesOutlined />} />
            </AutoComplete>
          </Form.Item>
          <Form.Item name="description" label={t('projects.columns.desc')}>
            <Input.TextArea rows={2} placeholder={t('projects.form.descPlaceholder')} />
          </Form.Item>
          <Form.Item name="default_test_command" label={t('rd.defaultTestCommand', '默认测试命令')} extra={t('projects.form.commandExtra')}>
            <Input placeholder="npm test / cargo test --workspace" />
          </Form.Item>
          <Form.Item name="default_build_command" label={t('rd.defaultBuildCommand', '默认构建命令')} extra={t('projects.form.commandExtra')}>
            <Input placeholder="npm run build / cargo build" />
          </Form.Item>
          <Form.Item name="auto_sync_enabled" label={t('projects.form.autoSync')} valuePropName="checked" extra={t('projects.form.autoSyncExtra')}>
            <Switch />
          </Form.Item>
          <Form.Item
            name="auto_sync_interval_minutes"
            label={t('projects.form.autoSyncInterval')}
            rules={[{ required: true, message: t('common.required') }]}
          >
            <InputNumber min={5} max={10080} precision={0} addonAfter={t('projects.minutes')} style={{ width: '100%' }} />
          </Form.Item>
          <Form.Item name="gitlab_token" label={t('projects.form.token')} extra={t('projects.form.tokenKeepExtra')}>
            <Input.Password prefix={<LinkOutlined />} placeholder={t('projects.form.tokenKeepPlaceholder')} />
          </Form.Item>
        </Form>
      </Drawer>

      <RepositoryBrowser repository={selectedRepository} open={!!selectedRepository} onClose={() => setSelectedRepository(null)} />
    </div>
  );
}
