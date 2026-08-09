import {
  Modal, Progress, Typography, Space, Tag, Alert, Button, Descriptions,
} from 'antd';
import { SyncOutlined, CheckCircleFilled, CloseCircleFilled, WarningFilled } from '@ant-design/icons';
import { useQuery } from '@tanstack/react-query';
import { nl2sqlApi } from '@/api';
import { ApiError } from '@/api/errors';
import { useTranslation } from 'react-i18next';
import type { RefreshTaskStatus } from '@/types';

const { Text } = Typography;

export interface DiscoverProgressModalProps {
  /** The data source id that is being created/updated */
  dataSourceId: string;
  /** The refresh task id returned by the discover endpoint */
  taskId: string;
  /** Called when the progress modal finishes (success or error) */
  onDone: (success: boolean, failedTables?: Array<{ table: string; error: string }>) => void;
  /** Called when user wants to view the schema drawer */
  onViewSchema: (dataSourceId: string) => void;
}

/** Maps a raw backend task status to the UI phase */
function getDiscoverPhase(
  status: string,
  progress: number,
): 'fetching' | 'indexing' | 'complete' | 'failed' {
  if (status === 'failed') return 'failed';
  if (status === 'completed') return 'complete';
  // During 'running', low progress means still fetching DB schema; >40% means indexing
  if (progress < 40) return 'fetching';
  return 'indexing';
}

export function DiscoverProgressModal({
  dataSourceId,
  taskId,
  onDone,
  onViewSchema,
}: DiscoverProgressModalProps) {
  const { t } = useTranslation();

  // Poll task status
  const { data: taskStatus, error: taskError } = useQuery<RefreshTaskStatus, ApiError>({
    queryKey: ['nl2sql', 'refresh-task', taskId],
    queryFn: () => nl2sqlApi.getRefreshTaskStatus(taskId),
    refetchInterval: 2000,
    retry: (failureCount, err) => {
      // Don't retry on 404 — task might not exist yet
      if (err?.message?.includes('404')) return false;
      return failureCount < 3;
    },
  });

  const phase = taskStatus
    ? getDiscoverPhase(taskStatus.status, taskStatus.progress)
    : 'fetching';

  const isFinished = phase === 'complete' || phase === 'failed';
  const failedTables = taskStatus?.failed_tables ?? null;

  const handleViewSchema = () => {
    onViewSchema(dataSourceId);
  };

  const progressColor = phase === 'failed'
    ? '#ff4d4f'
    : phase === 'complete'
      ? '#52c41a'
      : '#7c3aed';

  return (
    <Modal
      open
      title={
        <Space>
          <SyncOutlined
            spin={!isFinished}
            style={{ color: isFinished ? (phase === 'complete' ? '#52c41a' : '#ff4d4f') : '#7c3aed' }}
          />
          {t('datasources.discoverProgressTitle')}
        </Space>
      }
      footer={
        isFinished ? (
          <Space>
            {phase === 'complete' && (
              <Button
                type="primary"
                icon={<CheckCircleFilled />}
                onClick={handleViewSchema}
              >
                {t('datasources.viewSchema')}
              </Button>
            )}
            <Button onClick={() => onDone(phase === 'complete', failedTables?.map(f => ({ table: f.table, error: f.error })))}>
              {t('common.close')}
            </Button>
          </Space>
        ) : (
          <div style={{ display: 'flex', justifyContent: 'flex-end', paddingTop: 8 }}>
            <Button onClick={() => onDone(false)}>
              {t('datasources.discoverRunInBackground')}
            </Button>
          </div>
        )
      }
      maskClosable={false}
      closable={false}
      width={520}
    >
      <Space direction="vertical" size={16} style={{ width: '100%' }}>
        {/* Phase descriptions */}
        <Descriptions column={1} size="small" bordered>
          <Descriptions.Item
            label={t('datasources.discoverPhaseStatus')}
          >
            <Tag
              color={
                phase === 'complete' ? 'success'
                  : phase === 'failed' ? 'error'
                    : phase === 'indexing' ? 'processing'
                      : 'default'
              }
              icon={
                phase === 'complete' ? <CheckCircleFilled />
                  : phase === 'failed' ? <CloseCircleFilled />
                    : phase === 'indexing' ? <SyncOutlined />
                      : undefined
              }
            >
              {phase === 'fetching' && t('datasources.discoverPhaseFetching')}
              {phase === 'indexing' && t('datasources.discoverPhaseIndexing')}
              {phase === 'complete' && t('datasources.discoverPhaseComplete')}
              {phase === 'failed' && t('datasources.discoverPhaseFailed')}
            </Tag>
          </Descriptions.Item>
          <Descriptions.Item label={t('datasources.discoverTablesProcessed')}>
            {taskStatus?.processed_tables ?? 0}
          </Descriptions.Item>
          {taskStatus?.error_message && (
            <Descriptions.Item label={t('datasources.discoverError')}>
              <Text type="danger" style={{ fontSize: 12 }}>{taskStatus.error_message}</Text>
            </Descriptions.Item>
          )}
        </Descriptions>

        {/* Progress bar */}
        <div>
          <div style={{ marginBottom: 6, display: 'flex', justifyContent: 'space-between' }}>
            <Text type="secondary" style={{ fontSize: 12 }}>
              {phase === 'fetching' && t('datasources.discoverProgressFetching')}
              {phase === 'indexing' && t('datasources.discoverProgressIndexing')}
              {phase === 'complete' && t('datasources.discoverProgressComplete')}
              {phase === 'failed' && t('datasources.discoverProgressFailed')}
            </Text>
            <Text style={{ fontSize: 12, fontWeight: 500 }}>{taskStatus?.progress ?? 0}%</Text>
          </div>
          <Progress
            percent={taskStatus?.progress ?? 0}
            showInfo={false}
            strokeColor={progressColor}
            trailColor="var(--bg-secondary)"
            size="small"
          />
        </div>

        {/* Partial failure warning */}
        {failedTables && failedTables.length > 0 && (
          <Alert
            type="warning"
            showIcon
            icon={<WarningFilled />}
            message={t('datasources.discoverPartialFailure', { count: failedTables.length })}
            description={
              <ul style={{ margin: '8px 0 0', paddingLeft: 16, fontSize: 12 }}>
                {failedTables.slice(0, 5).map(f => (
                  <li key={f.table}>
                    <Text code style={{ fontSize: 11 }}>{f.table}</Text>
                    {' — '}
                    <Text type="secondary" style={{ fontSize: 11 }}>{f.error}</Text>
                  </li>
                ))}
                {failedTables.length > 5 && (
                  <li>
                    <Text type="secondary" style={{ fontSize: 11 }}>
                      ...{failedTables.length - 5} more
                    </Text>
                  </li>
                )}
              </ul>
            }
          />
        )}

        {/* Backend error */}
        {taskError && (
          <Alert
            type="error"
            showIcon
            message={t('datasources.discoverPollingError')}
            description={taskError.message}
          />
        )}

        {/* Loading state */}
        {!isFinished && (
          <Text type="secondary" style={{ fontSize: 11, textAlign: 'center', display: 'block' }}>
            {t('datasources.discoverBackgroundHint')}
          </Text>
        )}
        {isFinished && phase === 'complete' && (
          <Text type="secondary" style={{ fontSize: 11, textAlign: 'center', display: 'block' }}>
            {t('datasources.discoverCompleteHint')}
          </Text>
        )}
      </Space>
    </Modal>
  );
}
