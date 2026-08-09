import { Avatar, Button, Card, Space, Tag, Typography } from 'antd';
import { RobotOutlined, UserOutlined } from '@ant-design/icons';
import type { TFunction } from 'i18next';
import { Markdown } from '@/components/chat';
import type { TimelineMessage } from './types';
import { messageAccent } from './utils';

const { Paragraph, Text, Title } = Typography;

type Props = {
  t: TFunction;
  composeNewRun: boolean;
  timeline: TimelineMessage[];
  hasOlder: boolean;
  loadingOlder: boolean;
  reachedOldest: boolean;
  onLoadOlder: () => void;
};

export function DebateTimeline({
  t,
  composeNewRun,
  timeline,
  hasOlder,
  loadingOlder,
  reachedOldest,
  onLoadOlder,
}: Props) {
  if (composeNewRun || timeline.length === 0) {
    return (
      <div className="super-adversarial__empty-state">
        <Title level={3}>
          {composeNewRun ? t('chat.adversarialNewTitle') : t('chat.adversarialWelcomeTitle')}
        </Title>
        <Paragraph type="secondary">
          {composeNewRun ? t('chat.adversarialNewDesc') : t('chat.adversarialWelcomeDesc')}
        </Paragraph>
      </div>
    );
  }

  return (
    <Space direction="vertical" size={14} className="super-adversarial__timeline">
      {hasOlder ? (
        <div className="super-adversarial__load-older">
          <Button size="small" loading={loadingOlder} onClick={onLoadOlder}>
            {loadingOlder ? t('chat.adversarialLoadingOlder') : t('chat.adversarialLoadOlder')}
          </Button>
        </div>
      ) : reachedOldest ? (
        <div className="super-adversarial__load-older">
          <Text type="secondary">{t('chat.adversarialNoOlder')}</Text>
        </div>
      ) : null}
      {timeline.map((item) => {
        const accent = messageAccent(item.role, item.model);
        const isUser = item.role === 'user';
        return (
          <div
            key={item.id}
            className={isUser ? 'super-adversarial__message is-user' : 'super-adversarial__message'}
          >
            <Avatar
              icon={isUser ? <UserOutlined /> : <RobotOutlined />}
              style={{ background: accent, flexShrink: 0 }}
            />
            <Card
              className={`super-adversarial__bubble is-${item.role}`}
              size="small"
              style={{ borderColor: `${accent}55` }}
            >
              <Space direction="vertical" size={8} className="super-adversarial__fill">
                <Space wrap>
                  <Text strong style={{ color: accent }}>
                    {item.title}
                  </Text>
                  {item.subtitle ? <Tag>{item.subtitle}</Tag> : null}
                  {item.error ? <Tag color="red">{t('chat.adversarialModelError')}</Tag> : null}
                </Space>
                <div className="super-adversarial__bubble-content">
                  {isUser ? (
                    <span>{item.content}</span>
                  ) : (
                    <Markdown relaxed suppressHr>{item.content}</Markdown>
                  )}
                  {item.typing ? <span className="super-adversarial__typing-cursor">▍</span> : null}
                </div>
              </Space>
            </Card>
          </div>
        );
      })}
    </Space>
  );
}
