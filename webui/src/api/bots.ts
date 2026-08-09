import { client } from './client';
import type {
  BotAgentChannelInfo,
  BotAgentInfo,
  BotExternalIdentityInfo,
  BotMessageLogInfo,
} from '@/types';

export type BotCapabilityBindingInput = string | {
  capability_key?: string;
  trigger_prefixes?: string[];
  require_mention?: boolean;
  fallback_when_no_prefix?: boolean;
  model?: string;
  models?: string[];
  maxRounds?: number;
  repositoryId?: string;
  agentProfileId?: string;
  workflowId?: string;
  contextDepth?: string;
  shouldDeepScan?: boolean;
  dataSourceId?: string;
  allowExecuteSql?: boolean;
  deepAnalysis?: boolean;
  watchdogScope?: string;
  allowActions?: string[];
  executionMode?: string;
  syncTimeoutMs?: number;
  ackTimeoutMs?: number;
  confidenceThreshold?: number;
};

export const botAgentsApi = {
  list: (params?: { page?: number; per_page?: number }) =>
    client.get<{ items: BotAgentInfo[]; total: number }>('/bot-agents', { params }).then((r) => r.data),

  create: (data: {
    name: string;
    description?: string;
    enabled?: boolean;
    default_capability?: string;
    persona_prompt?: string;
    capabilities?: BotCapabilityBindingInput[];
  }) => client.post<BotAgentInfo>('/bot-agents', data).then((r) => r.data),

  update: (id: string, data: {
    name?: string;
    description?: string;
    enabled?: boolean;
    default_capability?: string;
    persona_prompt?: string;
    capabilities?: BotCapabilityBindingInput[];
  }) => client.patch<BotAgentInfo>(`/bot-agents/${id}`, data).then((r) => r.data),

  delete: (id: string) => client.delete(`/bot-agents/${id}`).then((r) => r.data),

  listChannels: (params?: { agent_id?: string; page?: number; per_page?: number }) =>
    client.get<{ items: BotAgentChannelInfo[]; total: number }>('/bot-agents/channels', { params }).then((r) => r.data),

  createChannel: (data: {
    agent_id: string;
    platform: string;
    name: string;
    enabled?: boolean;
    inbound_mode?: string;
    inbound_secret?: string;
    outbound_webhook_url?: string;
    outbound_token?: string;
    signing_secret?: string;
    outbound_signing_secret?: string;
    config_json?: Record<string, unknown>;
  }) => client.post<BotAgentChannelInfo>('/bot-agents/channels', data).then((r) => r.data),

  updateChannel: (id: string, data: {
    platform?: string;
    name?: string;
    enabled?: boolean;
    inbound_mode?: string;
    inbound_secret?: string;
    outbound_webhook_url?: string;
    outbound_token?: string;
    signing_secret?: string;
    outbound_signing_secret?: string;
    config_json?: Record<string, unknown>;
  }) => client.patch<BotAgentChannelInfo>(`/bot-agents/channels/${id}`, data).then((r) => r.data),

  deleteChannel: (id: string) => client.delete(`/bot-agents/channels/${id}`).then((r) => r.data),

  testChannel: (id: string, data?: { title?: string; text?: string }) =>
    client.post<{ ok: boolean; status: string; log_id: string; provider_response?: Record<string, unknown> }>(
      `/bot-agents/channels/${id}/test`,
      data ?? {},
    ).then((r) => r.data),

  listLogs: (params?: { agent_id?: string; channel_id?: string; page?: number; per_page?: number }) =>
    client.get<{ items: BotMessageLogInfo[]; total: number }>('/bot-agents/message-logs', { params }).then((r) => r.data),

  listIdentities: () =>
    client.get<{ items: BotExternalIdentityInfo[] }>('/bot-identities').then((r) => r.data),

  createPairingCode: (platform?: string, channelId?: string) =>
    client
      .post<{ code: string; expiresInSeconds: number }>('/bot-identities/pairing-codes', {
        platform,
        channelId,
      })
      .then((r) => r.data),

  revokeIdentity: (identityId: string) =>
    client.delete(`/bot-identities/${encodeURIComponent(identityId)}`).then((r) => r.data),
};
