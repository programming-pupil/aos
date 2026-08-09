import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { Empty, Input, List, Modal, Space, Tag, Typography } from 'antd';
import {
  BranchesOutlined,
  CodeOutlined,
  DiffOutlined,
  FileSearchOutlined,
  FunctionOutlined,
  PlayCircleOutlined,
  SearchOutlined,
} from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { RdWorkspaceTabKey } from './types';

const { Text } = Typography;

type CommandPaletteItem = {
  key: string;
  label: string;
  description?: string;
  shortcut?: string;
  group: string;
  icon?: ReactNode;
  run: () => void;
};

export function CommandPalette({
  open,
  hasRepository,
  hasTask,
  hasPendingChanges,
  onClose,
  onSelectTab,
  onQuickOpenFiles,
  onQuickOpenSymbols,
  onApplyAll,
  onRunTest,
  onStartPreview,
}: {
  open: boolean;
  hasRepository: boolean;
  hasTask: boolean;
  hasPendingChanges: boolean;
  onClose: () => void;
  onSelectTab: (tab: RdWorkspaceTabKey) => void;
  onQuickOpenFiles: () => void;
  onQuickOpenSymbols: () => void;
  onApplyAll: () => void;
  onRunTest: () => void;
  onStartPreview: () => void;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState('');

  useEffect(() => {
    if (open) setQuery('');
  }, [open]);

  const items = useMemo<CommandPaletteItem[]>(() => {
    const closeAndRun = (run: () => void) => () => {
      run();
      onClose();
    };
    return [
      {
        key: 'quick-open-files',
        label: t('rd.commandQuickOpenFiles', '快速打开文件'),
        description: t('rd.commandQuickOpenFilesDesc', '按文件名或路径查找并打开文件'),
        shortcut: 'Cmd/Ctrl+P',
        group: t('rd.commandGroupNavigation', '导航'),
        icon: <FileSearchOutlined />,
        run: closeAndRun(onQuickOpenFiles),
      },
      {
        key: 'quick-open-symbols',
        label: t('rd.commandQuickOpenSymbols', '跳转符号'),
        description: t('rd.commandQuickOpenSymbolsDesc', '按方法、变量或类型名跳转'),
        shortcut: 'Cmd/Ctrl+Shift+O',
        group: t('rd.commandGroupNavigation', '导航'),
        icon: <FunctionOutlined />,
        run: closeAndRun(onQuickOpenSymbols),
      },
      {
        key: 'show-editor',
        label: t('rd.commandShowEditor', '打开编辑器'),
        description: t('rd.commandShowEditorDesc', '查看当前文件和代码智能'),
        group: t('rd.commandGroupWorkspace', '工作区'),
        icon: <CodeOutlined />,
        run: closeAndRun(() => onSelectTab('file')),
      },
      {
        key: 'show-diff',
        label: t('rd.commandShowDiff', '查看 Diff'),
        description: t('rd.commandShowDiffDesc', '审查候选变更并选择 hunks'),
        group: t('rd.commandGroupWorkspace', '工作区'),
        icon: <DiffOutlined />,
        run: closeAndRun(() => onSelectTab('diff')),
      },
      {
        key: 'show-references',
        label: t('rd.commandShowReferences', '查看引用'),
        description: t('rd.commandShowReferencesDesc', '打开最近一次查找引用结果'),
        group: t('rd.commandGroupWorkspace', '工作区'),
        icon: <BranchesOutlined />,
        run: closeAndRun(() => onSelectTab('references')),
      },
      {
        key: 'show-preview',
        label: t('rd.commandShowPreview', '打开预览调试'),
        description: t('rd.commandShowPreviewDesc', '启动或查看前端预览、console 和 network'),
        group: t('rd.commandGroupWorkspace', '工作区'),
        icon: <PlayCircleOutlined />,
        run: closeAndRun(onStartPreview),
      },
      {
        key: 'run-test',
        label: t('rd.commandRunTest', '运行测试'),
        description: t('rd.commandRunTestDesc', '运行当前任务的测试命令'),
        group: t('rd.commandGroupActions', '动作'),
        icon: <PlayCircleOutlined />,
        run: closeAndRun(onRunTest),
      },
      {
        key: 'apply-all',
        label: t('rd.commandApplyAll', '应用全部 Diff'),
        description: t('rd.commandApplyAllDesc', '将所有待应用变更应用到主工作区'),
        group: t('rd.commandGroupActions', '动作'),
        icon: <DiffOutlined />,
        run: closeAndRun(onApplyAll),
      },
    ];
  }, [
    onApplyAll,
    onClose,
    onQuickOpenFiles,
    onQuickOpenSymbols,
    onRunTest,
    onSelectTab,
    onStartPreview,
    t,
  ]);

  const filtered = items.filter((item) => {
    if (!hasRepository && ['quick-open-files', 'quick-open-symbols', 'show-editor', 'show-preview'].includes(item.key)) {
      return false;
    }
    if (!hasTask && ['show-diff', 'show-references', 'run-test', 'apply-all'].includes(item.key)) {
      return false;
    }
    if (!hasPendingChanges && item.key === 'apply-all') {
      return false;
    }
    const haystack = `${item.label} ${item.description ?? ''} ${item.group}`.toLowerCase();
    return haystack.includes(query.trim().toLowerCase());
  });

  function runFirst() {
    filtered[0]?.run();
  }

  return (
    <Modal
      title={(
        <Space>
          <SearchOutlined />
          <span>{t('rd.commandPalette', '命令面板')}</span>
          <Tag color="blue">Cmd/Ctrl+Shift+P</Tag>
        </Space>
      )}
      open={open}
      onCancel={onClose}
      footer={null}
      width={720}
      destroyOnHidden
      styles={{
        body: { background: '#020617', paddingTop: 8 },
        header: { background: '#07111f', borderBottomColor: 'rgba(148, 163, 184, 0.18)' },
        content: { background: '#07111f' },
      }}
    >
      <Input
        autoFocus
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        onPressEnter={runFirst}
        placeholder={t('rd.commandPalettePlaceholder', '输入命令，例如 Diff、测试、预览、符号...')}
      />
      <div className="rd-quick-open-list">
        {filtered.length === 0 ? (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={<span style={{ color: '#94a3b8' }}>{t('rd.quickOpenEmpty', '没有匹配结果')}</span>}
          />
        ) : (
          <List
            size="small"
            dataSource={filtered}
            renderItem={(item) => (
              <List.Item className="rd-quick-open-item" onClick={item.run}>
                <Space align="start" style={{ minWidth: 0, width: '100%', justifyContent: 'space-between' }}>
                  <Space size={10} style={{ minWidth: 0 }}>
                    <span className="rd-command-palette-icon">{item.icon}</span>
                    <Space direction="vertical" size={2} style={{ minWidth: 0 }}>
                      <Space size={6} wrap>
                        <Text className="rd-quick-open-primary">{item.label}</Text>
                        <Tag>{item.group}</Tag>
                      </Space>
                      {item.description ? <Text className="rd-quick-open-secondary">{item.description}</Text> : null}
                    </Space>
                  </Space>
                  {item.shortcut ? <Tag color="cyan">{item.shortcut}</Tag> : null}
                </Space>
              </List.Item>
            )}
          />
        )}
      </div>
    </Modal>
  );
}
