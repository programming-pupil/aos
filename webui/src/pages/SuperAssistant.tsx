// ── Super Assistant (AOS Copilot) — unified single-session shell ─────────────────
//
// Merges the former AI Chat (`ai_chat`), Ops Assistant (`pm_assistant`) and Data
// Attribution (`dataAttribution`) entries into ONE entry point. There is a single
// input box that accepts general questions, live facts, business analysis, SQL,
// attachments, and code when needed — the user never picks a sub-scenario. Intent
// routing (ai_chat / pm_assistant / nl2sql / super_adversarial) is performed
// server-side by the AOS_Router; the frontend just hosts the unified session shell.
//
// This reuses the shared ChatCore component (the `sessionSource="pm"` deep-dialogue
// variant, which carries the richest capability set: streaming, stages, evidence,
// sources, attachments, memory and session management). No parallel chat UI is
// built here — all shared logic lives in ChatCore.

import { ChatCore } from '@/components/chat/ChatCore';
import { ContextStatusPanel } from '@/components/chat/ContextStatusPanel';
import { Space, Tag, Typography } from 'antd';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

const { Text } = Typography;

export default function SuperAssistant() {
  const { t } = useTranslation();
  // Track the active session so the Context_Status panel can surface memory
  // state (usage / compaction / remembered items) for the current session.
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);

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
          // Single unified session source. Backend AOS_Router dispatches each
          // message to the best capability; the shell stays capability-agnostic.
          sessionSource="pm"
          // Send/receive through the unified Super_Assistant endpoint
          // (`POST /super-assistant/messages`) rather than the local per-session
          // streaming path — server-side intent routing picks the capability
          // (Req 1.1 / 1.6 / 8.2).
          superAssistantEndpoint
          emptySessionText={t('superAssistant.empty', 'Ask anything to get started')}
          noSessionPlaceholder={{
            title: t('superAssistant.welcomeTitle', 'Super Assistant'),
            description: t(
              'superAssistant.welcomeDescription',
              'One entry for everything — chat, live lookup, deep research, data analysis, SQL, attachments and code. Just ask; the assistant routes your question automatically.',
            ),
            emoji: '🤖',
          }}
          showConfigTags={true}
          sidebarWidth={240}
          onActiveSessionChange={setActiveSessionId}
          messageListProps={{
            style: {
              background: 'var(--bg-surface)',
            },
          }}
          // A single input box: no sub-scenario picker. Placeholder makes clear the
          // one box accepts general questions, live facts, analysis, SQL, attachments
          // and code (Req 1.6).
          inputPlaceholder={t(
            'superAssistant.inputPlaceholder',
            'Ask anything — natural language, live facts, business analysis, SQL, attachments or code. No need to pick a scenario.',
          )}
          topBarExtra={
            <Space size={[6, 6]} wrap>
              <Tag color="processing" style={{ marginRight: 0 }}>
                {t('superAssistant.tagUnified', 'Unified entry')}
              </Tag>
              <Tag style={{ marginRight: 0 }}>
                {t('superAssistant.tagAutoRoute', 'Auto intent routing')}
              </Tag>
              <Tag style={{ marginRight: 0 }}>
                {t('superAssistant.tagMemory', 'Context memory')}
              </Tag>
              {/* Context_Status: usage / compaction / remembered items (Req 4.8, 8.5) */}
              <ContextStatusPanel sessionId={activeSessionId} sessionSource="pm" />
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
                <span>{t('superAssistant.hintChat', 'General chat, live lookup and deep research')}</span>
                <span>{t('superAssistant.hintSql', 'Data exploration, SQL knowledge and attribution')}</span>
                <span>{t('superAssistant.hintAttach', 'Drop attachments — they are parsed and remembered')}</span>
              </Space>
              <Text style={{ fontSize: 12, color: 'var(--text-muted)', minWidth: 0 }}>
                {t('superAssistant.hintEvidence', 'Deep answers expose stages, evidence and source links.')}
              </Text>
            </div>
          }
        />
      </div>
    </div>
  );
}
