import { useCallback, useEffect, useMemo, useState } from 'react';
import Editor from '@monaco-editor/react';
import { Alert, Button, Empty, Space, Spin, Tabs, Tag, Tooltip, Typography, message } from 'antd';
import {
  AimOutlined,
  BranchesOutlined,
  FileSearchOutlined,
  ReloadOutlined,
} from '@ant-design/icons';
import { useMutation, useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  RdCodeIntelAction,
  RdCodeIntelLocation,
  RdCodeIntelQueryResponse,
  RdFileContentResponse,
  RdRepository,
} from '@/types';
import { repoLabel } from './utils';
import { CodeIntelPopover } from './CodeIntelPopover';
import { DefinitionCandidates } from './DefinitionCandidates';

const { Text } = Typography;

type OpenFileTab = {
  path: string;
  pinned?: boolean;
  revealLine?: number;
  revealColumn?: number;
};

const languageMap: Record<string, string> = {
  typescript: 'typescript',
  javascript: 'javascript',
  tsx: 'typescript',
  jsx: 'javascript',
  rust: 'rust',
  python: 'python',
  go: 'go',
  java: 'java',
  c: 'c',
  cpp: 'cpp',
  json: 'json',
  yaml: 'yaml',
  markdown: 'markdown',
  shell: 'shell',
  sql: 'sql',
};

function editorLanguage(file?: RdFileContentResponse | null) {
  const language = file?.language?.toLowerCase();
  if (language && languageMap[language]) return languageMap[language];
  const ext = file?.path.split('.').pop()?.toLowerCase();
  return ext && languageMap[ext] ? languageMap[ext] : language || 'plaintext';
}

function editorLine(position: RdCodeIntelLocation) {
  return Math.max(1, position.line + 1);
}

export function CodeEditorPanel({
  repository,
  path,
  revealLine,
  revealColumn,
  onOpenPath,
  onReferences,
}: {
  repository?: RdRepository | null;
  path?: string | null;
  revealLine?: number;
  revealColumn?: number;
  onOpenPath?: (path: string) => void;
  onReferences?: (locations: RdCodeIntelLocation[]) => void;
}) {
  const { t } = useTranslation();
  const [openTabs, setOpenTabs] = useState<OpenFileTab[]>([]);
  const [activePath, setActivePath] = useState<string | undefined>(path ?? undefined);
  const [editorInstance, setEditorInstance] = useState<any>(null);
  const [lastResult, setLastResult] = useState<RdCodeIntelQueryResponse | null>(null);
  const [candidateOpen, setCandidateOpen] = useState(false);
  const [candidateTitle, setCandidateTitle] = useState<string>();

  useEffect(() => {
    if (!path) return;
    setActivePath(path);
    setOpenTabs((tabs) => {
      const nextTab = { path, revealLine, revealColumn };
      const index = tabs.findIndex((tab) => tab.path === path);
      if (index >= 0) {
        const next = [...tabs];
        next[index] = { ...next[index], ...nextTab };
        return next;
      }
      return [...tabs, nextTab];
    });
  }, [path, revealColumn, revealLine]);

  const fileQuery = useQuery({
    queryKey: repository?.id && activePath ? queryKeys.rd.repositoryFile(repository.id, activePath) : ['rd', 'codeEditor', 'none'],
    queryFn: () => rdApi.repositoryFile(repository!.id, activePath!),
    enabled: !!repository?.id && !!activePath,
    staleTime: 30_000,
  });

  const codeIntelStatusQuery = useQuery({
    queryKey: repository?.id ? queryKeys.rd.codeIntelStatus(repository.id) : queryKeys.rd.codeIntelStatus(undefined),
    queryFn: () => rdApi.codeIntelStatus(repository!.id),
    enabled: !!repository?.id,
    staleTime: 60_000,
  });

  const queryMutation = useMutation({
    mutationFn: (data: { action: RdCodeIntelAction; query?: string; line?: number; character?: number }) =>
      rdApi.codeIntelQuery(repository!.id, {
        action: data.action,
        path: activePath,
        line: data.line,
        character: data.character,
        query: data.query,
      }),
    onSuccess: (result, variables) => {
      setLastResult(result);
      if (variables.action === 'hover') return;
      if (variables.action === 'references') {
        onReferences?.(result.locations);
      }
      if (result.locations.length === 1 && variables.action === 'definition') {
        openLocation(result.locations[0]);
        return;
      }
      setCandidateTitle(variables.action === 'references' ? t('rd.findReferences', '查找引用') : t('rd.goToDefinition', '跳转定义'));
      setCandidateOpen(true);
      if (result.locations.length === 0 && result.message) {
        message.info(result.message);
      }
    },
    onError: (error) => {
      message.error((error as Error).message || t('rd.codeIntelQueryFailed', '代码智能查询失败'));
    },
  });

  const activeFile = fileQuery.data;
  const tabs = useMemo(() => openTabs.map((tab) => ({
    key: tab.path,
    label: (
      <Tooltip title={tab.path}>
        <span className="rd-code-tab-label">{tab.path.split('/').pop() || tab.path}</span>
      </Tooltip>
    ),
    closable: true,
  })), [openTabs]);

  const closeTab = useCallback((targetPath: string) => {
    setOpenTabs((tabs) => {
      const next = tabs.filter((tab) => tab.path !== targetPath);
      if (activePath === targetPath) {
        const fallback = next[next.length - 1]?.path;
        setActivePath(fallback);
        if (fallback) onOpenPath?.(fallback);
      }
      return next;
    });
  }, [activePath, onOpenPath]);

  const openLocation = useCallback((location: RdCodeIntelLocation) => {
    setCandidateOpen(false);
    setActivePath(location.path);
    setOpenTabs((tabs) => {
      const nextTab = {
        path: location.path,
        revealLine: editorLine(location),
        revealColumn: Math.max(1, location.character + 1),
        pinned: true,
      };
      const index = tabs.findIndex((tab) => tab.path === location.path);
      if (index >= 0) {
        const next = [...tabs];
        next[index] = { ...next[index], ...nextTab };
        return next;
      }
      return [...tabs, nextTab];
    });
    onOpenPath?.(location.path);
  }, [onOpenPath]);

  useEffect(() => {
    if (!editorInstance || !activePath) return;
    const tab = openTabs.find((item) => item.path === activePath);
    if (!tab?.revealLine) return;
    window.setTimeout(() => {
      editorInstance.revealLineInCenter?.(tab.revealLine);
      editorInstance.setPosition?.({ lineNumber: tab.revealLine, column: tab.revealColumn ?? 1 });
      editorInstance.focus?.();
    }, 50);
  }, [activePath, editorInstance, openTabs]);

  const runCodeIntel = useCallback((action: RdCodeIntelAction) => {
    if (!repository?.id || !activePath) return;
    const position = editorInstance?.getPosition?.();
    queryMutation.mutate({
      action,
      line: typeof position?.lineNumber === 'number' ? Math.max(0, position.lineNumber - 1) : undefined,
      character: typeof position?.column === 'number' ? Math.max(0, position.column - 1) : undefined,
    });
  }, [activePath, editorInstance, queryMutation, repository?.id]);

  function handleEditorMount(editor: any) {
    setEditorInstance(editor);
    editor.onMouseDown?.((event: any) => {
      if (!(event.event?.metaKey || event.event?.ctrlKey)) return;
      const position = event.target?.position;
      if (!position || !repository?.id || !activePath) return;
      queryMutation.mutate({
        action: 'definition',
        line: Math.max(0, position.lineNumber - 1),
        character: Math.max(0, position.column - 1),
      });
    });
  }

  if (!repository || !activePath) {
    return (
      <Empty
        image={Empty.PRESENTED_IMAGE_SIMPLE}
        description={<span style={{ color: '#94a3b8' }}>{t('rd.filePreviewEmpty', '从左侧文件树选择一个文件查看内容')}</span>}
      />
    );
  }

  if (fileQuery.error) {
    return (
      <Alert
        type="error"
        showIcon
        message={t('rd.filePreviewFailed', '文件预览加载失败')}
        description={(fileQuery.error as Error).message}
        action={(
          <Button size="small" icon={<ReloadOutlined />} onClick={() => fileQuery.refetch()}>
            {t('common.retry', '重试')}
          </Button>
        )}
      />
    );
  }

  return (
    <div className="rd-code-editor-panel">
      <div className="rd-code-editor-toolbar">
        <Space size={6} wrap style={{ minWidth: 0 }}>
          <Tag color="cyan">{repoLabel(repository)}</Tag>
          {activeFile?.language ? <Tag>{activeFile.language}</Tag> : null}
          {activeFile ? <Tag>{t('rd.fileSizeBytes', '{{count}} bytes', { count: activeFile.sizeBytes })}</Tag> : null}
          <CodeIntelPopover status={codeIntelStatusQuery.data} lastResult={lastResult} />
        </Space>
        <Space size={6} wrap>
          <Button size="small" icon={<AimOutlined />} loading={queryMutation.isPending} onClick={() => runCodeIntel('definition')}>
            {t('rd.goToDefinition', '跳转定义')}
          </Button>
          <Button size="small" icon={<BranchesOutlined />} loading={queryMutation.isPending} onClick={() => runCodeIntel('references')}>
            {t('rd.findReferences', '查找引用')}
          </Button>
          <Button size="small" icon={<FileSearchOutlined />} loading={queryMutation.isPending} onClick={() => runCodeIntel('hover')}>
            {t('rd.hoverInfo', 'Hover')}
          </Button>
          <Button size="small" icon={<ReloadOutlined />} loading={fileQuery.isFetching} onClick={() => fileQuery.refetch()} />
        </Space>
      </div>
      <Text copyable className="rd-file-preview-path">{activePath}</Text>
      {tabs.length > 0 ? (
        <Tabs
          size="small"
          type="editable-card"
          hideAdd
          activeKey={activePath}
          items={tabs}
          onChange={(next) => {
            setActivePath(next);
            onOpenPath?.(next);
          }}
          onEdit={(targetKey, action) => {
            if (action === 'remove' && typeof targetKey === 'string') closeTab(targetKey);
          }}
          className="rd-code-editor-tabs"
        />
      ) : null}
      <div className="rd-code-editor-body">
        {fileQuery.isLoading ? (
          <div className="rd-file-preview-loading">
            <Spin />
            <Text style={{ color: '#94a3b8' }}>{t('rd.filePreviewLoading', '正在读取文件...')}</Text>
          </div>
        ) : (
          <Editor
            height="min(64vh, 720px)"
            theme="vs-dark"
            language={editorLanguage(activeFile)}
            value={activeFile?.content ?? ''}
            options={{
              readOnly: true,
              minimap: { enabled: false },
              fontSize: 13,
              lineHeight: 20,
              scrollBeyondLastLine: false,
              wordWrap: 'off',
              automaticLayout: true,
              renderLineHighlight: 'all',
            }}
            onMount={handleEditorMount}
          />
        )}
      </div>
      <DefinitionCandidates
        open={candidateOpen}
        title={candidateTitle}
        source={lastResult?.source}
        message={lastResult?.message}
        locations={lastResult?.locations ?? []}
        onClose={() => setCandidateOpen(false)}
        onOpenLocation={openLocation}
      />
    </div>
  );
}
