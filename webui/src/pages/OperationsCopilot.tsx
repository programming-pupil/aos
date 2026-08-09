import { ChatCore } from '@/components/chat/ChatCore';
import { Space, Tag, Typography } from 'antd';
import { useTranslation } from 'react-i18next';

const { Text } = Typography;

export default function OperationsCopilot() {
  const { t } = useTranslation();

  return (
    <div
      style={{
        display: 'flex',
        height: '100%',
        minHeight: 0,
        background: 'var(--bg-surface)',
      }}
    >
      <div style={{ flex: 1, minWidth: 0, minHeight: 0 }}>
        <ChatCore
          sessionSource="pm"
          emptySessionText={t('operations.empty', 'Start your first operations question')}
          noSessionPlaceholder={{
            title: t('operations.copilotTitle', 'Ops Copilot'),
            description: t(
              'operations.copilotSubtitle',
              'Evidence-first PM and operations assistant for market, growth, review, competitor, and strategy work.',
            ),
            emoji: '📈',
          }}
          showConfigTags={true}
          inputPlaceholder={t(
            'operations.copilotPlaceholder',
            'Example: For Indonesia GP reviews in the last 30 days, what are the top retention pain points? Include source URLs and priority.',
          )}
          sidebarWidth={280}
          messageListProps={{
            style: {
              background: 'var(--bg-surface)',
            },
          }}
          topBarExtra={
            <Space size={[6, 6]} wrap>
              <Tag color="processing" style={{ marginRight: 0 }}>
                {t('operations.copilotModeResearch', 'Evidence-first research')}
              </Tag>
              <Tag style={{ marginRight: 0 }}>
                {t('operations.copilotModeQueue', 'Queued turns')}
              </Tag>
              <Tag style={{ marginRight: 0 }}>
                {t('operations.copilotModeSources', 'Sources & stages')}
              </Tag>
            </Space>
          }
          inputHintBar={
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 12,
                flexWrap: 'wrap',
                fontSize: 12,
                color: 'var(--text-muted)',
                lineHeight: 1.5,
              }}
            >
              <Space size={[10, 4]} wrap style={{ minWidth: 0 }}>
                <span>{t('operations.copilotHintAsk', 'Ask market, growth, review, competitor, or strategy questions')}</span>
                <span>{t('operations.copilotHintQueue', 'Busy sessions queue new turns until the current task finishes')}</span>
                <span>{t('operations.copilotHintCancel', 'Cancel is cooperative; active external calls may take a few seconds to settle')}</span>
              </Space>
              <Text style={{ fontSize: 12, color: 'var(--text-muted)', minWidth: 0 }}>
                {t('operations.copilotHintEvidence', 'Every deep answer should expose stages, evidence, and source URLs.')}
              </Text>
            </div>
          }
        />
      </div>
    </div>
  );
}
