import { useMemo, useRef, useState } from 'react';
import { useInfiniteQuery, useQueryClient } from '@tanstack/react-query';
import {
  Breadcrumb,
  Button,
  Drawer,
  Empty,
  Input,
  Modal,
  Popconfirm,
  Segmented,
  Space,
  Table,
  Tag,
  Tooltip,
  Typography,
  message,
} from 'antd';
import type { ColumnsType } from 'antd/es/table';
import {
  CopyOutlined,
  DeleteOutlined,
  DownloadOutlined,
  EditOutlined,
  FileAddOutlined,
  FileOutlined,
  FolderAddOutlined,
  FolderOpenOutlined,
  ReloadOutlined,
  SearchOutlined,
  UploadOutlined,
} from '@ant-design/icons';
import dayjs from 'dayjs';
import { useTranslation } from 'react-i18next';
import {
  agentApi,
  personalWorkspaceApi,
  uploadFile,
  type WorkspaceFileItem,
  type WorkspaceUploadItem,
} from '@/api';

const { Text, Title } = Typography;
const ROOT_PATH = '/projects/session';

type DialogState =
  | { kind: 'new-file'; value: string }
  | { kind: 'new-folder'; value: string }
  | { kind: 'rename'; value: string; item: WorkspaceFileItem }
  | null;

function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size >= 10 || unit === 0 ? size.toFixed(0) : size.toFixed(1)} ${units[unit]}`;
}

function formatTime(value?: string | null): string {
  if (!value) return '-';
  const parsed = /^\d+$/.test(value) ? dayjs.unix(Number(value)) : dayjs(value);
  return parsed.isValid() ? parsed.format('YYYY-MM-DD HH:mm:ss') : value;
}

function downloadBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

function parentPath(path: string): string {
  if (path === ROOT_PATH) return ROOT_PATH;
  const parent = path.slice(0, path.lastIndexOf('/'));
  return parent.startsWith(ROOT_PATH) ? parent : ROOT_PATH;
}

function childPath(parent: string, name: string): string {
  return `${parent.replace(/\/$/, '')}/${name.trim()}`;
}

export default function Workspace() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [mode, setMode] = useState<'files' | 'uploads'>('files');
  const [path, setPath] = useState(ROOT_PATH);
  const [search, setSearch] = useState('');
  const [dialog, setDialog] = useState<DialogState>(null);
  const [dialogSaving, setDialogSaving] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [editorOpen, setEditorOpen] = useState(false);
  const [editorPath, setEditorPath] = useState('');
  const [editorName, setEditorName] = useState('');
  const [editorContent, setEditorContent] = useState('');
  const [editorLoading, setEditorLoading] = useState(false);
  const [editorSaving, setEditorSaving] = useState(false);

  const filesQuery = useInfiniteQuery({
    queryKey: ['personal-workspace', 'files', path],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) => personalWorkspaceApi.listFiles({ path, cursor: pageParam, limit: 100 }),
    getNextPageParam: (page) => page.hasMore ? page.nextCursor ?? undefined : undefined,
    enabled: mode === 'files',
  });
  const uploadsQuery = useInfiniteQuery({
    queryKey: ['personal-workspace', 'uploads'],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) => personalWorkspaceApi.listUploads({ cursor: pageParam, limit: 50 }),
    getNextPageParam: (page) => page.hasMore ? page.nextCursor ?? undefined : undefined,
    enabled: mode === 'uploads',
  });

  const fileRows = useMemo(() => {
    const query = search.trim().toLowerCase();
    const seen = new Set<string>();
    return (filesQuery.data?.pages ?? []).flatMap((page) => page.items).filter((item) => {
      if (seen.has(item.path)) return false;
      seen.add(item.path);
      return !query || item.name.toLowerCase().includes(query);
    });
  }, [filesQuery.data?.pages, search]);
  const absolutePath = filesQuery.data?.pages[0]?.absolutePath;
  const uploadRows = useMemo(() => {
    const query = search.trim().toLowerCase();
    const seen = new Set<string>();
    return (uploadsQuery.data?.pages ?? []).flatMap((page) => page.items).filter((item) => {
      if (seen.has(item.fileId)) return false;
      seen.add(item.fileId);
      return !query || item.filename.toLowerCase().includes(query);
    });
  }, [search, uploadsQuery.data?.pages]);

  const invalidateFiles = async () => {
    await queryClient.invalidateQueries({ queryKey: ['personal-workspace', 'files'] });
  };
  const invalidateUploads = async () => {
    await queryClient.invalidateQueries({ queryKey: ['personal-workspace', 'uploads'] });
  };

  const openEditor = async (item: WorkspaceFileItem) => {
    setEditorOpen(true);
    setEditorPath(item.path);
    setEditorName(item.name);
    setEditorContent('');
    setEditorLoading(true);
    try {
      const file = await personalWorkspaceApi.readFile(item.path);
      setEditorContent(file.content);
    } catch (error) {
      message.error(`${t('workspace.readFailed', 'Failed to read file')}: ${(error as Error).message}`);
      setEditorOpen(false);
    } finally {
      setEditorLoading(false);
    }
  };

  const saveEditor = async () => {
    setEditorSaving(true);
    try {
      await personalWorkspaceApi.saveFile({ path: editorPath, content: editorContent, overwrite: true });
      message.success(t('workspace.saved', 'Saved'));
      await invalidateFiles();
    } catch (error) {
      message.error(`${t('workspace.saveFailed', 'Failed to save')}: ${(error as Error).message}`);
    } finally {
      setEditorSaving(false);
    }
  };

  const submitDialog = async () => {
    if (!dialog || !dialog.value.trim()) return;
    setDialogSaving(true);
    try {
      if (dialog.kind === 'new-folder') {
        await personalWorkspaceApi.createDirectory(childPath(path, dialog.value));
      } else if (dialog.kind === 'new-file') {
        const createdPath = childPath(path, dialog.value);
        await personalWorkspaceApi.saveFile({ path: createdPath, content: '', overwrite: false });
      } else {
        await personalWorkspaceApi.renameItem(dialog.item.path, dialog.value);
      }
      setDialog(null);
      await invalidateFiles();
    } catch (error) {
      message.error(`${t('common.operationFailed')}: ${(error as Error).message}`);
    } finally {
      setDialogSaving(false);
    }
  };

  const deleteFileItem = async (item: WorkspaceFileItem) => {
    try {
      await personalWorkspaceApi.deleteItem(item.path, item.kind === 'directory');
      await invalidateFiles();
    } catch (error) {
      message.error(`${t('common.operationFailed')}: ${(error as Error).message}`);
    }
  };

  const downloadFileItem = async (item: WorkspaceFileItem) => {
    try {
      downloadBlob(await personalWorkspaceApi.downloadFile(item.path), item.name);
    } catch (error) {
      message.error(`${t('workspace.downloadFailed', 'Download failed')}: ${(error as Error).message}`);
    }
  };

  const uploadSelectedFiles = async (files: FileList) => {
    setUploading(true);
    try {
      for (const file of Array.from(files)) {
        if (mode === 'files') {
          await personalWorkspaceApi.uploadFile(path, file);
        } else {
          const uploaded = await uploadFile(file);
          await agentApi.registerChatFile({
            fileId: uploaded.fileId,
            filename: uploaded.filename,
            mediaType: uploaded.mediaType,
            size: uploaded.size,
            url: uploaded.url,
            sessionId: null,
          });
        }
      }
      if (mode === 'files') await invalidateFiles();
      else await invalidateUploads();
      message.success(t('workspace.uploaded', 'Uploaded'));
    } catch (error) {
      message.error(`${t('chat.uploadFailed')}: ${(error as Error).message}`);
    } finally {
      setUploading(false);
      if (fileInputRef.current) fileInputRef.current.value = '';
    }
  };

  const deleteUpload = async (item: WorkspaceUploadItem) => {
    try {
      await personalWorkspaceApi.deleteUpload(item.fileId);
      await invalidateUploads();
    } catch (error) {
      message.error(`${t('common.operationFailed')}: ${(error as Error).message}`);
    }
  };

  const downloadUpload = async (item: WorkspaceUploadItem) => {
    try {
      downloadBlob(await personalWorkspaceApi.downloadUpload(item.url), item.filename);
    } catch (error) {
      message.error(`${t('workspace.downloadFailed', 'Download failed')}: ${(error as Error).message}`);
    }
  };

  const fileColumns: ColumnsType<WorkspaceFileItem> = [
    {
      title: t('common.name'),
      dataIndex: 'name',
      ellipsis: true,
      render: (_, item) => (
        <button
          type="button"
          onDoubleClick={() => item.kind === 'directory' ? setPath(item.path) : item.editable && void openEditor(item)}
          onClick={() => item.kind === 'directory' ? setPath(item.path) : undefined}
          style={{ border: 0, background: 'transparent', color: 'var(--text-primary)', cursor: item.kind === 'directory' ? 'pointer' : 'default', padding: 0 }}
        >
          <Space size={8}>
            {item.kind === 'directory' ? <FolderOpenOutlined style={{ color: '#eab308' }} /> : <FileOutlined />}
            <Text>{item.name}</Text>
          </Space>
        </button>
      ),
    },
    { title: t('common.type'), width: 110, render: (_, item) => item.kind === 'directory' ? t('workspace.folder', 'Folder') : t('workspace.file', 'File') },
    { title: t('workspace.size', 'Size'), dataIndex: 'sizeBytes', width: 110, render: (value) => formatBytes(value) },
    { title: t('common.updatedAt'), dataIndex: 'updatedAt', width: 180, render: formatTime },
    {
      title: t('common.actions'),
      width: 180,
      align: 'right',
      render: (_, item) => (
        <Space size={2}>
          {item.kind === 'file' && item.editable && (
            <Tooltip title={t('common.edit')}><Button type="text" icon={<EditOutlined />} onClick={() => void openEditor(item)} /></Tooltip>
          )}
          {item.kind === 'file' && (
            <Tooltip title={t('workspace.download', 'Download')}><Button type="text" icon={<DownloadOutlined />} onClick={() => void downloadFileItem(item)} /></Tooltip>
          )}
          <Tooltip title={t('common.rename')}><Button type="text" icon={<EditOutlined />} onClick={() => setDialog({ kind: 'rename', value: item.name, item })} /></Tooltip>
          <Popconfirm title={t('common.deleteConfirm')} onConfirm={() => void deleteFileItem(item)}>
            <Tooltip title={t('common.delete')}><Button type="text" danger icon={<DeleteOutlined />} /></Tooltip>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const uploadColumns: ColumnsType<WorkspaceUploadItem> = [
    { title: t('common.name'), dataIndex: 'filename', ellipsis: true, render: (name) => <Space><FileOutlined /><Text>{name}</Text></Space> },
    { title: t('common.status'), dataIndex: 'status', width: 110, render: (status) => <Tag color={status === 'indexed' ? 'green' : status === 'failed' ? 'red' : 'blue'}>{status}</Tag> },
    { title: t('workspace.size', 'Size'), dataIndex: 'sizeBytes', width: 110, render: (value) => formatBytes(value) },
    { title: t('common.updatedAt'), dataIndex: 'updatedAt', width: 180, render: formatTime },
    {
      title: t('common.actions'), width: 110, align: 'right', render: (_, item) => (
        <Space size={2}>
          <Tooltip title={t('workspace.download', 'Download')}><Button type="text" icon={<DownloadOutlined />} onClick={() => void downloadUpload(item)} /></Tooltip>
          <Popconfirm title={t('common.deleteConfirm')} onConfirm={() => void deleteUpload(item)}>
            <Tooltip title={t('common.delete')}><Button type="text" danger icon={<DeleteOutlined />} /></Tooltip>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const breadcrumbs = useMemo(() => {
    const relative = path.slice(ROOT_PATH.length).split('/').filter(Boolean);
    return [
      { title: <button type="button" onClick={() => setPath(ROOT_PATH)} style={{ border: 0, padding: 0, background: 'transparent', cursor: 'pointer', color: 'inherit' }}>{t('workspace.myFiles', 'My files')}</button> },
      ...relative.map((name, index) => ({
        title: <button type="button" onClick={() => setPath(`${ROOT_PATH}/${relative.slice(0, index + 1).join('/')}`)} style={{ border: 0, padding: 0, background: 'transparent', cursor: 'pointer', color: 'inherit' }}>{name}</button>,
      })),
    ];
  }, [path, t]);

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', minWidth: 0, overflow: 'hidden', background: 'var(--bg-void)' }}>
      <div style={{ padding: '20px 24px 14px', borderBottom: '1px solid var(--border-subtle)', background: 'var(--bg-surface)' }}>
        <Space align="center" style={{ justifyContent: 'space-between', width: '100%' }} wrap>
          <Title level={4} style={{ margin: 0 }}>{t('workspace.title', 'Workspace')}</Title>
          <Segmented
            value={mode}
            onChange={(value) => { setMode(value as 'files' | 'uploads'); setSearch(''); }}
            options={[
              { label: t('workspace.myFiles', 'My files'), value: 'files' },
              { label: t('workspace.assistantUploads', 'Assistant uploads'), value: 'uploads' },
            ]}
          />
        </Space>
      </div>

      <div style={{ padding: '12px 24px', borderBottom: '1px solid var(--border-subtle)', background: 'var(--bg-surface)' }}>
        <Space wrap style={{ justifyContent: 'space-between', width: '100%' }}>
          {mode === 'files' ? <Breadcrumb items={breadcrumbs} /> : <Text strong>{t('workspace.assistantUploads', 'Assistant uploads')}</Text>}
          <Space wrap>
            <Input allowClear prefix={<SearchOutlined />} value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t('common.search')} style={{ width: 220 }} />
            {mode === 'files' && path !== ROOT_PATH && <Button onClick={() => setPath(parentPath(path))}>{t('common.back')}</Button>}
            {mode === 'files' && <Button icon={<FolderAddOutlined />} onClick={() => setDialog({ kind: 'new-folder', value: '' })}>{t('workspace.newFolder', 'New folder')}</Button>}
            {mode === 'files' && <Button icon={<FileAddOutlined />} onClick={() => setDialog({ kind: 'new-file', value: '' })}>{t('workspace.newFile', 'New file')}</Button>}
            <Button icon={<UploadOutlined />} loading={uploading} onClick={() => fileInputRef.current?.click()}>{t('workspace.upload', 'Upload')}</Button>
            <Tooltip title={t('common.refresh')}><Button icon={<ReloadOutlined />} onClick={() => mode === 'files' ? void filesQuery.refetch() : void uploadsQuery.refetch()} /></Tooltip>
          </Space>
        </Space>
        {mode === 'files' && absolutePath ? (
          <div style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0, marginTop: 8 }}>
            <Text type="secondary" style={{ flex: '0 0 auto', fontSize: 12 }}>
              {t('workspace.absolutePath', 'Local path')}:
            </Text>
            <Text
              code
              ellipsis={{ tooltip: absolutePath }}
              style={{ minWidth: 0, maxWidth: 'min(760px, calc(100vw - 180px))' }}
            >
              {absolutePath}
            </Text>
            <Tooltip title={t('workspace.copyAbsolutePath', 'Copy local path')}>
              <Button
                type="text"
                size="small"
                icon={<CopyOutlined />}
                aria-label={t('workspace.copyAbsolutePath', 'Copy local path')}
                onClick={() => {
                  navigator.clipboard.writeText(absolutePath)
                    .then(() => message.success(t('workspace.pathCopied', 'Local path copied')))
                    .catch(() => message.error(t('workspace.pathCopyFailed', 'Failed to copy local path')));
                }}
              />
            </Tooltip>
          </div>
        ) : null}
        <input ref={fileInputRef} type="file" multiple style={{ display: 'none' }} onChange={(event) => event.target.files && void uploadSelectedFiles(event.target.files)} />
      </div>

      <div style={{ flex: 1, minHeight: 0, padding: '0 24px 20px', overflow: 'auto' }}>
        {mode === 'files' ? (
          <Table
            rowKey="path"
            columns={fileColumns}
            dataSource={fileRows}
            loading={filesQuery.isLoading}
            pagination={false}
            scroll={{ x: 780 }}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('workspace.emptyFolder', 'This folder is empty')} /> }}
          />
        ) : (
          <Table
            rowKey="fileId"
            columns={uploadColumns}
            dataSource={uploadRows}
            loading={uploadsQuery.isLoading}
            pagination={false}
            scroll={{ x: 720 }}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('workspace.noUploads', 'No assistant uploads')} /> }}
          />
        )}
        {((mode === 'files' && filesQuery.hasNextPage) || (mode === 'uploads' && uploadsQuery.hasNextPage)) && (
          <div style={{ display: 'flex', justifyContent: 'center', padding: 16 }}>
            <Button
              loading={mode === 'files' ? filesQuery.isFetchingNextPage : uploadsQuery.isFetchingNextPage}
              onClick={() => mode === 'files' ? void filesQuery.fetchNextPage() : void uploadsQuery.fetchNextPage()}
            >
              {t('workspace.loadMore', 'Load more')}
            </Button>
          </div>
        )}
      </div>

      <Modal
        title={dialog?.kind === 'new-folder' ? t('workspace.newFolder', 'New folder') : dialog?.kind === 'new-file' ? t('workspace.newFile', 'New file') : t('common.rename')}
        open={!!dialog}
        confirmLoading={dialogSaving}
        okButtonProps={{ disabled: !dialog?.value.trim() }}
        onCancel={() => setDialog(null)}
        onOk={() => void submitDialog()}
      >
        <Input autoFocus value={dialog?.value ?? ''} onChange={(event) => setDialog((current) => current ? { ...current, value: event.target.value } : current)} onPressEnter={() => void submitDialog()} />
      </Modal>

      <Drawer
        title={editorName}
        open={editorOpen}
        width="min(760px, 92vw)"
        onClose={() => setEditorOpen(false)}
        extra={<Button type="primary" loading={editorSaving} disabled={editorLoading} onClick={() => void saveEditor()}>{t('common.save')}</Button>}
      >
        <Input.TextArea
          value={editorContent}
          onChange={(event) => setEditorContent(event.target.value)}
          disabled={editorLoading}
          autoSize={{ minRows: 20 }}
          style={{ fontFamily: 'var(--font-code)', lineHeight: 1.55 }}
        />
      </Drawer>
    </div>
  );
}
