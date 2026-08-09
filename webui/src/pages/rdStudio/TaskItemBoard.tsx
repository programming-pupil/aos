import { Button, Empty, List, Space, Tag, Typography } from 'antd';
import { PlayCircleOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { RdSpecTaskItem } from '@/types';

const { Text } = Typography;

const STATUS_COLOR: Record<string, string> = {
  pending: 'default',
  running: 'processing',
  waiting_approval: 'warning',
  completed: 'success',
  failed: 'error',
  cancelled: 'default',
  skipped: 'default',
};

export function TaskItemBoard({
  items,
  loadingTaskId,
  canImplement,
  onImplement,
  onOpenTask,
}: {
  items: RdSpecTaskItem[];
  loadingTaskId?: string | null;
  canImplement: boolean;
  onImplement: (item: RdSpecTaskItem) => void;
  onOpenTask?: (taskId: string) => void;
}) {
  const { t } = useTranslation();
  if (items.length === 0) {
    return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.planNoTaskItems', '暂无任务项')} />;
  }

  return (
    <List
      className="rd-plan-task-board"
      dataSource={items}
      renderItem={(item, index) => (
        <List.Item>
          <Space direction="vertical" size={8} style={{ width: '100%', minWidth: 0 }}>
            <Space wrap style={{ width: '100%', justifyContent: 'space-between' }}>
              <Space size={6} wrap>
                <Tag>{index + 1}</Tag>
                <Tag color={item.priority === 'p0' ? 'red' : item.priority === 'p1' ? 'orange' : 'blue'}>
                  {item.priority?.toUpperCase?.() || 'P1'}
                </Tag>
                <Tag color={STATUS_COLOR[item.status] ?? 'default'}>
                  {t(`rd.planTaskStatuses.${item.status}`, item.status)}
                </Tag>
              </Space>
              <Space>
                {item.linkedRdTaskId ? (
                  <Button size="small" type="link" onClick={() => onOpenTask?.(item.linkedRdTaskId!)}>
                    {t('rd.openRdTask', '打开任务')}
                  </Button>
                ) : null}
                <Button
                  size="small"
                  icon={<PlayCircleOutlined />}
                  disabled={!canImplement}
                  loading={loadingTaskId === item.id}
                  onClick={() => onImplement(item)}
                >
                  {t('rd.implementTaskItem', '执行')}
                </Button>
              </Space>
            </Space>
            <Text strong className="rd-plan-task-title">{item.title}</Text>
            <Text type="secondary" className="rd-plan-task-description">{item.description}</Text>
            {item.acceptance?.length ? (
              <Space wrap size={[4, 4]}>
                {item.acceptance.slice(0, 4).map((acceptance) => (
                  <Tag key={acceptance}>{acceptance}</Tag>
                ))}
              </Space>
            ) : null}
          </Space>
        </List.Item>
      )}
    />
  );
}
