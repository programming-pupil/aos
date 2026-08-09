import { Button, Progress, Space, Tag, Tooltip, Typography } from 'antd';
import { CrownOutlined, FireOutlined, PauseCircleOutlined } from '@ant-design/icons';
import type { TFunction } from 'i18next';
import type { ChatAdversarialRun } from '@/types';
import { activeRunStatuses, adversarialStatusColor, getThreadDisplayTitle } from './utils';

const { Text, Title } = Typography;

type Props = {
  t: TFunction;
  activeRun: ChatAdversarialRun | null;
  composeNewRun: boolean;
  cancelling: boolean;
  onCancel: () => void;
};

export function RunHeader({
  t,
  activeRun,
  composeNewRun,
  cancelling,
  onCancel,
}: Props) {
  const progress = activeRun?.max_rounds
    ? Math.min(100, Math.max(0, Math.round((activeRun.current_round / activeRun.max_rounds) * 100)))
    : 0;
  const canCancel = activeRun ? activeRunStatuses(activeRun.status) : false;

  return (
    <header className="super-adversarial__header">
      <Space align="start" className="super-adversarial__between">
        <Space direction="vertical" size={6} className="super-adversarial__title-block">
          <Space>
            <FireOutlined className="super-adversarial__title-icon" />
            <Title level={3} className="super-adversarial__title">
              {composeNewRun
                ? t('chat.adversarialNewTitle')
                : activeRun
                  ? getThreadDisplayTitle(activeRun)
                  : t('chat.adversarialSelectRun')}
            </Title>
          </Space>
          {activeRun ? (
            <Space size={8} wrap>
              <Tag color={adversarialStatusColor(activeRun.status)}>
                {t(`chat.adversarialStatus.${activeRun.status}`, activeRun.status)}
              </Tag>
              <Text type="secondary">
                {t('chat.adversarialCurrentRound', {
                  current: activeRun.current_round,
                })}
              </Text>
              {activeRun.agent_task_id ? <Tag>AgentOps</Tag> : null}
              {activeRun.status === 'completed' && activeRun.winner_model ? (
                <Tag color="green">
                  <CrownOutlined /> {activeRun.winner_model}
                </Tag>
              ) : null}
              {activeRun.error_message ? <Tag color="red">{activeRun.error_message}</Tag> : null}
            </Space>
          ) : (
            <Text type="secondary">{t('chat.adversarialNewDesc')}</Text>
          )}
        </Space>
        {activeRun ? (
          <Space wrap className="super-adversarial__run-actions">
            <Button
              danger
              icon={<PauseCircleOutlined />}
              loading={cancelling}
              disabled={!canCancel || cancelling}
              onClick={onCancel}
            >
              {t('chat.adversarialStop')}
            </Button>
          </Space>
        ) : null}
      </Space>
      {activeRun ? (
        <Progress
          className="super-adversarial__progress"
          percent={activeRun.status === 'completed' ? 100 : progress}
          size="small"
          status={activeRun.status === 'failed' ? 'exception' : activeRun.status === 'completed' ? 'success' : 'active'}
          showInfo={false}
        />
      ) : null}
    </header>
  );
}
