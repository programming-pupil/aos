// ── Chat page — general AI dialogue for all users ─────────────────────────────────
//
// Wraps ChatCore with Chat-specific layout:
//   • MCP/Skill config tags in top bar
//   • source='chat' sessions
//
// All shared chat logic (streaming, sessions, tools, slash commands) lives in
// ChatCore — this page is purely presentational.

import { ChatCore } from '@/components/chat/ChatCore';
import { apiKeysApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { Alert, Select, Space, Tooltip, Typography } from 'antd';
import { useQuery } from '@tanstack/react-query';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

const { Text } = Typography;

function scenarioAppliesToChat(scenarios?: string[] | null): boolean {
  // Legacy keys may still have NULL/[] scenarios from older versions. Keep them
  // usable, while all new/edited keys are required to select explicit scenarios.
  return !scenarios || scenarios.length === 0 || scenarios.includes('chat');
}

function isChatModelKey(key: {
  enabled?: boolean;
  model_type?: string;
  model?: string | null;
  scenarios?: string[] | null;
}): boolean {
  return Boolean(
    key.enabled &&
      key.model_type === 'chat' &&
      key.model?.trim() &&
      scenarioAppliesToChat(key.scenarios)
  );
}

export default function Chat() {
  const { t } = useTranslation();
  const [selectedModel, setSelectedModel] = useState<string | undefined>();

  const apiKeysQ = useQuery({
    queryKey: queryKeys.apiKeys.list(),
    queryFn: () => apiKeysApi.list(),
  });

  const chatModelOptions = useMemo(() => {
    const seen = new Set<string>();
    return (apiKeysQ.data?.keys ?? [])
      .filter((key) => isChatModelKey(key) && key.runtime_available !== false)
      .sort((a, b) => (a.priority ?? 0) - (b.priority ?? 0))
      .flatMap((key) => {
        const model = key.model?.trim();
        if (!model || seen.has(model)) return [];
        seen.add(model);
        return [
          {
            value: model,
            label: `${model} · ${key.provider}`,
          },
        ];
      });
  }, [apiKeysQ.data?.keys]);

  const unusableChatKeyCount = useMemo(
    () =>
      (apiKeysQ.data?.keys ?? []).filter(
        (key) => isChatModelKey(key) && key.runtime_available === false
      ).length,
    [apiKeysQ.data?.keys]
  );

  useEffect(() => {
    if (chatModelOptions.length === 0) {
      setSelectedModel(undefined);
      return;
    }
    if (!selectedModel || !chatModelOptions.some((option) => option.value === selectedModel)) {
      setSelectedModel(chatModelOptions[0].value);
    }
  }, [chatModelOptions, selectedModel]);

  const modelSelector = (
    <Space size={8} wrap>
      <Text type="secondary" style={{ fontSize: 12 }}>
        {t('common.model')}
      </Text>
      <Tooltip title={t('chat.modelSelectorHelp')}>
        <Select
          size="small"
          style={{ minWidth: 260 }}
          loading={apiKeysQ.isLoading}
          disabled={apiKeysQ.isLoading || chatModelOptions.length === 0}
          value={selectedModel}
          placeholder={t('chat.selectModel')}
          options={chatModelOptions}
          onChange={setSelectedModel}
        />
      </Tooltip>
      {!apiKeysQ.isLoading && chatModelOptions.length === 0 ? (
        <Alert
          type="warning"
          showIcon
          message={
            unusableChatKeyCount > 0
              ? t('chat.noUsableChatModelConfigured')
              : t('chat.noChatModelConfigured')
          }
          style={{ padding: '2px 8px', fontSize: 12 }}
        />
      ) : null}
    </Space>
  );

  return (
    <ChatCore
      sessionSource="chat"
      selectedModel={selectedModel}
      emptySessionText={t('chat.noSession')}
      noSessionPlaceholder={{
        title: t('chat.welcomeTitle'),
        description: t('chat.noMessage'),
        emoji: '💬',
      }}
      showConfigTags={true}
      topBarExtra={modelSelector}
      inputPlaceholder={t('chat.inputPlaceholder')}
    />
  );
}
