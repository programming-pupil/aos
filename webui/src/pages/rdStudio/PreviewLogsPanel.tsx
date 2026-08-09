import { Alert, Empty, List, Space, Tag, Typography } from 'antd';
import { BugOutlined, ConsoleSqlOutlined } from '@ant-design/icons';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';

const { Text } = Typography;

function severityColor(severity?: string) {
  if (severity === 'error') return 'red';
  if (severity === 'warn' || severity === 'warning') return 'gold';
  if (severity === 'info') return 'blue';
  return 'default';
}

export function PreviewLogsPanel({
  sessionId,
}: {
  sessionId?: string | null;
}) {
  const { t } = useTranslation();
  const logsQuery = useQuery({
    queryKey: queryKeys.rd.previewLogs(sessionId ?? undefined),
    queryFn: () => rdApi.previewSessionLogs(sessionId!),
    enabled: !!sessionId,
    refetchInterval: (query) => {
      const status = query.state.data?.session.status;
      return status && ['running', 'starting'].includes(status) ? 3000 : false;
    },
  });

  if (!sessionId) {
    return (
      <Empty
        image={Empty.PRESENTED_IMAGE_SIMPLE}
        description={<span style={{ color: '#94a3b8' }}>{t('rd.previewLogsEmpty', '启动预览后会显示 console、network 和 runtime 日志')}</span>}
      />
    );
  }

  if (logsQuery.error) {
    return <Alert type="error" showIcon message={t('rd.previewLogsFailed', '预览日志加载失败')} description={(logsQuery.error as Error).message} />;
  }

  const data = logsQuery.data;
  const events = data?.events ?? [];
  return (
    <div className="rd-preview-logs-panel">
      {data?.session.logsPreview ? (
        <pre className="rd-preview-logs-output">{data.session.logsPreview}</pre>
      ) : null}
      {events.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={<span style={{ color: '#94a3b8' }}>{t('rd.previewEventsEmpty', '暂无预览事件')}</span>}
        />
      ) : (
        <List
          size="small"
          dataSource={events}
          renderItem={(event) => (
            <List.Item className="rd-preview-event-item">
              <Space direction="vertical" size={3} style={{ minWidth: 0, width: '100%' }}>
                <Space size={6} wrap>
                  {event.eventType.includes('console') ? <ConsoleSqlOutlined /> : <BugOutlined />}
                  <Tag color={severityColor(event.severity)}>{event.severity}</Tag>
                  <Text className="rd-preview-event-type">{event.eventType}</Text>
                </Space>
                <Text className="rd-preview-event-message">{event.message}</Text>
              </Space>
            </List.Item>
          )}
        />
      )}
    </div>
  );
}
