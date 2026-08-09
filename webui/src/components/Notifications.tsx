import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Badge,
  Button,
  Drawer,
  List,
  Tag,
  Typography,
  Empty,
  Popconfirm,
  Space,
  Tooltip,
} from 'antd';
import {
  BellOutlined,
  CheckOutlined,
  DeleteOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import { notificationsApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { usePermissions } from '@/store/permissions';

dayjs.extend(relativeTime);

const { Text } = Typography;

const LEVEL_COLORS: Record<string, string> = {
  info: 'blue',
  warning: 'orange',
  error: 'red',
  success: 'green',
};

interface NotificationBellProps {
  onClick: () => void;
  unreadCount: number;
}

export function NotificationBell({ onClick, unreadCount }: NotificationBellProps) {
  const { t } = useTranslation();
  return (
    <Tooltip title={unreadCount > 0 ? t('notifications.unreadCount', { count: unreadCount }) : undefined}>
      <Badge count={unreadCount > 0 ? unreadCount : 0} size="small" offset={[-2, 2]}>
        <Button
          type="text"
          size="small"
          icon={<BellOutlined style={{ fontSize: 16 }} />}
          onClick={onClick}
          style={{ color: 'var(--text-secondary)' }}
        />
      </Badge>
    </Tooltip>
  );
}

interface NotificationsDrawerProps {
  open: boolean;
  onClose: () => void;
}

export function NotificationsDrawer({ open, onClose }: NotificationsDrawerProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [page, setPage] = useState(1);
  const pageSize = 20;

  const { data, isLoading } = useQuery({
    queryKey: queryKeys.notifications.list({ page, per_page: pageSize }),
    queryFn: () => notificationsApi.list({ page, per_page: pageSize }),
    refetchInterval: 30_000,
    enabled: open,
  });

  useEffect(() => {
    const maxPage = Math.max(1, Math.ceil((data?.total ?? 0) / pageSize));
    if (page > maxPage) setPage(maxPage);
  }, [data?.total, page, pageSize]);

  const markReadMutation = useMutation({
    mutationFn: ({ id, read }: { id: string; read: boolean }) =>
      notificationsApi.markRead(id, read),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.notifications.list() }),
  });

  const markAllReadMutation = useMutation({
    mutationFn: () => notificationsApi.markAllRead(),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.notifications.list() }),
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => notificationsApi.delete(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.notifications.list() }),
  });

  return (
    <Drawer
      title={
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <span>{t('notifications.title')}</span>
          {data?.unread_count ? (
            <Button
              type="link"
              size="small"
              icon={<CheckOutlined />}
              onClick={() => markAllReadMutation.mutate()}
              loading={markAllReadMutation.isPending}
            >
              {t('notifications.markAllRead')}
            </Button>
          ) : null}
        </div>
      }
      placement="right"
      onClose={onClose}
      open={open}
      width={400}
      styles={{ body: { padding: 0 } }}
    >
      {isLoading ? null : !data?.notifications.length ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={
            <span>
              <Text type="secondary">{t('notifications.empty.title')}</Text>
            </span>
          }
          style={{ padding: '48px 0' }}
        />
      ) : (
        <List
          dataSource={data?.notifications ?? []}
          pagination={{
            current: page,
            pageSize,
            total: data?.total ?? 0,
            hideOnSinglePage: true,
            showSizeChanger: false,
            onChange: setPage,
          }}
          renderItem={(item) => (
            <List.Item
              key={item.id}
              style={{
                padding: '12px 20px',
                background: item.read ? 'transparent' : 'var(--bg-surface)',
                borderLeft: item.read ? 'none' : '3px solid var(--accent-ai)',
                transition: 'background 0.2s',
              }}
              onClick={() => {
                if (!item.read) markReadMutation.mutate({ id: item.id, read: true });
              }}
            >
              <div style={{ width: '100%' }}>
                <div style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 2 }}>
                      <Text strong style={{ fontSize: 14, color: item.read ? 'var(--text-secondary)' : 'var(--text-primary)' }}>
                        {item.title}
                      </Text>
                      <Tag color={LEVEL_COLORS[item.level] ?? 'default'} style={{ margin: 0, fontSize: 10 }}>
                        {t(`notifications.level.${item.level}`)}
                      </Tag>
                    </div>
                    <Text type="secondary" style={{ fontSize: 13, display: 'block', marginBottom: 4 }}>
                      {item.body}
                    </Text>
                    <Text type="secondary" style={{ fontSize: 11 }}>
                      {dayjs(item.created_at).fromNow()}
                    </Text>
                  </div>
                  <Space size={4} onClick={(e) => e.stopPropagation()}>
                    {!item.read && (
                      <Tooltip title={t('notifications.markRead')}>
                        <Button
                          type="text"
                          size="small"
                          icon={<CheckOutlined />}
                          onClick={() => markReadMutation.mutate({ id: item.id, read: true })}
                        />
                      </Tooltip>
                    )}
                    <Popconfirm
                      title={t('notifications.delete')}
                      onConfirm={() => deleteMutation.mutate(item.id)}
                    >
                      <Tooltip title={t('notifications.delete')}>
                        <Button type="text" size="small" danger icon={<DeleteOutlined />} />
                      </Tooltip>
                    </Popconfirm>
                  </Space>
                </div>
              </div>
            </List.Item>
          )}
        />
      )}
    </Drawer>
  );
}
