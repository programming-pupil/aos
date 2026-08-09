// ── Context_Status panel (Super_Assistant) ──────────────────────────────────────
//
// Makes the assistant's memory state perceivable in the unified session shell
// (Req 4.8 / 8.5). It surfaces four signals for the active session:
//   - contextUsagePercent  → how full the context window is
//   - compactionCount      → how many times the session has been compacted
//   - whether compacted     → derived from compactionCount > 0
//   - remembered (已记忆) items → count of Unified_Memory entries for the session
//
// It reuses the existing `AgentContextStatus` type and `agentApi` endpoints — no
// parallel status type is introduced. The compact top-bar form keeps the shell
// capability-agnostic while still exposing memory state at a glance.

import { agentApi } from '@/api/agent';
import { queryKeys } from '@/api/queryKeys';
import { useQuery } from '@tanstack/react-query';
import { Space, Tag, Tooltip, Typography } from 'antd';
import { useTranslation } from 'react-i18next';

const { Text } = Typography;

/** Clamp a raw usage percentage into the displayable 0..100 range. */
function clampPercent(value: number | null | undefined): number {
  if (typeof value !== 'number' || Number.isNaN(value)) return 0;
  return Math.min(100, Math.max(0, value));
}

/** Color the usage tag by pressure: warning near the compaction threshold. */
function usageColor(percent: number): string | undefined {
  if (percent >= 90) return 'error';
  if (percent >= 80) return 'warning';
  return 'processing';
}

export interface ContextStatusPanelProps {
  /** Active session id; when null the panel renders nothing. */
  sessionId: string | null;
  /** Session source drives which memory app scope is counted. */
  sessionSource: 'chat' | 'agent' | 'pm';
}

/**
 * Compact Context_Status panel for the Super_Assistant top bar. Fetches the
 * session's {@link AgentContextStatus} and its Unified_Memory items, then renders
 * usage / compaction / remembered-count signals. Renders nothing without an
 * active session (nothing meaningful to show yet).
 */
export function ContextStatusPanel({ sessionId, sessionSource }: ContextStatusPanelProps) {
  const { t } = useTranslation();

  const { data: contextStatus, isFetching: contextFetching } = useQuery({
    queryKey: [...queryKeys.agentSessions.detail(sessionId ?? 'none'), 'context-status'],
    queryFn: () => agentApi.getSessionContextStatus(sessionId!),
    enabled: !!sessionId,
    staleTime: 20_000,
  });

  const memoryApp = sessionSource === 'pm' ? 'pm' : 'chat';
  const { data: memories } = useQuery({
    queryKey: [...queryKeys.agentSessions.detail(sessionId ?? 'none'), 'memory-count', memoryApp],
    queryFn: () =>
      agentApi.listUnifiedMemories({
        app: memoryApp,
        sessionId: sessionId ?? undefined,
        includeLegacy: sessionSource === 'pm',
      }),
    enabled: !!sessionId,
    staleTime: 20_000,
  });

  if (!sessionId) return null;

  if (!contextStatus) {
    return (
      <Text type="secondary" style={{ fontSize: 12 }}>
        {contextFetching
          ? t('superAssistant.contextLoading', 'Loading memory status…')
          : t('superAssistant.contextUnavailable', 'Memory status unavailable')}
      </Text>
    );
  }

  const percent = clampPercent(contextStatus.contextUsagePercent);
  const compactionCount = Math.max(0, contextStatus.compactionCount ?? 0);
  const isCompacted = compactionCount > 0;
  const rememberedCount = memories?.items?.length ?? 0;

  return (
    <Space size={[6, 6]} wrap data-testid="context-status-panel">
      <Tooltip
        title={t(
          'superAssistant.contextUsageTooltip',
          'Estimated context: {{used}} / {{limit}} tokens',
          {
            used: contextStatus.estimatedTokens.toLocaleString(),
            limit: contextStatus.effectiveContextLimit.toLocaleString(),
          },
        )}
      >
        <Tag color={usageColor(percent)} style={{ marginRight: 0 }} data-testid="context-usage-tag">
          {t('superAssistant.contextUsagePercent', 'Context {{percent}}%', {
            percent: Math.round(percent),
          })}
        </Tag>
      </Tooltip>

      <Tooltip
        title={
          isCompacted
            ? t('superAssistant.compactedTooltip', 'This session has been compacted {{count}} time(s)', {
                count: compactionCount,
              })
            : t('superAssistant.notCompactedTooltip', 'This session has not been compacted yet')
        }
      >
        <Tag
          color={isCompacted ? 'green' : 'default'}
          style={{ marginRight: 0 }}
          data-testid="context-compaction-tag"
        >
          {isCompacted
            ? t('superAssistant.compactedCount', 'Compacted ×{{count}}', { count: compactionCount })
            : t('superAssistant.notCompacted', 'Not compacted')}
        </Tag>
      </Tooltip>

      <Tooltip
        title={t('superAssistant.rememberedTooltip', 'Key info remembered for this session')}
      >
        <Tag style={{ marginRight: 0 }} data-testid="context-remembered-tag">
          {t('superAssistant.rememberedCount', '{{count}} remembered', { count: rememberedCount })}
        </Tag>
      </Tooltip>
    </Space>
  );
}

export default ContextStatusPanel;
