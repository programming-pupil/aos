import { useEffect, useMemo, useState } from 'react';
import { Empty, Input, List, Modal, Space, Tag, Typography } from 'antd';
import { FileSearchOutlined, FunctionOutlined } from '@ant-design/icons';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { RdRepository } from '@/types';

const { Text } = Typography;

export function QuickOpenPalette({
  open,
  mode,
  repository,
  onClose,
  onOpen,
}: {
  open: boolean;
  mode: 'file' | 'symbol';
  repository?: RdRepository | null;
  onClose: () => void;
  onOpen: (path: string, line?: number, character?: number) => void;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState('');

  useEffect(() => {
    if (open) setQuery('');
  }, [open, mode]);

  const fileQuery = useQuery({
    queryKey: repository?.id ? queryKeys.rd.repositoryFileSuggestions([repository.id], query, 40) : ['rd', 'quickOpen', 'files', 'none'],
    queryFn: () => rdApi.repositoryFileSuggestions(repository!.id, { q: query, limit: 40 }),
    enabled: open && mode === 'file' && !!repository?.id,
    staleTime: 20_000,
  });

  const symbolQuery = useQuery({
    queryKey: repository?.id ? queryKeys.rd.repositorySymbols(repository.id, query, 60) : ['rd', 'quickOpen', 'symbols', 'none'],
    queryFn: () => rdApi.repositorySymbols(repository!.id, { q: query, limit: 60 }),
    enabled: open && mode === 'symbol' && !!repository?.id,
    staleTime: 20_000,
  });

  const fileItems = useMemo(() => fileQuery.data ?? [], [fileQuery.data]);
  const symbolItems = useMemo(() => symbolQuery.data ?? [], [symbolQuery.data]);

  const title = mode === 'symbol'
    ? t('rd.quickOpenSymbols', '跳转符号')
    : t('rd.quickOpenFiles', '快速打开文件');

  function openFirst() {
    if (mode === 'symbol') {
      const first = symbolItems[0];
      if (!first) return;
      onOpen(first.filePath, Math.max(1, first.lineNumber), 1);
    } else {
      const first = fileItems[0];
      if (!first) return;
      onOpen(first.path);
    }
    onClose();
  }

  const empty = mode === 'symbol' ? symbolItems.length === 0 : fileItems.length === 0;

  return (
    <Modal
      title={(
        <Space>
          {mode === 'symbol' ? <FunctionOutlined /> : <FileSearchOutlined />}
          <span>{title}</span>
          {repository ? <Tag color="cyan">{repository.name}</Tag> : null}
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
        container: { background: '#07111f' },
      }}
    >
      <Input
        autoFocus
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        onPressEnter={openFirst}
        placeholder={mode === 'symbol'
          ? t('rd.quickOpenSymbolPlaceholder', '输入方法、变量、类型名...')
          : t('rd.quickOpenFilePlaceholder', '输入文件名或路径...')}
      />
      <div className="rd-quick-open-list">
        {empty ? (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={<span style={{ color: '#94a3b8' }}>{t('rd.quickOpenEmpty', '没有匹配结果')}</span>}
          />
        ) : mode === 'symbol' ? (
          <List
            size="small"
            dataSource={symbolItems}
            renderItem={(item) => (
              <List.Item
                className="rd-quick-open-item"
                onClick={() => {
                  onOpen(item.filePath, Math.max(1, item.lineNumber), 1);
                  onClose();
                }}
              >
                <Space direction="vertical" size={2} style={{ minWidth: 0 }}>
                  <Space size={6} wrap>
                    <Text className="rd-quick-open-primary">{item.symbolName}</Text>
                    <Tag>{item.symbolKind}</Tag>
                    {item.language ? <Tag color="blue">{item.language}</Tag> : null}
                  </Space>
                  <Text className="rd-quick-open-secondary">{item.filePath}:{item.lineNumber}</Text>
                </Space>
              </List.Item>
            )}
          />
        ) : (
          <List
            size="small"
            dataSource={fileItems}
            renderItem={(item) => (
                <List.Item
                  className="rd-quick-open-item"
                  onClick={() => {
                    onOpen(item.path);
                    onClose();
                  }}
                >
                  <Space direction="vertical" size={2} style={{ minWidth: 0 }}>
                    <Text className="rd-quick-open-primary">{item.name}</Text>
                    <Text className="rd-quick-open-secondary">{item.path}</Text>
                  </Space>
                </List.Item>
            )}
          />
        )}
      </div>
    </Modal>
  );
}
