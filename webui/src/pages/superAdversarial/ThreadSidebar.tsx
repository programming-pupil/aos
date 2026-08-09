import {
  Button,
  Card,
  List,
  Popconfirm,
  Space,
  Tag,
  Tooltip,
  Typography,
} from 'antd';
import {
  DeleteOutlined,
  EditOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  PlusOutlined,
  PushpinFilled,
  PushpinOutlined,
} from '@ant-design/icons';
import type { TFunction } from 'i18next';
import type { ChatAdversarialRun } from '@/types';
import type { ThreadSummary } from './types';
import { adversarialStatusColor, getThreadDisplayTitle } from './utils';

const { Text, Paragraph } = Typography;

type Props = {
  t: TFunction;
  collapsed: boolean;
  threads: ThreadSummary[];
  selectedThreadId: string | null;
  composeNewRun: boolean;
  total: number;
  loading: boolean;
  fetchingNextPage: boolean;
  hasNextPage: boolean;
  pinningThreadId: string | null;
  deletingThreadId: string | null;
  renamingThreadId: string | null;
  onCollapseChange: (collapsed: boolean) => void;
  onNewRun: () => void;
  onSelectRun: (run: ChatAdversarialRun) => void;
  onTogglePin: (thread: ThreadSummary) => void;
  onRename: (thread: ThreadSummary) => void;
  onDelete: (thread: ThreadSummary) => void;
  onLoadMore: () => void;
};

export function ThreadSidebar({
  t,
  collapsed,
  threads,
  selectedThreadId,
  composeNewRun,
  total,
  loading,
  fetchingNextPage,
  hasNextPage,
  pinningThreadId,
  deletingThreadId,
  renamingThreadId,
  onCollapseChange,
  onNewRun,
  onSelectRun,
  onTogglePin,
  onRename,
  onDelete,
  onLoadMore,
}: Props) {
  return (
    <aside className="super-adversarial__sidebar">
      <div className="super-adversarial__sidebar-header">
        {collapsed ? (
          <Space direction="vertical" size={10} className="super-adversarial__collapsed-tools">
            <Tooltip title={t('chat.adversarialExpandSidebar')}>
              <Button size="small" icon={<MenuUnfoldOutlined />} onClick={() => onCollapseChange(false)} />
            </Tooltip>
            <Tooltip title={t('chat.adversarialNewSession')}>
              <Button size="small" type="primary" icon={<PlusOutlined />} onClick={onNewRun} />
            </Tooltip>
          </Space>
        ) : (
          <Space direction="vertical" size={10} className="super-adversarial__fill">
            <Space align="start" className="super-adversarial__between">
              <Space direction="vertical" size={2}>
                <Text strong>{t('chat.adversarialMode')}</Text>
                <Text type="secondary" className="super-adversarial__muted">
                  {t('chat.adversarialPageSubtitle')}
                </Text>
              </Space>
              <Tooltip title={t('chat.adversarialCollapseSidebar')}>
                <Button size="small" icon={<MenuFoldOutlined />} onClick={() => onCollapseChange(true)} />
              </Tooltip>
            </Space>
            <Space className="super-adversarial__between">
              <Text type="secondary" className="super-adversarial__muted">
                {t('chat.adversarialThreadTotal', { count: threads.length })}
              </Text>
              <Button size="small" type="primary" icon={<PlusOutlined />} onClick={onNewRun}>
                {t('chat.adversarialNewSession')}
              </Button>
            </Space>
          </Space>
        )}
      </div>
      {collapsed ? null : (
        <div
          className="super-adversarial__thread-list"
          onScroll={(event) => {
            if (fetchingNextPage || !hasNextPage) return;
            const target = event.currentTarget;
            if (target.scrollTop + target.clientHeight >= target.scrollHeight - 40) onLoadMore();
          }}
        >
          <List
            loading={loading}
            dataSource={threads}
            locale={{ emptyText: t('chat.adversarialEmpty') }}
            renderItem={(thread) => {
              const run = thread.latest;
              const selected = !composeNewRun && thread.threadId === selectedThreadId;
              return (
                <List.Item className="super-adversarial__thread-item">
                  <Card
                    size="small"
                    hoverable
                    className={selected ? 'super-adversarial__thread-card is-selected' : 'super-adversarial__thread-card'}
                    onClick={() => onSelectRun(run)}
                  >
                    <Space className="super-adversarial__between" align="start">
                      <Space size={6} wrap>
                        {run.thread_pinned ? (
                          <Tag color="gold">
                            <PushpinFilled /> {t('chat.adversarialPinned')}
                          </Tag>
                        ) : null}
                        <Tag color={adversarialStatusColor(run.status)}>
                          {t(`chat.adversarialStatus.${run.status}`, run.status)}
                        </Tag>
                        <Tag>{t('chat.adversarialThreadIterations', { count: thread.count })}</Tag>
                      </Space>
                      <Space size={2} onClick={(event) => event.stopPropagation()}>
                        <Tooltip title={run.thread_pinned ? t('common.unpin') : t('common.pin')}>
                          <Button
                            type="text"
                            size="small"
                            icon={run.thread_pinned ? <PushpinFilled /> : <PushpinOutlined />}
                            loading={pinningThreadId === thread.threadId}
                            onClick={() => onTogglePin(thread)}
                          />
                        </Tooltip>
                        <Tooltip title={t('common.rename')}>
                          <Button
                            type="text"
                            size="small"
                            icon={<EditOutlined />}
                            loading={renamingThreadId === thread.threadId}
                            onClick={() => onRename(thread)}
                          />
                        </Tooltip>
                        <Popconfirm
                          title={t('chat.adversarialDeleteConfirm')}
                          okText={t('common.confirm')}
                          cancelText={t('common.cancel')}
                          onConfirm={() => onDelete(thread)}
                        >
                          <Tooltip title={t('common.delete')}>
                            <Button
                              type="text"
                              size="small"
                              danger
                              icon={<DeleteOutlined />}
                              loading={deletingThreadId === thread.threadId}
                              onClick={(event) => event.stopPropagation()}
                            />
                          </Tooltip>
                        </Popconfirm>
                      </Space>
                    </Space>
                    <Paragraph ellipsis={{ rows: 2 }} className="super-adversarial__thread-title">
                      {getThreadDisplayTitle(run)}
                    </Paragraph>
                    <Text type="secondary" className="super-adversarial__muted">
                      {run.status === 'completed' && run.winner_model
                        ? t('chat.adversarialWinnerWithModel', { model: run.winner_model })
                        : t('chat.adversarialCurrentRound', {
                            current: run.current_round,
                          })}
                    </Text>
                  </Card>
                </List.Item>
              );
            }}
          />
          <div className="super-adversarial__list-footer">
            {fetchingNextPage ? (
              <Text type="secondary">{t('common.loading')}</Text>
            ) : hasNextPage ? (
              <Text type="secondary">{t('chat.adversarialLoadMore')}</Text>
            ) : threads.length > 0 ? (
              <Text type="secondary">{t('chat.adversarialNoMoreThreads', { count: threads.length, total })}</Text>
            ) : null}
          </div>
        </div>
      )}
    </aside>
  );
}
