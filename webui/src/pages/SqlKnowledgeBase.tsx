import { useEffect, useMemo, useRef, useState } from 'react';
import {
  Alert,
  Badge,
  Button,
  Card,
  Col,
  Drawer,
  Empty,
  Form,
  Input,
  Layout,
  List,
  Modal,
  Popconfirm,
  Row,
  Select,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
  Upload,
  message,
  type UploadProps,
} from 'antd';
import {
  CheckCircleOutlined,
  DeleteOutlined,
  EditOutlined,
  EyeOutlined,
  FileSearchOutlined,
  FileTextOutlined,
  PlusOutlined,
  ReloadOutlined,
  SearchOutlined,
  UploadOutlined,
} from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { dataSourcesApi, nl2sqlApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  DataSourceInfo,
  Nl2sqlReferenceFile,
  Nl2sqlReferencePack,
  Nl2sqlReferenceUsage,
} from '@/types';
import { ErrorBoundary } from '@/components/ErrorBoundary';
import { useDismissibleNotice } from '@/hooks/useDismissibleNotice';
import { useAuthStore } from '@/store/auth';

const { Dragger } = Upload;
const { Text, Title, Paragraph } = Typography;
const { TextArea } = Input;

const GLOBAL_DATASOURCE_ID = 'global';

function datasourceName(
  datasources: DataSourceInfo[],
  id: string,
  deletedLabel: string,
  globalLabel: string,
) {
  if (id === GLOBAL_DATASOURCE_ID) return globalLabel;
  return datasources.find((ds) => ds.id === id)?.name ?? `${id} · ${deletedLabel}`;
}

function spaceBindings(space: Nl2sqlReferencePack) {
  return (space.datasourceBindings && space.datasourceBindings.length > 0)
    ? space.datasourceBindings
    : [space.datasourceId].filter(Boolean);
}

async function listAllDataSourcesForSqlKnowledge() {
  const perPage = 100;
  let page = 1;
  let total = Number.POSITIVE_INFINITY;
  const dataSources: DataSourceInfo[] = [];
  while (dataSources.length < total) {
    const response = await dataSourcesApi.list({ page, per_page: perPage });
    dataSources.push(...(response.data_sources ?? []));
    total = response.total ?? dataSources.length;
    if ((response.data_sources ?? []).length === 0) break;
    page += 1;
  }
  return { data_sources: dataSources, total: Number.isFinite(total) ? total : dataSources.length };
}

function formatScore(score: number) {
  return Number.isFinite(score) ? score.toFixed(2) : '0.00';
}

function metadataList(file: Nl2sqlReferenceFile, key: 'tables' | 'metrics') {
  const value = file.metadata?.[key];
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
    : [];
}

function KnowledgeTags({
  item,
  verifiedLabel,
}: {
  item: Pick<Nl2sqlReferenceUsage | Nl2sqlReferencePack, 'verified'>;
  verifiedLabel: string;
}) {
  return (
    <Space size={4} wrap>
      {item.verified && <Tag color="green">{verifiedLabel}</Tag>}
    </Space>
  );
}

export default function SqlKnowledgeBase() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const currentUser = useAuthStore((state) => state.user);
  const isAdmin = currentUser?.role === 'admin' || currentUser?.role === 'superadmin';
  const localEmbeddingNotice = useDismissibleNotice('aos.embedding-local-notice.v1');
  const [createOpen, setCreateOpen] = useState(false);
  const [selectedSpaceId, setSelectedSpaceId] = useState<string | null>(null);
  const [searchQuestion, setSearchQuestion] = useState('');
  const [searchDatasourceId, setSearchDatasourceId] = useState<string | undefined>();
  const [readTarget, setReadTarget] = useState<Nl2sqlReferenceUsage | null>(null);
  const [previewFile, setPreviewFile] = useState<Nl2sqlReferenceFile | null>(null);
  const [editFile, setEditFile] = useState<Nl2sqlReferenceFile | null>(null);
  const [editContent, setEditContent] = useState('');
  const [createForm] = Form.useForm();
  const uploadBatchKeyRef = useRef<string | null>(null);

  const spacesQuery = useQuery({
    queryKey: queryKeys.nl2sql.sqlKnowledge.spaces({ includeGlobal: true }),
    queryFn: () => nl2sqlApi.listSqlKnowledgeSpaces({ includeGlobal: true }),
  });

  const datasourcesQuery = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: listAllDataSourcesForSqlKnowledge,
  });

  const embeddingQuery = useQuery({
    queryKey: queryKeys.nl2sql.embeddingConfig(),
    queryFn: () => nl2sqlApi.getEmbeddingConfig(),
  });

  const datasources = datasourcesQuery.data?.data_sources ?? [];
  const spaces = spacesQuery.data ?? [];
  const selectedSpace = spaces.find((space) => space.id === selectedSpaceId) ?? spaces[0] ?? null;
  const embeddingAvailable = embeddingQuery.data?.available === true;
  const datasourceIds = useMemo(() => new Set(datasources.map((ds) => ds.id)), [datasources]);
  const datasourceListReady = !datasourcesQuery.isLoading && !datasourcesQuery.isError;
  const isDeletedDatasourceId = (id: string) =>
    datasourceListReady && id !== 'global' && !datasourceIds.has(id);
  const selectedSpacePrimaryDeleted = Boolean(
    selectedSpace && isDeletedDatasourceId(selectedSpace.datasourceId),
  );

  const datasourceOptions = useMemo(
    () => datasources.map((ds) => ({
      label: `${ds.name} · ${ds.db_type} · ${t(`sqlKnowledge.visibility.${ds.visibility}`)}`,
      value: ds.id,
    })),
    [datasources, t],
  );
  const bindingDatasourceOptions = useMemo(
    () => [
      ...(isAdmin ? [{
        label: t('sqlKnowledge.allTenantDatasources'),
        value: GLOBAL_DATASOURCE_ID,
      }] : []),
      ...datasourceOptions,
    ],
    [datasourceOptions, isAdmin, t],
  );

  const invalidateReferencePackCaches = (affectedDatasourceIds?: Iterable<string | null | undefined>) => {
    const ids = new Set<string>();
    for (const id of affectedDatasourceIds ?? []) {
      if (id && id !== 'global') ids.add(id);
    }

    if (ids.size > 0) {
      ids.forEach((id) => {
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.referencePacks(id) });
      });
      return;
    }

    qc.invalidateQueries({
      predicate: (query) =>
        query.queryKey[0] === 'nl2sql' &&
        query.queryKey[1] === 'referencePacks',
    });
  };

  const refresh = (affectedDatasourceIds?: Iterable<string | null | undefined>) => {
    qc.invalidateQueries({ queryKey: queryKeys.nl2sql.sqlKnowledge.all() });
    invalidateReferencePackCaches(affectedDatasourceIds);
  };

  const createMutation = useMutation({
    mutationFn: (values: {
      name: string;
      description?: string;
      datasourceIds?: string[];
      verified?: boolean;
      tagsText?: string;
    }) => {
      const selectedIds = values.datasourceIds ?? [];
      const global = selectedIds.includes(GLOBAL_DATASOURCE_ID);
      return nl2sqlApi.createSqlKnowledgeSpace({
        name: values.name,
        description: values.description,
        datasourceIds: global ? [] : selectedIds,
        global,
        verified: values.verified,
        tags: values.tagsText
          ?.split(',')
          .map((v) => v.trim())
          .filter(Boolean),
      });
    },
    onSuccess: (space) => {
      message.success(t('sqlKnowledge.messages.spaceCreated'));
      setCreateOpen(false);
      createForm.resetFields();
      setSelectedSpaceId(space.id);
      refresh(spaceBindings(space));
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const updateMutation = useMutation({
    mutationFn: ({ space, patch }: { space: Nl2sqlReferencePack; patch: Parameters<typeof nl2sqlApi.updateSqlKnowledgeSpace>[1] }) =>
      nl2sqlApi.updateSqlKnowledgeSpace(space.id, patch),
    onSuccess: (space) => {
      message.success(t('sqlKnowledge.messages.spaceUpdated'));
      refresh(spaceBindings(space));
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const deleteSpaceMutation = useMutation({
    mutationFn: (spaceId: string) => nl2sqlApi.deleteSqlKnowledgeSpace(spaceId),
    onSuccess: () => {
      message.success(t('sqlKnowledge.messages.spaceDeleted'));
      setSelectedSpaceId(null);
      refresh(selectedSpace ? spaceBindings(selectedSpace) : undefined);
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const uploadMutation = useMutation({
    mutationFn: ({ spaceId, files }: { spaceId: string; files: File[] }) =>
      nl2sqlApi.uploadSqlKnowledgeFiles(spaceId, files),
    onSuccess: (res) => {
      message.success(t('sqlKnowledge.messages.filesUploaded', { count: res.files.length }));
      refresh(selectedSpace ? spaceBindings(selectedSpace) : undefined);
    },
    onError: (error: Error) => message.error(error.message || t('sqlKnowledge.messages.uploadFailed')),
  });

  const deleteFileMutation = useMutation({
    mutationFn: (fileId: string) => nl2sqlApi.deleteSqlKnowledgeFile(fileId),
    onSuccess: () => {
      message.success(t('sqlKnowledge.messages.fileDeleted'));
      refresh(selectedSpace ? spaceBindings(selectedSpace) : undefined);
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const updateFileMutation = useMutation({
    mutationFn: ({ fileId, content }: { fileId: string; content: string }) =>
      nl2sqlApi.updateSqlKnowledgeFile(fileId, { content }),
    onSuccess: (file) => {
      message.success(t('sqlKnowledge.messages.fileUpdated'));
      setEditFile(null);
      setEditContent('');
      setPreviewFile(file);
      refresh(selectedSpace ? spaceBindings(selectedSpace) : undefined);
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.sqlKnowledge.file(file.id, {}) });
    },
    onError: (error: Error) => message.error(error.message || t('sqlKnowledge.messages.fileUpdateFailed')),
  });

  const searchMutation = useMutation({
    mutationFn: () =>
      nl2sqlApi.searchSqlKnowledge({
        question: searchQuestion.trim(),
        datasourceId: searchDatasourceId,
        limit: 10,
      }),
    onError: (error: Error) => message.error(error.message || t('sqlKnowledge.messages.searchFailed')),
  });

  const readQuery = useQuery({
    queryKey: readTarget
      ? queryKeys.nl2sql.sqlKnowledge.file(readTarget.fileId, {
          startLine: Math.max(1, readTarget.startLine - 20),
          endLine: readTarget.endLine + 80,
        })
      : ['nl2sql', 'sqlKnowledge', 'file', 'none'],
    queryFn: () =>
      nl2sqlApi.readSqlKnowledgeFile(readTarget!.fileId, {
        startLine: Math.max(1, readTarget!.startLine - 20),
        endLine: readTarget!.endLine + 80,
      }),
    enabled: Boolean(readTarget),
  });

  const previewQuery = useQuery({
    queryKey: previewFile
      ? queryKeys.nl2sql.sqlKnowledge.file(previewFile.id, {})
      : ['nl2sql', 'sqlKnowledge', 'filePreview', 'none'],
    queryFn: () => nl2sqlApi.readSqlKnowledgeFile(previewFile!.id),
    enabled: Boolean(previewFile),
  });

  const editQuery = useQuery({
    queryKey: editFile
      ? queryKeys.nl2sql.sqlKnowledge.file(editFile.id, {})
      : ['nl2sql', 'sqlKnowledge', 'fileEdit', 'none'],
    queryFn: () => nl2sqlApi.readSqlKnowledgeFile(editFile!.id),
    enabled: Boolean(editFile),
  });

  useEffect(() => {
    if (editQuery.data && editFile?.id === editQuery.data.fileId) {
      setEditContent(editQuery.data.content);
    }
  }, [editFile?.id, editQuery.data]);

  const uploadProps: UploadProps = {
    multiple: true,
    showUploadList: false,
    accept: '.sql,.md,.txt,.csv,.json,.yaml,.yml,.zip',
    beforeUpload: (file, fileList) => {
      if (!selectedSpace) {
        message.warning(t('sqlKnowledge.selectSpaceFirst'));
        return Upload.LIST_IGNORE;
      }
      if (selectedSpacePrimaryDeleted) {
        message.warning(t('sqlKnowledge.deletedDatasourceWarning'));
        return Upload.LIST_IGNORE;
      }
      if (!selectedSpace.writable) {
        message.warning(t('sqlKnowledge.readOnlySpace'));
        return Upload.LIST_IGNORE;
      }
      const batchFiles = fileList.length > 0 ? fileList : [file];
      const batchKey = batchFiles.map((item) => item.uid).join('|');
      if (uploadBatchKeyRef.current === batchKey) {
        return Upload.LIST_IGNORE;
      }
      uploadBatchKeyRef.current = batchKey;
      window.setTimeout(() => {
        if (uploadBatchKeyRef.current === batchKey) {
          uploadBatchKeyRef.current = null;
        }
      }, 0);
      uploadMutation.mutate({
        spaceId: selectedSpace.id,
        files: batchFiles.map((item) => item as File),
      });
      return Upload.LIST_IGNORE;
    },
    disabled: !selectedSpace || !selectedSpace.writable || selectedSpacePrimaryDeleted || !embeddingAvailable || uploadMutation.isPending,
  };
  const folderUploadProps: UploadProps = {
    ...uploadProps,
    directory: true,
  };

  const fileColumns = [
    {
      title: t('sqlKnowledge.file'),
      dataIndex: 'filename',
      key: 'filename',
      render: (_: unknown, file: Nl2sqlReferenceFile) => (
        <Space direction="vertical" size={2} style={{ maxWidth: 420 }}>
          <Tooltip title={file.filename}>
            <span
              style={{
                display: 'inline-block',
                maxWidth: 420,
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
                fontWeight: 600,
              }}
            >
              {file.filename}
            </span>
          </Tooltip>
          <Text type="secondary" style={{ fontSize: 12 }}>
            {file.language || 'text'} · v{file.versionNo ?? 1} · {file.chunkCount} chunks
          </Text>
          <Space size={4} wrap>
            {metadataList(file, 'tables').slice(0, 3).map((table) => (
              <Tag key={`table-${table}`} color="blue">{table}</Tag>
            ))}
            {metadataList(file, 'metrics').slice(0, 3).map((metric) => (
              <Tag key={`metric-${metric}`} color="purple">{metric}</Tag>
            ))}
          </Space>
        </Space>
      ),
    },
    {
      title: t('sqlKnowledge.status'),
      dataIndex: 'status',
      key: 'status',
      width: 140,
      render: (status: string, file: Nl2sqlReferenceFile) => (
        <Tooltip title={file.error || undefined}>
          <span style={{ display: 'inline-flex' }}>
            <Badge
              status={status === 'indexed' ? 'success' : status === 'failed' ? 'error' : 'processing'}
              text={status}
            />
          </span>
        </Tooltip>
      ),
    },
    {
      title: t('sqlKnowledge.updatedAt'),
      dataIndex: 'updatedAt',
      key: 'updatedAt',
      width: 180,
    },
    {
      title: t('common.actions'),
      key: 'actions',
      width: 140,
      render: (_: unknown, file: Nl2sqlReferenceFile) => (
        <Space size={4}>
          <Tooltip title={t('sqlKnowledge.previewFile')}>
            <Button
              size="small"
              type="text"
              icon={<EyeOutlined />}
              onClick={() => setPreviewFile(file)}
            />
          </Tooltip>
          <Tooltip title={t('sqlKnowledge.editFile')}>
            <Button
              size="small"
              type="text"
              icon={<EditOutlined />}
              disabled={!embeddingAvailable || selectedSpacePrimaryDeleted || !selectedSpace?.writable}
              onClick={() => setEditFile(file)}
            />
          </Tooltip>
          <Popconfirm
            title={t('sqlKnowledge.deleteFileConfirm')}
            onConfirm={() => deleteFileMutation.mutate(file.id)}
          >
            <Button size="small" danger type="text" icon={<DeleteOutlined />} disabled={!selectedSpace?.writable} />
          </Popconfirm>
        </Space>
      ),
    },
  ];

  return (
    <ErrorBoundary>
      <Layout style={{ minHeight: '100vh', background: 'var(--bg-void)' }}>
        <Layout.Content style={{ padding: 24, maxWidth: 1320, margin: '0 auto', width: '100%' }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', gap: 16, marginBottom: 20 }}>
            <div>
              <Title level={4} style={{ margin: 0 }}>
                <FileTextOutlined style={{ marginRight: 8 }} />
                {t('sqlKnowledge.title')}
              </Title>
              <Text type="secondary">{t('sqlKnowledge.subtitle')}</Text>
            </div>
            <Space>
              <Button icon={<ReloadOutlined />} onClick={() => refresh()}>
                {t('common.refresh')}
              </Button>
              <Button
                type="primary"
                icon={<PlusOutlined />}
                onClick={() => setCreateOpen(true)}
                disabled={!embeddingAvailable}
              >
                {t('sqlKnowledge.createSpace')}
              </Button>
            </Space>
          </div>

          {embeddingQuery.data?.configured_via === 'local' && localEmbeddingNotice.visible && (
            <Alert
              type="info"
              showIcon
              closable
              onClose={localEmbeddingNotice.dismiss}
              style={{ marginBottom: 16 }}
              message={t('sqlKnowledge.embeddingRequiredTitle')}
              description={t('sqlKnowledge.embeddingRequiredDesc')}
            />
          )}

          <Row gutter={[16, 16]}>
            <Col xs={24} lg={8}>
              <Card
                title={t('sqlKnowledge.spaces')}
                loading={spacesQuery.isLoading}
                bodyStyle={{ padding: 0 }}
              >
                {spaces.length === 0 ? (
                  <Empty
                    image={Empty.PRESENTED_IMAGE_SIMPLE}
                    description={t('sqlKnowledge.emptySpaces')}
                    style={{ padding: 24 }}
                  />
                ) : (
                  <List
                    dataSource={spaces}
                    renderItem={(space) => {
                      const active = selectedSpace?.id === space.id;
                      const bindings = spaceBindings(space);
                      const primaryDeleted = isDeletedDatasourceId(space.datasourceId);
                      return (
                        <List.Item
                          onClick={() => setSelectedSpaceId(space.id)}
                          style={{
                            cursor: 'pointer',
                            padding: '14px 16px',
                            background: active ? 'var(--bg-elevated)' : undefined,
                            borderInlineStart: active ? '3px solid var(--ant-color-primary)' : '3px solid transparent',
                          }}
                          actions={[
                            <Tooltip key="enabled" title={t('sqlKnowledge.enabledHelp')}>
                              <Switch
                                size="small"
                                checkedChildren={t('sqlKnowledge.enabledShort')}
                                unCheckedChildren={t('sqlKnowledge.disabledShort')}
                                checked={space.enabled}
                                disabled={primaryDeleted || !space.writable}
                                aria-label={t('sqlKnowledge.enabledHelp')}
                                onClick={(checked, event) => {
                                  event.stopPropagation();
                                  updateMutation.mutate({ space, patch: { enabled: checked } });
                                }}
                              />
                            </Tooltip>,
                            <Popconfirm
                              key="delete"
                              title={t('sqlKnowledge.deleteSpaceConfirm')}
                              onConfirm={(event) => {
                                event?.stopPropagation();
                                deleteSpaceMutation.mutate(space.id);
                              }}
                            >
                              <Button
                                size="small"
                                danger
                                type="text"
                                icon={<DeleteOutlined />}
                                disabled={!space.writable}
                                onClick={(event) => event.stopPropagation()}
                              />
                            </Popconfirm>,
                          ]}
                        >
                          <List.Item.Meta
                            title={
                              <Space wrap size={6}>
                                <Text strong>{space.name}</Text>
                                {space.scope === 'tenant' ? (
                                  <Tag color="blue">{t('sqlKnowledge.global')}</Tag>
                                ) : bindings.length > 0 && bindings.every((id) =>
                                  datasources.find((ds) => ds.id === id)?.visibility === 'tenant'
                                ) ? (
                                  <Tag color="green">{t('sqlKnowledge.visibility.tenant')}</Tag>
                                ) : (
                                  <Tag>{t('sqlKnowledge.visibility.private')}</Tag>
                                )}
                                <KnowledgeTags
                                  item={space}
                                  verifiedLabel={t('sqlKnowledge.verified')}
                                />
                              </Space>
                            }
                            description={
                              <Space direction="vertical" size={4}>
                                <Text type="secondary" ellipsis={{ tooltip: space.description }}>
                                  {space.description || t('sqlKnowledge.noDescription')}
                                </Text>
                                <Text type="secondary" style={{ fontSize: 12 }}>
                                  {space.fileCount} files · {space.chunkCount} chunks
                                </Text>
                                <Space size={4} wrap>
                                  {bindings.slice(0, 3).map((id) => (
                                    <Tag key={id} color={isDeletedDatasourceId(id) ? 'red' : undefined}>
                                      {datasourceName(
                                        datasources,
                                        id,
                                        t('sqlKnowledge.datasourceDeleted'),
                                        t('sqlKnowledge.allTenantDatasources'),
                                      )}
                                    </Tag>
                                  ))}
                                  {bindings.length > 3 && <Tag>+{bindings.length - 3}</Tag>}
                                  {primaryDeleted && <Tag color="red">{t('sqlKnowledge.datasourceDeleted')}</Tag>}
                                </Space>
                              </Space>
                            }
                          />
                        </List.Item>
                      );
                    }}
                  />
                )}
              </Card>
            </Col>

            <Col xs={24} lg={16}>
              <Space direction="vertical" size={16} style={{ width: '100%' }}>
                <Card
                  title={selectedSpace ? selectedSpace.name : t('sqlKnowledge.selectSpace')}
                  extra={
                    selectedSpace ? (
                      <Space>
                        <Tooltip title={t('sqlKnowledge.verifiedHelp')}>
                          <Switch
                            checkedChildren={t('sqlKnowledge.verified')}
                            unCheckedChildren={t('sqlKnowledge.unverified')}
                            checked={Boolean(selectedSpace.verified)}
                            disabled={selectedSpacePrimaryDeleted || !selectedSpace.writable}
                            aria-label={t('sqlKnowledge.verifiedHelp')}
                            onChange={(checked) =>
                              updateMutation.mutate({ space: selectedSpace, patch: { verified: checked } })
                            }
                          />
                        </Tooltip>
                      </Space>
                    ) : null
                  }
                >
                  {selectedSpacePrimaryDeleted && (
                    <Alert
                      type="warning"
                      showIcon
                      style={{ marginBottom: 16 }}
                      message={t('sqlKnowledge.deletedDatasourceTitle')}
                      description={t('sqlKnowledge.deletedDatasourceDesc')}
                    />
                  )}
                  <Dragger {...uploadProps} style={{ marginBottom: 16 }}>
                    <p className="ant-upload-drag-icon"><UploadOutlined /></p>
                    <p className="ant-upload-text">{t('sqlKnowledge.uploadTitle')}</p>
                    <p className="ant-upload-hint">{t('sqlKnowledge.uploadHint')}</p>
                  </Dragger>
                  <Upload {...folderUploadProps}>
                    <Button
                      icon={<UploadOutlined />}
                      loading={uploadMutation.isPending}
                      disabled={!selectedSpace || !selectedSpace.writable || selectedSpacePrimaryDeleted || !embeddingAvailable}
                      style={{ marginBottom: 16 }}
                    >
                      {t('sqlKnowledge.importFolder')}
                    </Button>
                  </Upload>
                  <Table
                    size="small"
                    rowKey="id"
                    dataSource={selectedSpace?.files ?? []}
                    columns={fileColumns}
                    pagination={{ pageSize: 8 }}
                    locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('sqlKnowledge.emptyFiles')} /> }}
                  />
                </Card>

                <Card title={<Space><SearchOutlined />{t('sqlKnowledge.searchTest')}</Space>}>
                  <Space direction="vertical" style={{ width: '100%' }} size={12}>
                    <Row gutter={[12, 12]}>
                      <Col xs={24} md={8}>
                        <Select
                          allowClear
                          showSearch
                          style={{ width: '100%' }}
                          placeholder={t('sqlKnowledge.datasourceOptional')}
                          options={datasourceOptions}
                          value={searchDatasourceId}
                          onChange={setSearchDatasourceId}
                          optionFilterProp="label"
                        />
                      </Col>
                      <Col xs={24} md={16}>
                        <Input.Search
                          placeholder={t('sqlKnowledge.searchPlaceholder')}
                          value={searchQuestion}
                          onChange={(event) => setSearchQuestion(event.target.value)}
                          onSearch={() => {
                            if (!searchQuestion.trim()) {
                              message.warning(t('sqlKnowledge.enterQuestion'));
                              return;
                            }
                            searchMutation.mutate();
                          }}
                          enterButton={
                            <Button
                              type="primary"
                              icon={<FileSearchOutlined />}
                              loading={searchMutation.isPending}
                            >
                              {t('sqlKnowledge.runSearch')}
                            </Button>
                          }
                          disabled={!embeddingAvailable}
                        />
                      </Col>
                    </Row>

                    {(searchMutation.data?.references ?? []).length === 0 ? (
                      <Empty
                        image={Empty.PRESENTED_IMAGE_SIMPLE}
                        description={searchMutation.data ? t('sqlKnowledge.noSearchHits') : t('sqlKnowledge.searchHint')}
                      />
                    ) : (
                      <List
                        dataSource={searchMutation.data?.references ?? []}
                        renderItem={(ref) => (
                          <List.Item
                            actions={[
                              <Button key="read" size="small" type="link" onClick={() => setReadTarget(ref)}>
                                {t('sqlKnowledge.readContext')}
                              </Button>,
                            ]}
                          >
                            <List.Item.Meta
                              title={
                                <Space size={6} wrap>
                                  <Tag color="blue">{ref.packName}</Tag>
                                  <Text strong>{ref.filename}</Text>
                                  <Text type="secondary">L{ref.startLine}-{ref.endLine}</Text>
                                  {ref.chunkType && <Tag>{ref.chunkType}</Tag>}
                                  {ref.language && <Tag>{ref.language}</Tag>}
                                  {ref.verified && <Tag color="green"><CheckCircleOutlined /> {t('sqlKnowledge.verified')}</Tag>}
                                  <Tag color="purple">{formatScore(ref.score)}</Tag>
                                </Space>
                              }
                              description={
                                <Space direction="vertical" size={6} style={{ width: '100%' }}>
                                  <Text type="secondary">{ref.reason}</Text>
                                  <Paragraph
                                    style={{
                                      margin: 0,
                                      whiteSpace: 'pre-wrap',
                                      wordBreak: 'break-word',
                                      maxHeight: 120,
                                      overflow: 'auto',
                                      background: 'var(--bg-secondary)',
                                      border: '1px solid var(--border-subtle)',
                                      borderRadius: 6,
                                      padding: '8px 10px',
                                    }}
                                  >
                                    {ref.preview}
                                  </Paragraph>
                                </Space>
                              }
                            />
                          </List.Item>
                        )}
                      />
                    )}
                  </Space>
                </Card>
              </Space>
            </Col>
          </Row>

          <Modal
            title={t('sqlKnowledge.createSpace')}
            open={createOpen}
            onCancel={() => setCreateOpen(false)}
            onOk={() => createForm.submit()}
            confirmLoading={createMutation.isPending}
          >
            <Form
              form={createForm}
              layout="vertical"
              initialValues={{ verified: true }}
              onFinish={createMutation.mutate}
            >
              <Form.Item name="name" label={t('sqlKnowledge.spaceName')} rules={[{ required: true }]}>
                <Input maxLength={120} />
              </Form.Item>
              <Form.Item name="description" label={t('sqlKnowledge.description')}>
                <TextArea rows={3} />
              </Form.Item>
              <Form.Item
                name="datasourceIds"
                label={t('sqlKnowledge.bindDatasources')}
                extra={t('sqlKnowledge.datasourceScopeDerivedHelp')}
                rules={[
                  {
                    validator: (_, value?: string[]) => {
                      if (!Array.isArray(value) || value.length === 0) {
                        return Promise.reject(new Error(t('sqlKnowledge.bindDatasourceRequired')));
                      }
                      if (value.includes(GLOBAL_DATASOURCE_ID) && value.length > 1) {
                        return Promise.reject(new Error(t('sqlKnowledge.globalCannotMix')));
                      }
                      const selectedVisibilities = new Set(
                        value
                          .map((id) => datasources.find((ds) => ds.id === id)?.visibility)
                          .filter(Boolean),
                      );
                      if (selectedVisibilities.size > 1) {
                        return Promise.reject(new Error(t('sqlKnowledge.mixedVisibilityNotAllowed')));
                      }
                      return Promise.resolve();
                    },
                  },
                ]}
              >
                <Select
                  mode="multiple"
                  showSearch
                  options={bindingDatasourceOptions}
                  optionFilterProp="label"
                  placeholder={t('sqlKnowledge.bindDatasourcesPlaceholder')}
                />
              </Form.Item>
              <Form.Item name="verified" label={t('sqlKnowledge.markVerified')} valuePropName="checked">
                <Switch />
              </Form.Item>
              <Form.Item name="tagsText" label={t('sqlKnowledge.tags')}>
                <Input placeholder={t('sqlKnowledge.tagsPlaceholder')} />
              </Form.Item>
            </Form>
          </Modal>

          <Drawer
            title={readTarget ? `${readTarget.filename} · L${readTarget.startLine}-${readTarget.endLine}` : t('sqlKnowledge.readContext')}
            open={Boolean(readTarget)}
            width={760}
            onClose={() => setReadTarget(null)}
          >
            <Paragraph
              style={{
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-word',
                fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)',
                fontSize: 12,
                background: 'var(--bg-secondary)',
                border: '1px solid var(--border-subtle)',
                borderRadius: 8,
                padding: 12,
              }}
            >
              {readQuery.data?.content ?? (readQuery.isLoading ? t('common.loading') : '')}
            </Paragraph>
          </Drawer>

          <Drawer
            title={previewFile ? `${t('sqlKnowledge.previewFile')} · ${previewFile.filename}` : t('sqlKnowledge.previewFile')}
            open={Boolean(previewFile)}
            width={900}
            onClose={() => setPreviewFile(null)}
            extra={
              previewFile ? (
                <Button
                  icon={<EditOutlined />}
                  disabled={!embeddingAvailable || selectedSpacePrimaryDeleted || !selectedSpace?.writable}
                  onClick={() => {
                    setEditFile(previewFile);
                    setPreviewFile(null);
                  }}
                >
                  {t('sqlKnowledge.editFile')}
                </Button>
              ) : null
            }
          >
            <Paragraph
              style={{
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-word',
                fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)',
                fontSize: 12,
                background: 'var(--bg-secondary)',
                border: '1px solid var(--border-subtle)',
                borderRadius: 8,
                padding: 12,
                maxHeight: 'calc(100vh - 180px)',
                overflow: 'auto',
              }}
            >
              {previewQuery.data?.content ?? (previewQuery.isLoading ? t('common.loading') : '')}
            </Paragraph>
          </Drawer>

          <Modal
            title={editFile ? `${t('sqlKnowledge.editFile')} · ${editFile.filename}` : t('sqlKnowledge.editFile')}
            open={Boolean(editFile)}
            width={980}
            okText={t('sqlKnowledge.saveAndReindex')}
            cancelText={t('common.cancel')}
            confirmLoading={updateFileMutation.isPending}
            onCancel={() => {
              setEditFile(null);
              setEditContent('');
            }}
            onOk={() => {
              if (!editFile) return;
              if (!editContent.trim()) {
                message.warning(t('sqlKnowledge.emptyEditContent'));
                return;
              }
              updateFileMutation.mutate({ fileId: editFile.id, content: editContent });
            }}
          >
            <Alert
              type="info"
              showIcon
              style={{ marginBottom: 12 }}
              message={t('sqlKnowledge.editReindexHint')}
            />
            <TextArea
              value={editQuery.isLoading ? t('common.loading') : editContent}
              onChange={(event) => setEditContent(event.target.value)}
              rows={24}
              disabled={editQuery.isLoading || updateFileMutation.isPending}
              style={{
                fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)',
                fontSize: 12,
              }}
            />
          </Modal>
        </Layout.Content>
      </Layout>
    </ErrorBoundary>
  );
}
