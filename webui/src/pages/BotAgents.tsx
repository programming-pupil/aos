import { useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Card,
  Col,
  Collapse,
  Descriptions,
  Drawer,
  Form,
  Input,
  List,
  Modal,
  Popconfirm,
  Row,
  Select,
  Space,
  Switch,
  Table,
  Tabs,
  Tag,
  Typography,
  message,
} from 'antd';
import type { ColumnsType } from 'antd/es/table';
import {
  ApiOutlined,
  BookOutlined,
  DeleteOutlined,
  DisconnectOutlined,
  EditOutlined,
  ExportOutlined,
  LinkOutlined,
  PlusOutlined,
  QuestionCircleOutlined,
  RobotOutlined,
  SendOutlined,
} from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { botAgentsApi } from '@/api';
import type { BotCapabilityBindingInput } from '@/api/bots';
import { queryKeys } from '@/api/queryKeys';
import { usePermissions } from '@/store/permissions';
import type {
  BotAgentChannelInfo,
  BotAgentInfo,
  BotMessageLogInfo,
} from '@/types';

const { Title, Text, Paragraph } = Typography;
type BotCapabilityBindingObject = Exclude<BotCapabilityBindingInput, string>;

const CAPABILITY_OPTIONS = [
  { labelKey: 'botAgents.capability.aosRouter', fallback: '超级助手', value: 'aos_router' },
  { labelKey: 'botAgents.capability.superAdversarial', fallback: '超级对抗', value: 'super_adversarial' },
  { labelKey: 'botAgents.capability.nl2sql', fallback: '数据探索', value: 'nl2sql' },
  { labelKey: 'botAgents.capability.rdAgent', fallback: '代码开发', value: 'rd_agent' },
];

const PLATFORM_OPTIONS = [
  { labelKey: 'botAgents.platforms.genericWebhook', fallback: '通用 Webhook', value: 'generic_webhook' },
  { labelKey: 'botAgents.platforms.dingtalk', fallback: '钉钉', value: 'dingtalk' },
  { labelKey: 'botAgents.platforms.feishu', fallback: '飞书', value: 'feishu' },
  { labelKey: 'botAgents.platforms.lark', fallback: 'Lark', value: 'lark' },
  { labelKey: 'botAgents.platforms.wecom', fallback: '企业微信', value: 'wecom' },
  { labelKey: 'botAgents.platforms.telegram', fallback: 'Telegram', value: 'telegram' },
  { labelKey: 'botAgents.platforms.whatsapp', fallback: 'WhatsApp', value: 'whatsapp' },
  { labelKey: 'botAgents.platforms.slack', fallback: 'Slack', value: 'slack' },
  { labelKey: 'botAgents.platforms.discord', fallback: 'Discord', value: 'discord' },
];

const INBOUND_MODE_OPTIONS = [
  { labelKey: 'botAgents.inboundModes.auto', fallback: '自动（优先长连接/轮询）', value: 'auto' },
  { labelKey: 'botAgents.inboundModes.stream', fallback: 'Stream 长连接', value: 'stream' },
  { labelKey: 'botAgents.inboundModes.socket', fallback: 'Socket 长连接', value: 'socket' },
  { labelKey: 'botAgents.inboundModes.polling', fallback: 'Polling 轮询', value: 'polling' },
  { labelKey: 'botAgents.inboundModes.webhook', fallback: 'Webhook 回调', value: 'webhook' },
];

const PLATFORM_INBOUND_MODES: Record<string, string[]> = {
  dingtalk: ['auto', 'stream'],
  telegram: ['auto', 'polling', 'webhook'],
  generic_webhook: ['webhook'],
  feishu: ['auto', 'stream'],
  lark: ['auto', 'stream'],
  wecom: ['auto', 'stream'],
  whatsapp: ['webhook'],
  slack: ['auto', 'socket'],
  discord: ['auto', 'socket'],
};

const PLATFORM_GUIDE_URLS: Record<string, string> = {
  dingtalk: 'https://open.dingtalk.com/document/orgapp/stream-overview',
  feishu: 'https://open.feishu.cn/document/server-docs/event-subscription-guide/overview',
  lark: 'https://open.larksuite.com/document/server-docs/event-subscription-guide/overview',
  wecom: 'https://developer.work.weixin.qq.com/document/path/101463',
  slack: 'https://docs.slack.dev/apis/events-api/using-socket-mode/',
  discord: 'https://discord.com/developers/docs/events/gateway',
  telegram: 'https://core.telegram.org/bots/api#getting-updates',
  whatsapp: 'https://developers.facebook.com/docs/whatsapp/cloud-api/get-started',
};

const ADVANCED_CONFIG_EXAMPLES: Record<string, Record<string, unknown>> = {
  generic_webhook: {
    headers: { 'X-Source': 'aos' },
    authHeaderName: 'Authorization',
    authHeaderPrefix: 'Bearer',
    messageType: 'text',
    payloadTemplate: { text: '{{text}}', conversationId: '{{external_conversation_id}}' },
  },
  dingtalk: { apiBase: 'https://api.dingtalk.com', messageType: 'markdown', atAll: false, atMobiles: [], atUserIds: [], subscriptions: [] },
  feishu: { apiBase: 'https://open.feishu.cn/open-apis', receiveIdType: 'chat_id' },
  lark: { apiBase: 'https://open.larksuite.com/open-apis', receiveIdType: 'chat_id' },
  wecom: { wsBase: 'wss://openws.work.weixin.qq.com' },
  telegram: { apiBase: 'https://api.telegram.org', pollTimeoutSecs: 25, pollLimit: 50, parseMode: 'Markdown' },
  slack: { apiBase: 'https://slack.com/api', username: 'AOS', iconEmoji: ':robot_face:' },
  discord: { apiBase: 'https://discord.com/api/v10', gatewayUrl: 'wss://gateway.discord.gg/?v=10&encoding=json', intents: 33281, username: 'AOS' },
  whatsapp: { apiBase: 'https://graph.facebook.com/v20.0', previewUrl: false },
};

const NOTIFICATION_EVENT_OPTIONS = [
  {
    labelKey: 'botAgents.notificationEvents.taskCompleted',
    descriptionKey: 'botAgents.notificationEventHelp.taskCompleted',
    fallback: '任务完成',
    value: 'task.completed',
  },
  {
    labelKey: 'botAgents.notificationEvents.taskFailed',
    descriptionKey: 'botAgents.notificationEventHelp.taskFailed',
    fallback: '任务失败',
    value: 'task.failed',
  },
  {
    labelKey: 'botAgents.notificationEvents.taskWaitingInput',
    descriptionKey: 'botAgents.notificationEventHelp.taskWaitingInput',
    fallback: '等待输入',
    value: 'task.waiting_input',
  },
  {
    labelKey: 'botAgents.notificationEvents.taskWaitingApproval',
    descriptionKey: 'botAgents.notificationEventHelp.taskWaitingApproval',
    fallback: '等待审批',
    value: 'task.waiting_approval',
  },
  {
    labelKey: 'botAgents.notificationEvents.taskStalled',
    descriptionKey: 'botAgents.notificationEventHelp.taskStalled',
    fallback: '任务卡住',
    value: 'task.stalled',
  },
  {
    labelKey: 'botAgents.notificationEvents.taskCancelled',
    descriptionKey: 'botAgents.notificationEventHelp.taskCancelled',
    fallback: '取消完成',
    value: 'task.cancelled',
  },
];

interface CapabilityBindingFormValue {
  capability_key?: string;
  trigger_prefixes?: string;
  require_mention?: boolean;
  model?: string;
  models?: string;
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
}

interface AgentFormValues {
  name: string;
  description?: string;
  enabled?: boolean;
  persona_prompt?: string;
  capabilities?: CapabilityBindingFormValue[];
}

interface ChannelFormValues {
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
  keyword_prefix?: string;
  notify_on_events?: string[];
  dingtalk_client_id?: string;
  dingtalk_client_secret?: string;
  dingtalk_robot_webhook_url?: string;
  dingtalk_robot_access_token?: string;
  dingtalk_robot_signing_secret?: string;
  telegram_bot_token?: string;
  telegram_default_chat_id?: string;
  feishu_app_id?: string;
  feishu_app_secret?: string;
  feishu_verification_token?: string;
  feishu_encrypt_key?: string;
  wecom_bot_id?: string;
  wecom_bot_secret?: string;
  wecom_token?: string;
  wecom_encoding_aes_key?: string;
  slack_app_token?: string;
  default_recipient?: string;
  whatsapp_phone_number_id?: string;
  config_json?: string;
}

type ChannelPayload = {
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
};

const MANAGED_CONFIG_KEYS = [
  'keywordPrefix',
  'keyword_prefix',
  'keyword',
  'notifyOnEvents',
  'notify_on_events',
  'notificationEvents',
  'clientId',
  'client_id',
  'appId',
  'app_id',
  'appKey',
  'app_key',
  'clientSecret',
  'client_secret',
  'appSecret',
  'app_secret',
  'verificationToken',
  'verification_token',
  'encryptKey',
  'encrypt_key',
  'botId',
  'bot_id',
  'botSecret',
  'bot_secret',
  'encodingAesKey',
  'encoding_aes_key',
  'appToken',
  'app_token',
  'socketToken',
  'socket_token',
  'botToken',
  'bot_token',
  'telegramToken',
  'token',
  'defaultChatId',
  'default_chat_id',
  'defaultConversationId',
  'default_conversation_id',
  'defaultChannel',
  'default_channel',
  'defaultRecipient',
  'default_recipient',
  'chatId',
  'chat_id',
  'recipient',
  'phoneNumberId',
  'phone_number_id',
  'phoneId',
  'phone_id',
  'outboundSigningSecret',
  'outbound_signing_secret',
];

function parseJsonText(raw?: string): Record<string, unknown> | undefined {
  if (!raw?.trim()) return undefined;
  const parsed = JSON.parse(raw);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('JSON must be an object');
  }
  return parsed as Record<string, unknown>;
}

function stringifyJson(value?: Record<string, unknown> | null): string {
  if (!value) return '';
  return JSON.stringify(value, null, 2);
}

function cleanString(value?: string): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function stripManagedConfig(config?: Record<string, unknown> | null): Record<string, unknown> | undefined {
  if (!config) return undefined;
  const next = { ...config };
  MANAGED_CONFIG_KEYS.forEach((key) => {
    delete next[key];
  });
  return Object.keys(next).length > 0 ? next : undefined;
}

function splitPrefixes(raw?: string): string[] {
  return (raw ?? '')
    .split(/[,，\\s]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function stringArrayFromConfig(config: Record<string, unknown> | null | undefined, key: string | string[]): string[] {
  const keys = Array.isArray(key) ? key : [key];
  for (const itemKey of keys) {
    const value = config?.[itemKey];
    if (Array.isArray(value)) return value.filter((item): item is string => typeof item === 'string');
  }
  return [];
}

function visibleCapabilityKey(key: string): string {
  return ['ai_chat', 'generic_ai', 'pm_assistant', 'watchdog'].includes(key) ? 'aos_router' : key;
}

function boolFromConfig(config: Record<string, unknown> | null | undefined, key: string): boolean {
  return config?.[key] === true;
}

function stringFromConfig(config: Record<string, unknown> | null | undefined, keys: string[]): string {
  for (const key of keys) {
    const value = config?.[key];
    if (typeof value === 'string' && value.trim()) return value.trim();
  }
  return '';
}

function capabilityConfigWarnings(
  value: CapabilityBindingFormValue | undefined,
  t: ReturnType<typeof useTranslation>['t'],
): string[] {
  const capability = value?.capability_key;
  if (!capability) return [];
  const warnings: string[] = [];
  if (capability === 'rd_agent' && !cleanString(value.repositoryId)) {
    warnings.push(t('botAgents.diagnostics.rdRepositoryMissing', '代码开发未配置仓库 ID 时，会使用系统默认仓库；如无默认仓库，Bot 会返回配置错误。'));
  }
  if (capability === 'nl2sql' && !cleanString(value.dataSourceId)) {
    warnings.push(t('botAgents.diagnostics.nl2sqlDataSourceMissing', '数据探索未配置数据源 ID 时，会使用默认数据源；如无默认数据源，Bot 会返回配置错误。'));
  }
  if (capability === 'super_adversarial' && splitPrefixes(value.models).length < 2) {
    warnings.push(t('botAgents.diagnostics.superAdversarialModelsDefault', '超级对抗未配置至少 2 个模型时，会尝试使用菜单默认模型池；可用模型不足 2 个会创建失败。'));
  }
  if (capability === 'watchdog' && (value.allowActions ?? []).some((action) => action === 'cancel_task' || action === 'retry_task')) {
    warnings.push(t('botAgents.diagnostics.watchdogWriteActions', '看门狗已开启取消/重试动作，请只给可信 Bot 使用。所有动作会写入 AgentOps 审计事件。'));
  }
  if (capability === 'pm_assistant') {
    warnings.push(t('botAgents.diagnostics.pmAssistantShortAnswer', '产运助手默认走短答链路；开启深度分析后会创建真实产运分析任务，完成后主动推送结果。'));
  }
  return warnings;
}

function buildCapabilityPayload(item: CapabilityBindingFormValue): BotCapabilityBindingObject {
  const capabilityKey = item.capability_key?.trim();
  const triggerPrefixes = splitPrefixes(item.trigger_prefixes);
  const base: BotCapabilityBindingObject = {
    capability_key: capabilityKey,
    trigger_prefixes: triggerPrefixes,
    require_mention: item.require_mention === true,
    fallback_when_no_prefix: capabilityKey === 'aos_router' && triggerPrefixes.length === 0,
  };
  if (item.executionMode) base.executionMode = item.executionMode;
  if (item.syncTimeoutMs) base.syncTimeoutMs = Number(item.syncTimeoutMs);
  if (item.ackTimeoutMs || item.ackTimeoutMs === 0) base.ackTimeoutMs = Number(item.ackTimeoutMs);
  if (!capabilityKey) return base;
  if (['ai_chat', 'generic_ai', 'pm_assistant', 'rd_agent'].includes(capabilityKey)) {
    base.model = item.model?.trim() || undefined;
  }
  if (capabilityKey === 'super_adversarial') {
    base.models = splitPrefixes(item.models);
    base.maxRounds = item.maxRounds ? Number(item.maxRounds) : undefined;
  }
  if (capabilityKey === 'pm_assistant') {
    base.deepAnalysis = item.deepAnalysis === true;
  }
  if (capabilityKey === 'rd_agent') {
    base.repositoryId = item.repositoryId?.trim() || undefined;
    base.agentProfileId = item.agentProfileId?.trim() || undefined;
    base.workflowId = item.workflowId?.trim() || undefined;
    base.contextDepth = item.contextDepth?.trim() || undefined;
    base.shouldDeepScan = item.shouldDeepScan === true;
  }
  if (capabilityKey === 'nl2sql') {
    base.dataSourceId = item.dataSourceId?.trim() || undefined;
    base.allowExecuteSql = item.allowExecuteSql === true;
  }
  if (capabilityKey === 'watchdog') {
    base.watchdogScope = item.watchdogScope || undefined;
    base.allowActions = item.allowActions?.length ? item.allowActions : ['open_task'];
  }
  if (capabilityKey === 'aos_router') {
    base.confidenceThreshold = item.confidenceThreshold ? Number(item.confidenceThreshold) : undefined;
  }
  return base;
}

function inboundModesForPlatform(platform?: string): string[] {
  return PLATFORM_INBOUND_MODES[platform || 'generic_webhook'] ?? ['auto', 'webhook'];
}

function webhookIngressEnabled(platform?: string, inboundMode?: string): boolean {
  const mode = inboundMode || 'auto';
  return mode === 'webhook'
    || (mode === 'auto' && ['whatsapp', 'generic_webhook'].includes(platform || 'generic_webhook'));
}

function buildChannelConfig(values: ChannelFormValues): Record<string, unknown> | undefined {
  const config = parseJsonText(values.config_json) ?? {};
  MANAGED_CONFIG_KEYS.forEach((key) => {
    delete config[key];
  });

  const keyword = values.keyword_prefix?.trim();
  const notifyOnEvents = values.notify_on_events ?? [];

  if (values.platform === 'dingtalk') {
    const clientId = cleanString(values.dingtalk_client_id);
    if (clientId) {
      config.clientId = clientId;
    }
  }

  if (values.platform === 'feishu' || values.platform === 'lark') {
    const appId = cleanString(values.feishu_app_id);
    const verificationToken = cleanString(values.feishu_verification_token);
    const encryptKey = cleanString(values.feishu_encrypt_key);
    if (appId) {
      config.appId = appId;
    }
    if (verificationToken) {
      config.verificationToken = verificationToken;
    }
    if (encryptKey) {
      config.encryptKey = encryptKey;
    }
  }

  if (values.platform === 'wecom') {
    const botId = cleanString(values.wecom_bot_id);
    const token = cleanString(values.wecom_token);
    const encodingAesKey = cleanString(values.wecom_encoding_aes_key);
    if (botId) {
      config.botId = botId;
    }
    if (token) {
      config.token = token;
    }
    if (encodingAesKey) {
      config.encodingAesKey = encodingAesKey;
    }
  }

  if (values.platform === 'telegram') {
    const defaultChatId = cleanString(values.telegram_default_chat_id);
    if (defaultChatId) {
      config.defaultChatId = defaultChatId;
    }
  }

  const defaultRecipient = cleanString(values.default_recipient);
  if (defaultRecipient) {
    config.defaultConversationId = defaultRecipient;
  }

  const whatsappPhoneNumberId = cleanString(values.whatsapp_phone_number_id);
  if (values.platform === 'whatsapp' && whatsappPhoneNumberId) {
    config.phoneNumberId = whatsappPhoneNumberId;
  }

  if (keyword) {
    config.keywordPrefix = keyword;
  }
  if (notifyOnEvents.length > 0) {
    config.notifyOnEvents = notifyOnEvents;
  }
  return Object.keys(config).length > 0 ? config : undefined;
}

function buildChannelPayload(values: ChannelFormValues): ChannelPayload {
  const platform = values.platform || 'generic_webhook';
  const payload: ChannelPayload = {
    agent_id: values.agent_id,
    platform,
    name: values.name,
    enabled: values.enabled,
    inbound_mode: values.inbound_mode,
    inbound_secret: cleanString(values.inbound_secret),
    config_json: buildChannelConfig(values),
  };

  if (platform === 'dingtalk') {
    payload.outbound_webhook_url = cleanString(values.dingtalk_robot_webhook_url);
    payload.outbound_token = cleanString(values.dingtalk_robot_access_token);
    payload.signing_secret = cleanString(values.dingtalk_client_secret);
    payload.outbound_signing_secret = cleanString(values.dingtalk_robot_signing_secret);
  } else if (platform === 'feishu' || platform === 'lark') {
    payload.outbound_webhook_url = cleanString(values.outbound_webhook_url);
    payload.signing_secret = cleanString(values.feishu_app_secret);
    payload.outbound_signing_secret = cleanString(values.outbound_signing_secret);
  } else if (platform === 'wecom') {
    payload.outbound_webhook_url = cleanString(values.outbound_webhook_url);
    payload.signing_secret = cleanString(values.wecom_bot_secret);
  } else if (platform === 'telegram') {
    payload.outbound_token = cleanString(values.telegram_bot_token);
  } else if (platform === 'slack') {
    payload.outbound_webhook_url = cleanString(values.outbound_webhook_url);
    payload.outbound_token = cleanString(values.outbound_token);
    payload.signing_secret = cleanString(values.slack_app_token);
  } else if (platform === 'discord') {
    payload.outbound_webhook_url = cleanString(values.outbound_webhook_url);
    payload.outbound_token = cleanString(values.outbound_token);
  } else {
    payload.outbound_webhook_url = cleanString(values.outbound_webhook_url);
    payload.outbound_token = cleanString(values.outbound_token);
    payload.signing_secret = cleanString(values.signing_secret);
    payload.outbound_signing_secret = cleanString(values.outbound_signing_secret);
  }

  return payload;
}

function makeWebhookUrl(channelId: string): string {
  if (typeof window === 'undefined') return `/api/v1/bot-agents/webhooks/${channelId}`;
  return `${window.location.origin}/api/v1/bot-agents/webhooks/${channelId}`;
}

function formatBotTimestamp(value?: string | null): string {
  if (!value) return '-';
  const normalized = /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?$/.test(value)
    ? `${value.replace(' ', 'T')}Z`
    : value;
  const date = new Date(normalized);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export default function BotAgents() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { hasPermission } = usePermissions();
  const canWrite = hasPermission('bot_agents:write');
  const canDelete = hasPermission('bot_agents:delete');
  const [activeTab, setActiveTab] = useState('agents');
  const [agentModalOpen, setAgentModalOpen] = useState(false);
  const [editingAgent, setEditingAgent] = useState<BotAgentInfo | null>(null);
  const [channelModalOpen, setChannelModalOpen] = useState(false);
  const [editingChannel, setEditingChannel] = useState<BotAgentChannelInfo | null>(null);
  const [detailAgent, setDetailAgent] = useState<BotAgentInfo | null>(null);
  const [testingChannelId, setTestingChannelId] = useState<string | null>(null);
  const [pairingChannelId, setPairingChannelId] = useState<string>();
  const [pairCode, setPairCode] = useState<{
    code: string;
    expiresInSeconds: number;
  } | null>(null);
  const [advancedHelpOpen, setAdvancedHelpOpen] = useState(false);
  const [agentForm] = Form.useForm<AgentFormValues>();
  const [channelForm] = Form.useForm<ChannelFormValues>();
  const watchedCapabilities = Form.useWatch('capabilities', agentForm) ?? [];
  const selectedChannelPlatform = Form.useWatch('platform', channelForm);
  const selectedInboundMode = Form.useWatch('inbound_mode', channelForm);

  const agentsQuery = useQuery({
    queryKey: queryKeys.botAgents.list({ page: 1, per_page: 100 }),
    queryFn: () => botAgentsApi.list({ page: 1, per_page: 100 }),
  });
  const channelsQuery = useQuery({
    queryKey: queryKeys.botAgents.channels({ page: 1, per_page: 100 }),
    queryFn: () => botAgentsApi.listChannels({ page: 1, per_page: 100 }),
  });
  const logsQuery = useQuery({
    queryKey: queryKeys.botAgents.logs({ page: 1, per_page: 50 }),
    queryFn: () => botAgentsApi.listLogs({ page: 1, per_page: 50 }),
  });
  const identitiesQuery = useQuery({
    queryKey: queryKeys.botAgents.identities(),
    queryFn: botAgentsApi.listIdentities,
    refetchInterval: activeTab === 'identities' ? 5000 : false,
  });
  const agents = agentsQuery.data?.items ?? [];
  const channels = channelsQuery.data?.items ?? [];

  const agentNameMap = useMemo(() => new Map(agents.map((item) => [item.id, item.name])), [agents]);
  const channelNameMap = useMemo(() => new Map(channels.map((item) => [item.id, item.name])), [channels]);
  const capabilityOptions = useMemo(
    () => CAPABILITY_OPTIONS.map((item) => ({ label: t(item.labelKey, item.fallback), value: item.value })),
    [t],
  );
  const platformOptions = useMemo(
    () => PLATFORM_OPTIONS.map((item) => ({ label: t(item.labelKey, item.fallback), value: item.value })),
    [t],
  );
  const inboundModeOptions = useMemo(
    () => {
      const allowed = new Set(inboundModesForPlatform(selectedChannelPlatform));
      return INBOUND_MODE_OPTIONS
        .filter((item) => allowed.has(item.value))
        .map((item) => ({ label: t(item.labelKey, item.fallback), value: item.value }));
    },
    [selectedChannelPlatform, t],
  );
  const notificationEventOptions = useMemo(
    () => NOTIFICATION_EVENT_OPTIONS.map((item) => ({ label: t(item.labelKey, item.fallback), value: item.value })),
    [t],
  );
  const capabilityLabel = (value: string) => {
    const visible = visibleCapabilityKey(value);
    return capabilityOptions.find((item) => item.value === visible)?.label ?? visible;
  };
  const platformLabel = (value: string) => platformOptions.find((item) => item.value === value)?.label ?? value;
  const inboundModeLabel = (value?: string) => t(`botAgents.inboundModes.${value || 'auto'}`, value || 'auto');
  const directionLabel = (value: string) => t(`botAgents.directions.${value}`, value);
  const statusLabel = (value?: string | null) => {
    const key = value || 'unknown';
    return t(`botAgents.statuses.${key}`, key);
  };
  const queueStatusLabel = (value?: string | null) => {
    const key = value && value !== 'undefined' ? value : 'none';
    return t(`botAgents.queueStatuses.${key}`, t('botAgents.queueStatuses.unknown', '未知'));
  };
  const queueStatusColor = (value?: string | null) => {
    if (value === 'succeeded') return 'success';
    if (value === 'dead' || value === 'failed') return 'error';
    if (value === 'claimed') return 'processing';
    if (value === 'queued') return 'warning';
    return 'default';
  };

  useEffect(() => {
    const allowed = inboundModesForPlatform(selectedChannelPlatform);
    const current = channelForm.getFieldValue('inbound_mode') || 'auto';
    if (!allowed.includes(current)) {
      channelForm.setFieldValue('inbound_mode', allowed[0]);
    }
  }, [channelForm, selectedChannelPlatform]);

  useEffect(() => {
    if (!pairingChannelId) {
      setPairingChannelId(channels.find((channel) => channel.enabled)?.id);
    }
  }, [channels, pairingChannelId]);

  const inboundStatusColor = (value?: string) => {
    switch ((value || '').toLowerCase()) {
      case 'connected':
      case 'polling':
        return 'green';
      case 'connecting':
      case 'processing':
        return 'blue';
      case 'error':
      case 'failed':
      case 'unsupported':
        return 'red';
      default:
        return 'default';
    }
  };

  const platformInboundHelp = (platform?: string) => {
    switch (platform) {
      case 'dingtalk':
        return t('botAgents.platformInboundHelp.dingtalk', '钉钉通道会分成“接收群消息”和“发送群消息”两组字段：接收使用 Stream 长连接凭证，发送使用群机器人 Webhook/加签。');
      case 'telegram':
        return t('botAgents.platformInboundHelp.telegram', 'Telegram 自动模式会使用 getUpdates 轮询。填写 Bot Token 后即可接收入站消息；如需测试发送或主动通知，请填写默认 Chat ID。');
      case 'feishu':
        return t('botAgents.platformInboundHelp.feishu', '飞书自动模式使用官方长连接接收消息事件；填写 App ID 和 App Secret 后，本地运行不需要公网回调 URL。');
      case 'lark':
        return t('botAgents.platformInboundHelp.lark', 'Lark 自动模式使用国际版官方长连接与 API；填写 App ID 和 App Secret 后，本地运行不需要公网回调 URL。');
      case 'slack':
        return t('botAgents.platformInboundHelp.slack', 'Slack 自动模式会使用 Socket Mode 长连接接收 message/app_mention 事件；填写 App-Level Token 后，本地运行也不需要公网回调 URL。');
      case 'whatsapp':
        return t('botAgents.platformInboundHelp.whatsapp', 'WhatsApp Cloud API 官方入站需要公网可访问的 Webhook；本地部署需使用隧道或中转服务，不支持飞书、钉钉式的本地长连接。');
      case 'wecom':
        return t('botAgents.platformInboundHelp.wecom', '企业微信自动模式会使用智能机器人 WebSocket 长连接；填写 Bot ID 和 Bot Secret 后，本地运行也不需要公网回调 URL。');
      case 'discord':
        return t('botAgents.platformInboundHelp.discord', 'Discord 自动模式会使用 Gateway WebSocket 接收消息事件；填写 Bot Token 后，本地运行也不需要公网回调 URL。');
      default:
        return t('botAgents.platformInboundHelp.generic', '通用 Webhook 适合自定义机器人或临时接入：入站使用 AOS 生成的回调 URL，出站使用你填写的目标 Webhook。');
    }
  };

  const invalidateAll = () => {
    qc.invalidateQueries({ queryKey: queryKeys.botAgents.all });
  };

  const saveAgentMutation = useMutation({
    mutationFn: async (values: AgentFormValues) => {
      const capabilities: BotCapabilityBindingObject[] = (values.capabilities ?? [])
        .map(buildCapabilityPayload)
        .filter((item) => item.capability_key);
      const payload = {
        name: values.name,
        description: values.description,
        enabled: values.enabled,
        persona_prompt: values.persona_prompt,
        default_capability: capabilities.find((item) => item.capability_key === 'aos_router')?.capability_key
          ?? capabilities[0]?.capability_key
          ?? 'aos_router',
        capabilities: capabilities.length ? capabilities : [{
          capability_key: 'aos_router',
          trigger_prefixes: [],
          require_mention: false,
          fallback_when_no_prefix: true,
        }],
      };
      if (editingAgent) return botAgentsApi.update(editingAgent.id, payload);
      return botAgentsApi.create(payload);
    },
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      setAgentModalOpen(false);
      setEditingAgent(null);
      agentForm.resetFields();
      invalidateAll();
    },
    onError: (err: Error) => message.error(err.message ?? t('common.operationFailed')),
  });

  const deleteAgentMutation = useMutation({
    mutationFn: (id: string) => botAgentsApi.delete(id),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      invalidateAll();
    },
    onError: (err: Error) => message.error(err.message ?? t('common.operationFailed')),
  });

  const saveChannelMutation = useMutation({
    mutationFn: async (values: ChannelFormValues) => {
      const payload = buildChannelPayload(values);
      if (editingChannel) return botAgentsApi.updateChannel(editingChannel.id, payload);
      return botAgentsApi.createChannel(payload);
    },
    onSuccess: (savedChannel) => {
      const created = !editingChannel;
      message.success(created
        ? t('botAgents.channelCreatedContinuePairing', '通道已创建，请继续完成身份绑定')
        : t('common.operateSuccess'));
      setChannelModalOpen(false);
      setEditingChannel(null);
      channelForm.resetFields();
      invalidateAll();
      if (created) {
        setPairingChannelId(savedChannel.id);
        setPairCode(null);
        setActiveTab('identities');
      }
    },
    onError: (err: Error) => message.error(err.message ?? t('common.operationFailed')),
  });

  const deleteChannelMutation = useMutation({
    mutationFn: (id: string) => botAgentsApi.deleteChannel(id),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      invalidateAll();
    },
    onError: (err: Error) => message.error(err.message ?? t('common.operationFailed')),
  });

  const testChannelMutation = useMutation({
    mutationFn: (input: { id: string; title: string; text: string }) =>
      botAgentsApi.testChannel(input.id, { title: input.title, text: input.text }),
    onSuccess: () => {
      message.success(t('botAgents.testSendSuccess', '测试消息已发送'));
      invalidateAll();
    },
    onError: (err: Error, input) => {
      if (err.message?.includes('bot_identity_binding_required')) {
        setPairingChannelId(input.id);
        setPairCode(null);
        setActiveTab('identities');
        message.warning({
          content: t(
            'botAgents.testSendIdentityRequired',
            '测试发送前需要先绑定当前社交账号。已为你打开「身份绑定」，生成绑定码并在 Bot 私聊中发送后再试。',
          ),
          duration: 8,
        });
        return;
      }
      message.error(err.message ?? t('botAgents.testSendFailed', '测试发送失败'));
    },
    onSettled: () => setTestingChannelId(null),
  });

  const pairingMutation = useMutation({
    mutationFn: (channelId: string) => {
      const channel = channels.find((item) => item.id === channelId);
      if (!channel) {
        throw new Error(t('botAgents.selectPairingChannel', '请先选择要绑定的通道'));
      }
      return botAgentsApi.createPairingCode(channel.platform, channel.id);
    },
    onSuccess: (result) => setPairCode(result),
    onError: (err: Error) => message.error(err.message ?? t('common.operationFailed')),
  });

  const revokeIdentityMutation = useMutation({
    mutationFn: botAgentsApi.revokeIdentity,
    onSuccess: () => {
      message.success(t('botAgents.identityRevoked', '身份绑定已解除'));
      qc.invalidateQueries({ queryKey: queryKeys.botAgents.identities() });
    },
    onError: (err: Error) => message.error(err.message ?? t('common.operationFailed')),
  });

  const openAgentModal = (agent?: BotAgentInfo) => {
    setEditingAgent(agent ?? null);
    agentForm.setFieldsValue(agent ? {
      name: agent.name,
      description: agent.description ?? undefined,
      enabled: agent.enabled,
      persona_prompt: agent.persona_prompt ?? undefined,
      capabilities: agent.capabilities
        .map((item) => ({ ...item, capability_key: visibleCapabilityKey(item.capability_key) }))
        .filter((item, index, items) => items.findIndex((candidate) => candidate.capability_key === item.capability_key) === index)
        .map((item) => ({
        capability_key: item.capability_key,
        trigger_prefixes: stringArrayFromConfig(item.config_json, 'trigger_prefixes').join(', '),
        require_mention: boolFromConfig(item.config_json, 'require_mention'),
        model: stringFromConfig(item.config_json, ['model']),
        models: stringArrayFromConfig(item.config_json, 'models').join(', '),
        maxRounds: typeof item.config_json?.maxRounds === 'number' ? item.config_json.maxRounds : undefined,
        repositoryId: stringFromConfig(item.config_json, ['repositoryId', 'repository_id']),
        agentProfileId: stringFromConfig(item.config_json, ['agentProfileId', 'agent_profile_id']),
        workflowId: stringFromConfig(item.config_json, ['workflowId', 'workflow_id']),
        contextDepth: stringFromConfig(item.config_json, ['contextDepth', 'context_depth']),
        shouldDeepScan: boolFromConfig(item.config_json, 'shouldDeepScan') || boolFromConfig(item.config_json, 'should_deep_scan'),
        dataSourceId: stringFromConfig(item.config_json, ['dataSourceId', 'data_source_id']),
        allowExecuteSql: boolFromConfig(item.config_json, 'allowExecuteSql'),
        deepAnalysis: boolFromConfig(item.config_json, 'deepAnalysis') || boolFromConfig(item.config_json, 'deep_analysis'),
        watchdogScope: stringFromConfig(item.config_json, ['watchdogScope', 'watchdog_scope']),
        allowActions: stringArrayFromConfig(item.config_json, ['allowActions', 'allow_actions']),
        executionMode: stringFromConfig(item.config_json, ['executionMode', 'execution_mode']),
        syncTimeoutMs: typeof item.config_json?.syncTimeoutMs === 'number' ? item.config_json.syncTimeoutMs : undefined,
        ackTimeoutMs: typeof item.config_json?.ackTimeoutMs === 'number' ? item.config_json.ackTimeoutMs : undefined,
        confidenceThreshold: typeof item.config_json?.confidenceThreshold === 'number' ? item.config_json.confidenceThreshold : undefined,
      })),
    } : {
      enabled: true,
      capabilities: [{
        capability_key: 'aos_router',
        require_mention: false,
      }],
    });
    setAgentModalOpen(true);
  };

  const openIdentityPairing = (channel: BotAgentChannelInfo) => {
    setPairingChannelId(channel.id);
    setPairCode(null);
    setActiveTab('identities');
  };

  const openChannelModal = (channel?: BotAgentChannelInfo) => {
    setEditingChannel(channel ?? null);
    channelForm.resetFields();
    const config = channel?.config_json;
    channelForm.setFieldsValue(channel ? {
      agent_id: channel.agent_id,
      platform: channel.platform,
      name: channel.name,
      enabled: channel.enabled,
      inbound_mode: channel.inbound_mode || 'auto',
      outbound_webhook_url: channel.outbound_webhook_url ?? undefined,
      outbound_token: !channel.outbound_token_set
        ? stringFromConfig(config, ['botToken', 'bot_token', 'token', 'accessToken', 'access_token']) || undefined
        : undefined,
      signing_secret: !channel.signing_secret_set
        ? stringFromConfig(config, ['signingSecret', 'signing_secret', 'secret']) || undefined
        : undefined,
      outbound_signing_secret: !channel.outbound_signing_secret_set
        ? stringFromConfig(config, ['outboundSigningSecret', 'outbound_signing_secret', 'signingSecret', 'signing_secret']) || undefined
        : undefined,
      dingtalk_client_id: stringFromConfig(config, ['clientId', 'client_id', 'appKey', 'app_key']) || undefined,
      dingtalk_client_secret: !channel.signing_secret_set
        ? stringFromConfig(config, ['clientSecret', 'client_secret', 'appSecret', 'app_secret']) || undefined
        : undefined,
      dingtalk_robot_webhook_url: channel.outbound_webhook_url ?? undefined,
      dingtalk_robot_access_token: !channel.outbound_token_set
        ? stringFromConfig(config, ['botToken', 'bot_token', 'telegramToken', 'token']) || undefined
        : undefined,
      feishu_app_id: stringFromConfig(config, ['appId', 'app_id', 'appKey', 'app_key']) || undefined,
      feishu_app_secret: !channel.signing_secret_set
        ? stringFromConfig(config, ['appSecret', 'app_secret', 'clientSecret', 'client_secret']) || undefined
        : undefined,
      feishu_verification_token: stringFromConfig(config, ['verificationToken', 'verification_token']) || undefined,
      feishu_encrypt_key: stringFromConfig(config, ['encryptKey', 'encrypt_key']) || undefined,
      wecom_bot_id: stringFromConfig(config, ['botId', 'bot_id']) || undefined,
      wecom_bot_secret: !channel.signing_secret_set
        ? stringFromConfig(config, ['botSecret', 'bot_secret', 'secret']) || undefined
        : undefined,
      wecom_token: stringFromConfig(config, ['token']) || undefined,
      wecom_encoding_aes_key: stringFromConfig(config, ['encodingAesKey', 'encoding_aes_key']) || undefined,
      telegram_bot_token: !channel.outbound_token_set
        ? stringFromConfig(config, ['botToken', 'bot_token', 'telegramToken', 'token']) || undefined
        : undefined,
      slack_app_token: !channel.signing_secret_set
        ? stringFromConfig(config, ['appToken', 'app_token', 'socketToken', 'socket_token']) || undefined
        : undefined,
      telegram_default_chat_id: stringFromConfig(config, ['defaultChatId', 'default_chat_id', 'chatId', 'chat_id', 'recipient']) || undefined,
      default_recipient: stringFromConfig(config, ['defaultConversationId', 'default_conversation_id', 'defaultChannel', 'default_channel', 'defaultRecipient', 'default_recipient', 'recipient', 'channel', 'chat_id']) || undefined,
      whatsapp_phone_number_id: stringFromConfig(config, ['phoneNumberId', 'phone_number_id', 'phoneId', 'phone_id']) || undefined,
      keyword_prefix: stringFromConfig(config, ['keywordPrefix', 'keyword_prefix', 'keyword']) || undefined,
      notify_on_events: stringArrayFromConfig(config, 'notifyOnEvents'),
      config_json: stringifyJson(stripManagedConfig(config)),
    } : {
      agent_id: detailAgent?.id ?? agents[0]?.id,
      platform: 'generic_webhook',
      inbound_mode: 'webhook',
      enabled: true,
      name: 'Default Channel',
    });
    setChannelModalOpen(true);
  };

  const secretPlaceholder = (isSet?: boolean) => (
    isSet ? t('botAgents.keepSecret', '留空则保持原值') : undefined
  );

  const renderGenericWebhookSettings = () => (
    <Card
      size="small"
      title={t('botAgents.genericWebhookSection', '通用 Webhook 配置')}
      style={{ marginBottom: 16 }}
    >
      <Alert
        showIcon
        type="info"
        style={{ marginBottom: 16 }}
        message={t('botAgents.genericWebhookNotice', '适合自定义机器人或临时中转服务：入站走 AOS 生成的 Webhook URL，出站会按通用 JSON payload POST 到目标地址。')}
      />
      <Form.Item
        name="outbound_webhook_url"
        label={t('botAgents.outboundWebhook', '出站 Webhook URL')}
        extra={t('botAgents.genericOutboundWebhookHelp', 'AOS 主动发送测试消息、完成通知或机器人回复时，会 POST 到这个 URL。')}
      >
        <Input placeholder="https://example.com/webhook" />
      </Form.Item>
      <Form.Item
        name="outbound_token"
        label={t('botAgents.outboundTokenOptional', '出站 Token（可选）')}
      >
        <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_token_set)} />
      </Form.Item>
      <Form.Item
        name="signing_secret"
        label={t('botAgents.signingSecretOptional', '签名 Secret（可选）')}
      >
        <Input.Password placeholder={secretPlaceholder(editingChannel?.signing_secret_set)} />
      </Form.Item>
    </Card>
  );

  const renderFeishuSettings = () => (
    <Card
      size="small"
      title={selectedChannelPlatform === 'lark'
        ? t('botAgents.larkSection', 'Lark 配置')
        : t('botAgents.feishuSection', '飞书配置')}
      style={{ marginBottom: 16 }}
    >
      <Alert
        showIcon
        type="info"
        style={{ marginBottom: 16 }}
        message={selectedChannelPlatform === 'lark'
          ? t('botAgents.larkNotice', '入站使用 Lark 国际版长连接，本地运行不需要公网域名；出站使用 Lark 自定义机器人 Webhook 或 OpenAPI。')
          : t('botAgents.feishuNotice', '入站使用飞书长连接，本地运行不需要公网域名；出站使用自定义机器人 Webhook 或 OpenAPI。')}
      />
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="feishu_app_id"
            label={selectedChannelPlatform === 'lark' ? 'Lark App ID' : t('botAgents.feishuAppId', '飞书 App ID')}
            extra={t('botAgents.feishuAppIdHelp', '用于长连接事件订阅。')}
          >
            <Input placeholder="cli_xxxxxxxxxxxxx" />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="feishu_app_secret"
            label={selectedChannelPlatform === 'lark' ? 'Lark App Secret' : t('botAgents.feishuAppSecret', '飞书 App Secret')}
          >
            <Input.Password placeholder={secretPlaceholder(editingChannel?.signing_secret_set)} />
          </Form.Item>
        </Col>
      </Row>
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="feishu_verification_token"
            label={t('botAgents.feishuVerificationToken', 'Verification Token（可选）')}
          >
            <Input.Password />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="feishu_encrypt_key"
            label={t('botAgents.feishuEncryptKey', 'Encrypt Key（可选）')}
          >
            <Input.Password />
          </Form.Item>
        </Col>
      </Row>
      <Form.Item
        name="outbound_webhook_url"
        label={selectedChannelPlatform === 'lark' ? 'Lark Custom Bot Webhook URL' : t('botAgents.feishuWebhook', '飞书自定义机器人 Webhook URL')}
        extra={t('botAgents.feishuWebhookHelp', '用于 AOS 主动发送测试消息、完成通知或机器人回复；留空时会使用 App ID/App Secret + 默认 Chat ID 调用飞书 OpenAPI。')}
      >
        <Input placeholder={selectedChannelPlatform === 'lark' ? 'https://open.larksuite.com/open-apis/bot/v2/hook/...' : 'https://open.feishu.cn/open-apis/bot/v2/hook/...'} />
      </Form.Item>
      <Form.Item
        name="default_recipient"
        label={selectedChannelPlatform === 'lark' ? 'Default Lark Chat ID' : t('botAgents.feishuDefaultChatId', '默认飞书 Chat ID（可选）')}
        extra={t('botAgents.feishuDefaultChatIdHelp', '未填写自定义机器人 Webhook 时，测试发送和主动通知会发到这个 chat_id；也可以先在飞书给机器人发一条消息，AOS 会自动复用最近会话。')}
      >
        <Input placeholder="oc_xxxxxxxxxxxxx" />
      </Form.Item>
      <Form.Item
        name="outbound_signing_secret"
        label={selectedChannelPlatform === 'lark' ? 'Lark Bot Signing Secret (optional)' : t('botAgents.feishuSigningSecret', '飞书机器人加签 Secret（可选）')}
        extra={t('botAgents.feishuSigningSecretHelp', '仅当飞书自定义机器人开启“签名校验”时填写。')}
      >
        <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_signing_secret_set)} />
      </Form.Item>
    </Card>
  );

  const renderWeComSettings = () => (
    <Card size="small" title={t('botAgents.wecomSection', '企业微信配置')} style={{ marginBottom: 16 }}>
      <Alert
        showIcon
        type="info"
        style={{ marginBottom: 16 }}
        message={t('botAgents.wecomNotice', '入站优先使用企业微信智能机器人 WebSocket 长连接，本地运行不需要公网域名；出站可继续使用群机器人 Webhook。')}
      />
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="wecom_bot_id"
            label={t('botAgents.wecomBotId', '企业微信 Bot ID')}
            extra={t('botAgents.wecomBotIdHelp', '企业微信智能机器人配置里的 BotID。')}
          >
            <Input placeholder="bot_xxxxxxxxxxxxx" />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="wecom_bot_secret"
            label={t('botAgents.wecomBotSecret', '企业微信 Bot Secret')}
          >
            <Input.Password placeholder={secretPlaceholder(editingChannel?.signing_secret_set)} />
          </Form.Item>
        </Col>
      </Row>
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="wecom_token"
            label={t('botAgents.wecomToken', 'Token（可选）')}
          >
            <Input.Password />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="wecom_encoding_aes_key"
            label={t('botAgents.wecomEncodingAesKey', 'EncodingAESKey（可选）')}
          >
            <Input.Password />
          </Form.Item>
        </Col>
      </Row>
      <Form.Item
        name="outbound_webhook_url"
        label={t('botAgents.wecomWebhook', '企业微信群机器人 Webhook URL')}
      >
        <Input placeholder="https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=..." />
      </Form.Item>
    </Card>
  );

  const renderSlackSettings = () => (
    <Card size="small" title={t('botAgents.slackSection', 'Slack 配置')} style={{ marginBottom: 16 }}>
      <Alert
        showIcon
        type="info"
        style={{ marginBottom: 16 }}
        message={t('botAgents.slackNotice', '入站优先使用 Slack Socket Mode 长连接，本地运行不需要公网域名；出站优先使用 Incoming Webhook，未填写时使用 Bot Token + Channel ID。')}
      />
      <Form.Item
        name="slack_app_token"
        label={t('botAgents.slackAppToken', 'Slack App-Level Token')}
        extra={t('botAgents.slackAppTokenHelp', '用于 Socket Mode 入站长连接，通常以 xapp- 开头；留空会保持已保存值。')}
      >
        <Input.Password placeholder={secretPlaceholder(editingChannel?.signing_secret_set)} />
      </Form.Item>
      <Form.Item
        name="outbound_webhook_url"
        label={t('botAgents.slackIncomingWebhook', 'Slack Incoming Webhook URL（可选）')}
      >
        <Input placeholder="https://hooks.slack.com/services/..." />
      </Form.Item>
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="outbound_token"
            label={t('botAgents.slackBotToken', 'Slack Bot Token（可选）')}
            extra={t('botAgents.slackBotTokenHelp', '未填写 Incoming Webhook URL 时使用，通常以 xoxb- 开头。')}
          >
            <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_token_set)} />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="default_recipient"
            label={t('botAgents.defaultSlackChannel', '默认 Slack Channel ID')}
            extra={t('botAgents.defaultSlackChannelHelp', '未填写 Incoming Webhook URL、改用 Bot Token 发送时必填。')}
          >
            <Input placeholder="C0123456789" />
          </Form.Item>
        </Col>
      </Row>
    </Card>
  );

  const renderWhatsAppSettings = () => (
    <Card size="small" title={t('botAgents.whatsappSection', 'WhatsApp 配置')} style={{ marginBottom: 16 }}>
      <Alert
        showIcon
        type="info"
        style={{ marginBottom: 16 }}
        message={t('botAgents.whatsappNotice', 'WhatsApp Cloud API 官方入站主要使用 Webhook；本地无公网测试建议使用隧道或中转服务，AOS 不会伪装成轮询/长连接。')}
      />
      <Form.Item
        name="outbound_webhook_url"
        label={t('botAgents.whatsappRelayWebhook', '中转 Webhook URL（可选）')}
        extra={t('botAgents.whatsappRelayWebhookHelp', '如果你有自建 WhatsApp 中转服务，填写后优先 POST 到这里；留空则使用 Cloud API。')}
      >
        <Input placeholder="https://example.com/whatsapp/send" />
      </Form.Item>
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="outbound_token"
            label={t('botAgents.whatsappAccessToken', 'WhatsApp Access Token（可选）')}
          >
            <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_token_set)} />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="whatsapp_phone_number_id"
            label={t('botAgents.whatsappPhoneNumberId', 'WhatsApp Phone Number ID')}
            extra={t('botAgents.whatsappPhoneNumberIdHelp', '使用 WhatsApp Cloud API 且未填写中转 Webhook URL 时必填。')}
          >
            <Input placeholder="123456789012345" />
          </Form.Item>
        </Col>
      </Row>
      <Form.Item
        name="default_recipient"
        label={t('botAgents.defaultRecipient', '默认接收人 / 会话 ID')}
        extra={t('botAgents.defaultRecipientHelp', '没有入站会话上下文时，主动通知会发到这个接收人。')}
      >
        <Input placeholder="6281234567890" />
      </Form.Item>
    </Card>
  );

  const renderDiscordSettings = () => (
    <Card size="small" title={t('botAgents.discordSection', 'Discord 配置')} style={{ marginBottom: 16 }}>
      <Alert
        showIcon
        type="info"
        style={{ marginBottom: 16 }}
        message={t('botAgents.discordNotice', '入站优先使用 Discord Gateway WebSocket，本地运行不需要公网域名；出站优先使用 Discord Webhook，未填写时使用 Bot Token + Channel ID。')}
      />
      <Form.Item
        name="outbound_webhook_url"
        label={t('botAgents.discordWebhook', 'Discord Webhook URL（可选）')}
      >
        <Input placeholder="https://discord.com/api/webhooks/..." />
      </Form.Item>
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="outbound_token"
            label={t('botAgents.discordBotToken', 'Discord Bot Token（可选）')}
          >
            <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_token_set)} />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="default_recipient"
            label={t('botAgents.discordChannelId', 'Discord Channel ID')}
            extra={t('botAgents.discordChannelIdHelp', '未填写 Webhook URL、改用 Bot Token 发送时必填。')}
          >
            <Input placeholder="123456789012345678" />
          </Form.Item>
        </Col>
      </Row>
    </Card>
  );

  const renderDingTalkSettings = () => (
    <Space direction="vertical" size={12} style={{ width: '100%', marginBottom: 16 }}>
      <Card size="small" title={t('botAgents.dingtalkInboundSection', '接收群消息（Stream 长连接）')}>
        <Alert
          showIcon
          type="info"
          style={{ marginBottom: 16 }}
          message={t('botAgents.dingtalkInboundNotice', '用于让 AOS 主动连接钉钉开放平台接收群消息，不需要公网回调 URL。')}
        />
        <Row gutter={12}>
          <Col xs={24} md={12}>
            <Form.Item
              name="dingtalk_client_id"
              label={t('botAgents.dingtalkClientId', 'Client ID / AppKey')}
              extra={t('botAgents.dingtalkClientIdHelp', '来自钉钉开放平台应用凭证。')}
            >
              <Input placeholder="dingxxxxxxxxxxxx" />
            </Form.Item>
          </Col>
          <Col xs={24} md={12}>
            <Form.Item
              name="dingtalk_client_secret"
              label={t('botAgents.dingtalkClientSecret', 'Client Secret / AppSecret')}
              extra={t('botAgents.dingtalkClientSecretHelp', '只用于 Stream 长连接鉴权；留空会保持已保存值。')}
            >
              <Input.Password placeholder={secretPlaceholder(editingChannel?.signing_secret_set)} />
            </Form.Item>
          </Col>
        </Row>
      </Card>

      <Card size="small" title={t('botAgents.dingtalkOutboundSection', '发送群消息（群机器人）')}>
        <Alert
          showIcon
          type="info"
          style={{ marginBottom: 16 }}
          message={t('botAgents.dingtalkOutboundNotice', '用于 AOS 主动发送测试消息、完成通知和机器人回复。它和上面的 Stream 凭证不是同一组配置。')}
        />
        <Form.Item
          name="dingtalk_robot_webhook_url"
          label={t('botAgents.dingtalkRobotWebhook', '群机器人 Webhook URL')}
          extra={t('botAgents.dingtalkRobotWebhookHelp', '从钉钉群机器人设置里复制；如果 URL 已带 access_token，下方 Access Token 可以留空。')}
        >
          <Input placeholder="https://oapi.dingtalk.com/robot/send?access_token=..." />
        </Form.Item>
        <Row gutter={12}>
          <Col xs={24} md={12}>
            <Form.Item
              name="dingtalk_robot_access_token"
              label={t('botAgents.dingtalkRobotAccessToken', '群机器人 Access Token（可选）')}
            >
              <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_token_set)} />
            </Form.Item>
          </Col>
          <Col xs={24} md={12}>
            <Form.Item
              name="dingtalk_robot_signing_secret"
              label={t('botAgents.dingtalkRobotSigningSecret', '群机器人加签 Secret（可选）')}
              extra={t('botAgents.dingtalkRobotSigningSecretHelp', '仅当钉钉群机器人开启“加签”安全设置时填写。')}
            >
              <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_signing_secret_set)} />
            </Form.Item>
          </Col>
        </Row>
        <Form.Item
          name="keyword_prefix"
          label={t('botAgents.dingtalkKeyword', '钉钉安全关键词')}
          extra={t('botAgents.dingtalkKeywordHelp', '如果钉钉群机器人开启了“自定义关键词”安全校验，填写这里。系统会自动把关键词加到发送内容前面，例如填写 AOS 后消息会以 [AOS] 开头。')}
        >
          <Input placeholder={t('botAgents.dingtalkKeywordPlaceholder', '例如：AOS')} />
        </Form.Item>
      </Card>
    </Space>
  );

  const renderTelegramSettings = () => (
    <Card size="small" title={t('botAgents.telegramSection', 'Telegram Bot 配置')} style={{ marginBottom: 16 }}>
      <Alert
        showIcon
        type="info"
        style={{ marginBottom: 16 }}
        message={t('botAgents.telegramNotice', '自动模式会通过 getUpdates 轮询接收消息；默认 Chat ID 用于测试发送和主动通知。')}
      />
      <Row gutter={12}>
        <Col xs={24} md={12}>
          <Form.Item
            name="telegram_bot_token"
            label={t('botAgents.telegramBotToken', 'Bot Token')}
          >
            <Input.Password placeholder={secretPlaceholder(editingChannel?.outbound_token_set)} />
          </Form.Item>
        </Col>
        <Col xs={24} md={12}>
          <Form.Item
            name="telegram_default_chat_id"
            label={t('botAgents.telegramDefaultChatId', '默认 Chat ID（可选）')}
            extra={t('botAgents.telegramDefaultChatIdHelp', '没有入站会话上下文时，测试发送和主动通知会发到这个 Chat ID。')}
          >
            <Input placeholder="-1001234567890" />
          </Form.Item>
        </Col>
      </Row>
    </Card>
  );

  const renderPlatformSettings = () => {
    switch (selectedChannelPlatform) {
      case 'dingtalk':
        return renderDingTalkSettings();
      case 'telegram':
        return renderTelegramSettings();
      case 'feishu':
      case 'lark':
        return renderFeishuSettings();
      case 'wecom':
        return renderWeComSettings();
      case 'slack':
        return renderSlackSettings();
      case 'whatsapp':
        return renderWhatsAppSettings();
      case 'discord':
        return renderDiscordSettings();
      default:
        return renderGenericWebhookSettings();
    }
  };

  const agentColumns: ColumnsType<BotAgentInfo> = [
    {
      title: t('common.name'),
      dataIndex: 'name',
      render: (_, row) => (
        <Space direction="vertical" size={2}>
          <Space>
            <RobotOutlined style={{ color: 'var(--accent-ai)' }} />
            <Button type="link" style={{ padding: 0 }} onClick={() => setDetailAgent(row)}>{row.name}</Button>
            <Tag color={row.enabled ? 'green' : 'default'}>{row.enabled ? t('common.enabled') : t('common.disabled')}</Tag>
          </Space>
          {row.description && <Text type="secondary" style={{ fontSize: 12 }}>{row.description}</Text>}
        </Space>
      ),
    },
    {
      title: t('botAgents.capabilities', '能力'),
      dataIndex: 'capabilities',
      render: (items: BotAgentInfo['capabilities']) => (
        <Space wrap>
          {items.map((item) => {
            const prefixes = stringArrayFromConfig(item.config_json, 'trigger_prefixes');
            const requireMention = boolFromConfig(item.config_json, 'require_mention');
            return (
              <Tag key={item.capability_key}>
                {capabilityLabel(item.capability_key)}
                {prefixes.length ? ` · ${t('botAgents.prefixShort', '前缀')}: ${prefixes.join('/')}` : ''}
                {requireMention ? ` · ${t('botAgents.requireMentionShort', '需@')}` : ''}
              </Tag>
            );
          })}
        </Space>
      ),
    },
    {
      title: t('botAgents.channels', '通道'),
      dataIndex: 'channels_count',
      width: 90,
      render: (value: number) => <Tag color="blue">{value}</Tag>,
    },
    {
      title: t('common.updatedAt'),
      dataIndex: 'updated_at',
      width: 180,
      render: (value: string) => new Date(value).toLocaleString(),
    },
    {
      title: t('common.actions'),
      width: 180,
      render: (_, row) => (
        <Space>
          <Button size="small" icon={<EditOutlined />} disabled={!canWrite} onClick={() => openAgentModal(row)}>{t('common.edit')}</Button>
          <Popconfirm title={t('common.deleteConfirm')} disabled={!canDelete} onConfirm={() => deleteAgentMutation.mutate(row.id)}>
            <Button size="small" danger icon={<DeleteOutlined />} disabled={!canDelete}>{t('common.delete')}</Button>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const channelColumns: ColumnsType<BotAgentChannelInfo> = [
    { title: t('common.name'), dataIndex: 'name' },
    { title: t('botAgents.agent', '机器人'), dataIndex: 'agent_id', render: (id: string) => agentNameMap.get(id) ?? id },
    { title: t('botAgents.platform', '平台'), dataIndex: 'platform', render: (v: string) => <Tag>{platformLabel(v)}</Tag> },
    { title: t('common.status'), dataIndex: 'enabled', render: (v: boolean) => <Tag color={v ? 'green' : 'default'}>{v ? t('common.enabled') : t('common.disabled')}</Tag> },
    {
      title: t('botAgents.inboundRuntime', '入站运行'),
      render: (_, row) => (
        <Space direction="vertical" size={2}>
          <Space size={4} wrap>
            <Tag color="blue">{inboundModeLabel(row.inbound_mode)}</Tag>
            <Tag color={inboundStatusColor(row.inbound_status)}>
              {statusLabel(row.inbound_status || 'idle')}
            </Tag>
          </Space>
          {row.inbound_error ? (
            <Text type="danger" style={{ fontSize: 12 }}>{row.inbound_error}</Text>
          ) : (
            <Text type="secondary" style={{ fontSize: 12 }}>
              {row.inbound_last_seen_at
                ? `${t('botAgents.lastSeenAt', '最近检查')}: ${new Date(row.inbound_last_seen_at).toLocaleString()}`
                : t('botAgents.notCheckedYet', '尚未检查')}
            </Text>
          )}
        </Space>
      ),
    },
    {
      title: t('botAgents.inboundWebhook', '入站回调 URL'),
      width: 150,
      render: (_, row) => webhookIngressEnabled(row.platform, row.inbound_mode) ? (
        <Button
          size="small"
          icon={<LinkOutlined />}
          onClick={() => {
            navigator.clipboard.writeText(makeWebhookUrl(row.id));
            message.success(t('common.copySuccess'));
          }}
        >
          {t('botAgents.copyInboundWebhook', '复制入站 URL')}
        </Button>
      ) : <Text type="secondary">{t('botAgents.notApplicable', '不适用')}</Text>,
    },
    {
      title: t('common.actions'),
      width: 350,
      render: (_, row) => (
        <Space>
          <Button
            size="small"
            icon={<SendOutlined />}
            disabled={!canWrite}
            loading={testingChannelId === row.id && testChannelMutation.isPending}
            onClick={() => {
              setTestingChannelId(row.id);
              testChannelMutation.mutate({
                id: row.id,
                title: t('botAgents.testMessageTitle', 'AOS Bot 网关测试'),
                text: t('botAgents.testMessageText', '这是一条来自 AOS Bot 网关的测试通知。'),
              });
            }}
          >
            {t('botAgents.testSend', '测试发送')}
          </Button>
          <Button
            size="small"
            icon={<LinkOutlined />}
            disabled={!row.enabled}
            onClick={() => openIdentityPairing(row)}
          >
            {t('botAgents.bindIdentity', '绑定身份')}
          </Button>
          <Button size="small" disabled={!canWrite} onClick={() => openChannelModal(row)}>{t('common.edit')}</Button>
          <Popconfirm title={t('common.deleteConfirm')} disabled={!canDelete} onConfirm={() => deleteChannelMutation.mutate(row.id)}>
            <Button size="small" danger disabled={!canDelete}>{t('common.delete')}</Button>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const logColumns: ColumnsType<BotMessageLogInfo> = [
    { title: t('botAgents.direction', '方向'), dataIndex: 'direction', width: 110, render: (v: string) => <Tag>{directionLabel(v)}</Tag> },
    { title: t('botAgents.platform', '平台'), dataIndex: 'platform', width: 130, render: (v: string) => platformLabel(v) },
    {
      title: t('botAgents.agent', '机器人'),
      dataIndex: 'agent_id',
      width: 180,
      render: (id?: string) => (
        <Text ellipsis={{ tooltip: id }} style={{ maxWidth: 160 }}>
          {id ? (agentNameMap.get(id) ?? id) : '-'}
        </Text>
      ),
    },
    {
      title: t('botAgents.channel', '通道'),
      dataIndex: 'channel_id',
      width: 180,
      render: (id?: string) => (
        <Text ellipsis={{ tooltip: id }} style={{ maxWidth: 160 }}>
          {id ? (channelNameMap.get(id) ?? id) : '-'}
        </Text>
      ),
    },
    {
      title: t('botAgents.agentTask', 'Agent 任务'),
      dataIndex: 'agent_task_id',
      width: 150,
      render: (id?: string | null) => id ? (
        <Button
          size="small"
          type="link"
          icon={<ExportOutlined />}
          href={`/tasks?task=${encodeURIComponent(id)}`}
          style={{ paddingInline: 0, whiteSpace: 'nowrap' }}
        >
          {t('botAgents.openWatchdog', '查看任务')}
        </Button>
      ) : '-',
    },
    { title: t('common.status'), dataIndex: 'status', width: 130, render: (v: string) => <Tag>{statusLabel(v)}</Tag> },
    {
      title: t('botAgents.queueStatus', '队列状态'),
      dataIndex: 'queue_status',
      width: 130,
      render: (value: string) => <Tag color={queueStatusColor(value)}>{queueStatusLabel(value)}</Tag>,
    },
    {
      title: t('botAgents.queueAttempts', '尝试'),
      width: 90,
      render: (_, row) => {
        const attempts = Number.isFinite(row.attempt_count) ? row.attempt_count : 0;
        const maxAttempts = Number.isFinite(row.max_attempts) && row.max_attempts > 0 ? row.max_attempts : 3;
        return `${attempts}/${maxAttempts}`;
      },
    },
    {
      title: t('botAgents.lastError', '最近错误'),
      dataIndex: 'last_error',
      width: 520,
      render: (value?: string | null, row?: BotMessageLogInfo) => {
        const error = value || row?.error_message;
        return error ? (
          <Paragraph
            type="danger"
            ellipsis={{ rows: 2, tooltip: error }}
            style={{
              margin: 0,
              maxWidth: 520,
              whiteSpace: 'normal',
              overflowWrap: 'anywhere',
              wordBreak: 'break-word',
            }}
          >
            {error}
          </Paragraph>
        ) : '-';
      },
    },
    {
      title: t('botAgents.queueFinishedAt', '完成时间'),
      dataIndex: 'finished_at',
      width: 180,
      render: (value?: string | null) => value ? new Date(value).toLocaleString() : '-',
    },
    { title: t('common.createdAt'), dataIndex: 'created_at', width: 180, render: (value: string) => new Date(value).toLocaleString() },
  ];

  return (
    <div style={{ padding: 24 }}>
      <Space direction="vertical" size={18} style={{ width: '100%' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', gap: 16, alignItems: 'flex-start' }}>
          <div>
            <Title level={3} style={{ marginBottom: 4 }}>{t('botAgents.title', 'Bot 网关')}</Title>
            <Text type="secondary">
              {t('botAgents.subtitle', '把 AOS 的产运、数分、素材和研发能力绑定到可配置机器人，通过外部聊天平台触发和接收通知。')}
            </Text>
          </div>
          <Space>
            <Button icon={<ApiOutlined />} disabled={!canWrite || agents.length === 0} onClick={() => openChannelModal()}>
              {t('botAgents.newChannel', '新建通道')}
            </Button>
            <Button type="primary" icon={<PlusOutlined />} disabled={!canWrite} onClick={() => openAgentModal()}>
              {t('botAgents.newAgent', '新建机器人')}
            </Button>
          </Space>
        </div>

        <Row gutter={16}>
          <Col xs={24} md={8}><Card><Space><RobotOutlined /><Text>{t('botAgents.agentCount', '机器人')}</Text><Title level={4} style={{ margin: 0 }}>{agents.length}</Title></Space></Card></Col>
          <Col xs={24} md={8}><Card><Space><ApiOutlined /><Text>{t('botAgents.channelCount', '通道')}</Text><Title level={4} style={{ margin: 0 }}>{channels.length}</Title></Space></Card></Col>
          <Col xs={24} md={8}><Card><Space><LinkOutlined /><Text>{t('botAgents.logCount', '最近消息')}</Text><Title level={4} style={{ margin: 0 }}>{logsQuery.data?.items.length ?? 0}</Title></Space></Card></Col>
        </Row>

        <Card>
          <Tabs
            activeKey={activeTab}
            onChange={setActiveTab}
            items={[
              {
                key: 'agents',
                label: t('botAgents.agents', '机器人'),
                children: <Table rowKey="id" loading={agentsQuery.isLoading} dataSource={agents} columns={agentColumns} pagination={{ pageSize: 10 }} />,
              },
              {
                key: 'channels',
                label: t('botAgents.channels', '通道'),
                children: <Table rowKey="id" loading={channelsQuery.isLoading} dataSource={channels} columns={channelColumns} pagination={{ pageSize: 10 }} scroll={{ x: 'max-content' }} />,
              },
              {
                key: 'identities',
                label: t('botAgents.identityBinding', '身份绑定'),
                children: (
                  <Space direction="vertical" size={18} style={{ width: '100%' }}>
                    <Alert
                      showIcon
                      type="info"
                      message={t('botAgents.identityBindingTitle', '绑定你的社交账号')}
                      description={t(
                        'botAgents.identityBindingHelp',
                        '绑定后，AOS 会使用当前登录用户的权限、会话和任务范围处理该社交账号的消息。每个用户只需为自己的账号绑定一次，不需要配置值守规则。',
                      )}
                    />
                    <Space wrap>
                      <Select
                        value={pairingChannelId}
                        onChange={(value) => {
                          setPairingChannelId(value);
                          setPairCode(null);
                        }}
                        placeholder={t('botAgents.selectPairingChannel', '请选择要绑定的通道')}
                        style={{ width: 320, maxWidth: '100%' }}
                        options={channels.map((channel) => ({
                          value: channel.id,
                          disabled: !channel.enabled,
                          label: `${channel.name} · ${platformLabel(channel.platform)}`,
                        }))}
                      />
                      <Button
                        type="primary"
                        icon={<LinkOutlined />}
                        disabled={!pairingChannelId}
                        loading={pairingMutation.isPending}
                        onClick={() => pairingChannelId && pairingMutation.mutate(pairingChannelId)}
                      >
                        {t('botAgents.createPairingCode', '生成绑定码')}
                      </Button>
                    </Space>
                    {channels.length === 0 ? (
                      <Alert
                        showIcon
                        type="warning"
                        message={t('botAgents.identityRequiresChannel', '请先让管理员创建并启用一个 Bot 通道')}
                      />
                    ) : null}
                    {pairCode ? (
                      <Alert
                        showIcon
                        type="success"
                        message={(
                          <Text code copyable>
                            {t('botAgents.pairingCommand', {
                              code: pairCode.code,
                              defaultValue: `绑定 ${pairCode.code}`,
                            })}
                          </Text>
                        )}
                        description={t('botAgents.pairingCodeHelp', {
                          channel: channelNameMap.get(pairingChannelId ?? '') ?? '-',
                          seconds: pairCode.expiresInSeconds,
                          defaultValue: '请在 {{seconds}} 秒内打开“{{channel}}”对应的 Bot 私聊，发送上面的完整命令。绑定成功后列表会自动刷新。',
                        })}
                      />
                    ) : null}
                    <Title level={5} style={{ margin: 0 }}>
                      {t('botAgents.boundIdentities', '已绑定账号')}
                    </Title>
                    <List
                      loading={identitiesQuery.isLoading}
                      dataSource={identitiesQuery.data?.items ?? []}
                      locale={{ emptyText: t('botAgents.noBoundIdentities', '尚未绑定任何社交账号') }}
                      renderItem={(identity) => (
                        <List.Item
                          actions={[
                            <Popconfirm
                              key="revoke"
                              title={t('botAgents.revokeIdentityConfirm', '解除后，该社交账号将无法继续访问你的 AOS 任务，确定解除吗？')}
                              onConfirm={() => revokeIdentityMutation.mutate(identity.id)}
                            >
                              <Button
                                type="text"
                                danger
                                icon={<DisconnectOutlined />}
                                aria-label={t('botAgents.revokeIdentity', '解除绑定')}
                              >
                                {t('botAgents.revokeIdentity', '解除绑定')}
                              </Button>
                            </Popconfirm>,
                          ]}
                        >
                          <List.Item.Meta
                            title={(
                              <Space wrap>
                                <Tag>{platformLabel(identity.platform)}</Tag>
                                <Text>{identity.displayName ?? identity.externalUserId}</Text>
                              </Space>
                            )}
                            description={(
                              <Space wrap split={<Text type="secondary">·</Text>}>
                                <Text type="secondary">
                                  {channelNameMap.get(identity.channelId ?? '') ?? t('botAgents.unknownChannel', '未知通道')}
                                </Text>
                                <Text type="secondary">
                                  {t('botAgents.boundAt', '绑定时间')}: {formatBotTimestamp(identity.verifiedAt)}
                                </Text>
                                <Text type="secondary">
                                  {t('botAgents.lastActiveAt', '最近活动')}: {formatBotTimestamp(identity.lastSeenAt)}
                                </Text>
                              </Space>
                            )}
                          />
                        </List.Item>
                      )}
                    />
                  </Space>
                ),
              },
              {
                key: 'logs',
                label: t('botAgents.messageLogs', '消息日志'),
                children: (
                  <Table
                    rowKey="id"
                    loading={logsQuery.isLoading}
                    dataSource={logsQuery.data?.items ?? []}
                    columns={logColumns}
                    pagination={{ pageSize: 10 }}
                    scroll={{ x: 'max-content' }}
                  />
                ),
              },
            ]}
          />
        </Card>
      </Space>

      <Modal
        open={agentModalOpen}
        title={editingAgent ? t('botAgents.editAgent', '编辑机器人') : t('botAgents.newAgent', '新建机器人')}
        onCancel={() => setAgentModalOpen(false)}
        onOk={() => agentForm.submit()}
        confirmLoading={saveAgentMutation.isPending}
      >
        <Form layout="vertical" form={agentForm} onFinish={(values) => saveAgentMutation.mutate(values)}>
          <Alert
            showIcon
            type="info"
            style={{ marginBottom: 16 }}
            message={t('botAgents.taskControlTitle', '任务查询与控制已内置')}
            description={t('botAgents.taskControlHelp', '完成身份绑定后，可直接用自然语言查询进度、失败原因、卡住位置，或取消刚才的任务；这套能力在路由前统一处理，不需要额外绑定“看门狗”。')}
          />
          <Form.Item name="name" label={t('common.name')} rules={[{ required: true }]}><Input /></Form.Item>
          <Form.Item name="description" label={t('common.description')}><Input.TextArea rows={2} /></Form.Item>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked"><Switch /></Form.Item>
          <Form.List name="capabilities">
            {(fields, { add, remove }) => (
              <Form.Item
                label={t('botAgents.boundCapabilities', '绑定能力')}
                extra={t('botAgents.boundCapabilitiesHelp', '给机器人绑定可调用能力并配置触发方式。前缀命中优先；未命中前缀时自动进入超级助手。')}
                required
              >
                <Space direction="vertical" style={{ width: '100%' }}>
                  {fields.map(({ key, ...field }) => {
                    const capabilityValue = watchedCapabilities[field.name]?.capability_key;
                    const diagnostics = capabilityConfigWarnings(watchedCapabilities[field.name], t);
                    return (
                    <Card key={key} size="small">
                      {diagnostics.length > 0 && (
                        <Alert
                          type="warning"
                          showIcon
                          style={{ marginBottom: 12 }}
                          message={t('botAgents.configDiagnostics', '配置诊断')}
                          description={(
                            <Space direction="vertical" size={2}>
                              {diagnostics.map((item) => (
                                <Text key={item} type="secondary">{item}</Text>
                              ))}
                            </Space>
                          )}
                        />
                      )}
                      <Row gutter={12}>
                        <Col xs={24} md={10}>
                          <Form.Item
                            {...field}
                            name={[field.name, 'capability_key']}
                            label={t('botAgents.capabilityKey', '能力')}
                            rules={[{ required: true }]}
                          >
                            <Select options={capabilityOptions} />
                          </Form.Item>
                          {agentForm.getFieldValue(['capabilities', field.name, 'capability_key']) === 'aos_router' && (
                            <Alert
                              type="info"
                              showIcon
                              style={{ marginBottom: 12 }}
                              message={t('botAgents.routerHint', '超级助手是默认入口：用户无需记前缀，WebUI 与 IM 复用同一套会话、上下文、任务和产物。')}
                            />
                          )}
                        </Col>
                        <Col xs={24} md={14}>
                          <Form.Item
                            {...field}
                            name={[field.name, 'trigger_prefixes']}
                            label={t('botAgents.triggerPrefixes', '触发前缀')}
                          >
                            <Input placeholder={t('botAgents.triggerPrefixesPlaceholder', '例如：cy, 产运；多个前缀用逗号或空格分隔')} />
                          </Form.Item>
                        </Col>
                        <Col xs={24} md={12}>
                          <Form.Item {...field} name={[field.name, 'require_mention']} valuePropName="checked">
                            <Switch checkedChildren={t('botAgents.requireMentionShort', '需@')} unCheckedChildren={t('common.no', '否')} />
                          </Form.Item>
                        </Col>
                        <Col xs={24} md={12} style={{ textAlign: 'right' }}>
                          <Button danger disabled={fields.length <= 1} onClick={() => remove(field.name)}>
                            {t('common.delete')}
                          </Button>
                        </Col>
                        <Col xs={24}>
                          <Collapse
                            size="small"
                            ghost
                            items={[{
                              key: 'advanced',
                              label: t('botAgents.capabilityAdvanced', '高级配置'),
                              children: (
                                <Row gutter={12}>
                                  <Col xs={24} md={8}>
                                    <Form.Item {...field} name={[field.name, 'executionMode']} label={t('botAgents.executionMode', '执行模式')}>
                                      <Select
                                        allowClear
                                        placeholder={t('botAgents.executionModeAuto', '自动')}
                                        options={[
                                          { label: t('botAgents.executionModeHybrid', '混合'), value: 'hybrid' },
                                          { label: t('botAgents.executionModeSync', '同步'), value: 'sync' },
                                          { label: t('botAgents.executionModeAsync', '异步'), value: 'async' },
                                          { label: t('botAgents.executionModeClarification', '多轮澄清'), value: 'clarification' },
                                        ]}
                                      />
                                    </Form.Item>
                                  </Col>
                                  <Col xs={24} md={8}>
                                    <Form.Item {...field} name={[field.name, 'syncTimeoutMs']} label={t('botAgents.syncTimeoutMs', '同步超时 ms')}>
                                      <Input type="number" min={1000} max={120000} placeholder={t('botAgents.syncTimeoutMsPlaceholder', '默认按能力策略')} />
                                    </Form.Item>
                                  </Col>
                                  <Col xs={24} md={8}>
                                    <Form.Item {...field} name={[field.name, 'ackTimeoutMs']} label={t('botAgents.ackTimeoutMs', '回执超时 ms')}>
                                      <Input type="number" min={0} max={30000} placeholder={t('botAgents.ackTimeoutMsPlaceholder', '默认按能力策略')} />
                                    </Form.Item>
                                  </Col>
                                  {capabilityValue === 'rd_agent' && (
                                    <Col xs={24} md={12}>
                                      <Form.Item {...field} name={[field.name, 'model']} label={t('botAgents.model', '模型')}>
                                        <Input placeholder={t('botAgents.modelPlaceholder', '可选；为空使用菜单默认模型')} />
                                      </Form.Item>
                                    </Col>
                                  )}
                                  {capabilityValue === 'super_adversarial' && (
                                    <>
                                      <Col xs={24} md={14}>
                                        <Form.Item {...field} name={[field.name, 'models']} label={t('botAgents.models', '多模型')}>
                                          <Input placeholder={t('botAgents.modelsPlaceholder', '超级对抗使用，多个模型用逗号分隔')} />
                                        </Form.Item>
                                      </Col>
                                      <Col xs={24} md={10}>
                                        <Form.Item {...field} name={[field.name, 'maxRounds']} label={t('botAgents.maxRounds', '最大轮次')}>
                                          <Input type="number" min={1} max={8} />
                                        </Form.Item>
                                      </Col>
                                    </>
                                  )}
                                  {capabilityValue === 'rd_agent' && (
                                    <>
                                      <Col xs={24} md={16}>
                                        <Form.Item {...field} name={[field.name, 'repositoryId']} label={t('botAgents.repositoryId', '仓库 ID')}>
                                          <Input placeholder={t('botAgents.repositoryIdPlaceholder', 'RD Agent 使用')} />
                                        </Form.Item>
                                      </Col>
                                      <Col xs={24} md={12}>
                                        <Form.Item {...field} name={[field.name, 'agentProfileId']} label={t('botAgents.agentProfileId', 'Agent Profile ID')}>
                                          <Input placeholder={t('botAgents.agentProfileIdPlaceholder', 'RD Agent 使用')} />
                                        </Form.Item>
                                      </Col>
                                      <Col xs={24} md={12}>
                                        <Form.Item {...field} name={[field.name, 'workflowId']} label={t('botAgents.workflowId', 'Workflow ID')}>
                                          <Input placeholder={t('botAgents.workflowIdPlaceholder', 'RD Agent 使用')} />
                                        </Form.Item>
                                      </Col>
                                      <Col xs={24} md={12}>
                                        <Form.Item {...field} name={[field.name, 'contextDepth']} label={t('botAgents.contextDepth', '上下文深度')}>
                                          <Select
                                            allowClear
                                            options={[
                                              { label: t('botAgents.contextDepthStandard', '标准'), value: 'standard' },
                                              { label: t('botAgents.contextDepthDeep', '深度'), value: 'deep' },
                                            ]}
                                          />
                                        </Form.Item>
                                      </Col>
                                      <Col xs={24} md={12}>
                                        <Form.Item {...field} name={[field.name, 'shouldDeepScan']} valuePropName="checked">
                                          <Switch checkedChildren={t('botAgents.shouldDeepScan', '深度扫描')} unCheckedChildren={t('common.no', '否')} />
                                        </Form.Item>
                                      </Col>
                                    </>
                                  )}
                                  {capabilityValue === 'nl2sql' && (
                                    <>
                                      <Col xs={24} md={14}>
                                        <Form.Item {...field} name={[field.name, 'dataSourceId']} label={t('botAgents.dataSourceId', '数据源 ID')}>
                                          <Input placeholder={t('botAgents.dataSourceIdPlaceholder', 'NL2SQL 使用')} />
                                        </Form.Item>
                                      </Col>
                                      <Col xs={24} md={10}>
                                        <Form.Item {...field} name={[field.name, 'allowExecuteSql']} valuePropName="checked">
                                          <Switch checkedChildren={t('botAgents.allowExecuteSql', '允许执行 SQL')} unCheckedChildren={t('botAgents.generateSqlOnly', '只生成 SQL')} />
                                        </Form.Item>
                                      </Col>
                                    </>
                                  )}
                                  {capabilityValue === 'aos_router' && (
                                    <Col xs={24} md={12}>
                                      <Form.Item {...field} name={[field.name, 'confidenceThreshold']} label={t('botAgents.confidenceThreshold', '路由置信度阈值')}>
                                        <Input type="number" min={0.5} max={0.99} step={0.01} placeholder={t('botAgents.confidenceThresholdPlaceholder', '默认 0.80')} />
                                      </Form.Item>
                                    </Col>
                                  )}
                                  {!capabilityValue && (
                                    <Col xs={24}>
                                      <Text type="secondary">{t('botAgents.selectCapabilityForAdvanced', '选择能力后显示对应高级配置。')}</Text>
                                    </Col>
                                  )}
                                </Row>
                              ),
                            }]}
                          />
                        </Col>
                      </Row>
                    </Card>
                    );
                  })}
                  <Button
                    type="dashed"
                    icon={<PlusOutlined />}
                    onClick={() => add({ require_mention: true })}
                    block
                  >
                    {t('botAgents.addCapability', '添加绑定能力')}
                  </Button>
                </Space>
              </Form.Item>
            )}
          </Form.List>
          <Form.Item name="persona_prompt" label={t('botAgents.personaPrompt', '机器人提示词')}>
            <Input.TextArea rows={4} placeholder={t('botAgents.personaPromptPlaceholder', '可选：定义机器人语气、业务边界、默认回复格式等')} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        open={channelModalOpen}
        title={editingChannel ? t('botAgents.editChannel', '编辑通道') : t('botAgents.newChannel', '新建通道')}
        onCancel={() => setChannelModalOpen(false)}
        onOk={() => channelForm.submit()}
        confirmLoading={saveChannelMutation.isPending}
        width={720}
      >
        <Form layout="vertical" form={channelForm} onFinish={(values) => saveChannelMutation.mutate(values)}>
          <Form.Item name="agent_id" label={t('botAgents.agent', '机器人')} rules={[{ required: true }]}>
            <Select options={agents.map((agent) => ({ label: agent.name, value: agent.id }))} />
          </Form.Item>
          <Form.Item
            name="platform"
            label={t('botAgents.platform', '平台')}
            rules={[{ required: true }]}
            extra={PLATFORM_GUIDE_URLS[selectedChannelPlatform || ''] ? (
              <Button
                type="link"
                size="small"
                icon={<BookOutlined />}
                href={PLATFORM_GUIDE_URLS[selectedChannelPlatform || '']}
                target="_blank"
                rel="noreferrer"
                style={{ paddingInline: 0 }}
              >
                {t('botAgents.platformSetupGuide', '官方配置手册')}
              </Button>
            ) : t('botAgents.genericPlatformGuide', '通用 Webhook 没有第三方平台手册，请按下方字段与接收端约定配置。')}
          >
            <Select options={platformOptions} />
          </Form.Item>
          <Form.Item name="name" label={t('common.name')} rules={[{ required: true }]}><Input /></Form.Item>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked"><Switch /></Form.Item>
          <Form.Item
            name="inbound_mode"
            label={t('botAgents.inboundMode', '入站模式')}
            extra={t('botAgents.inboundModeHelp', '自动模式会选择该平台的本地长连接或轮询；WhatsApp 和通用平台仅支持 Webhook。入站凭证不一定等同于测试发送凭证。')}
          >
            <Select options={inboundModeOptions} />
          </Form.Item>
          <Alert
            showIcon
            type="info"
            style={{ marginBottom: 16 }}
            message={platformInboundHelp(selectedChannelPlatform)}
          />
          {editingChannel && webhookIngressEnabled(selectedChannelPlatform, selectedInboundMode) && (
            <Form.Item
              label={t('botAgents.inboundWebhook', '入站回调 URL')}
              extra={t('botAgents.inboundWebhookHelp', '把这个 URL 配置到外部平台消息回调/事件订阅。支持原生事件解析的平台会自动提取正文、会话和发送者；若设置入口 Secret，请在 URL 后追加 ?secret=你的入口Secret。')}
            >
              <Input.Group compact>
                <Input readOnly value={makeWebhookUrl(editingChannel.id)} style={{ width: 'calc(100% - 96px)' }} />
                <Button
                  icon={<LinkOutlined />}
                  onClick={() => {
                    navigator.clipboard.writeText(makeWebhookUrl(editingChannel.id));
                    message.success(t('common.copySuccess'));
                  }}
                >
                  {t('common.copy')}
                </Button>
              </Input.Group>
            </Form.Item>
          )}
          {!editingChannel && webhookIngressEnabled(selectedChannelPlatform, selectedInboundMode) && (
            <Alert
              showIcon
              type="info"
              style={{ marginBottom: 16 }}
              message={t('botAgents.inboundWebhookAfterSave', '保存通道后会生成可复制的入站回调 URL。')}
            />
          )}

          {webhookIngressEnabled(selectedChannelPlatform, selectedInboundMode) && (
            <Form.Item
              name="inbound_secret"
              label={t('botAgents.inboundSecret', '入口 Secret')}
              extra={t('botAgents.inboundSecretHelp', '用于入站 Webhook 鉴权；长连接或轮询入站不需要填写。')}
            >
              <Input.Password placeholder={secretPlaceholder(editingChannel?.inbound_secret_set)} />
            </Form.Item>
          )}

          {renderPlatformSettings()}

          <Form.Item
            name="notify_on_events"
            label={t('botAgents.autoNotifyEvents', '自动通知事件')}
            extra={t('botAgents.autoNotifyEventsHelp', '选择后，该通道会在标准任务事件发生时向已绑定的私聊会话发送通知。')}
          >
            <Select mode="multiple" allowClear options={notificationEventOptions} />
          </Form.Item>
          <Descriptions
            size="small"
            column={1}
            bordered
            style={{ marginBottom: 16 }}
            items={NOTIFICATION_EVENT_OPTIONS.map((item) => ({
              key: item.value,
              label: t(item.labelKey, item.fallback),
              children: t(item.descriptionKey),
            }))}
          />
          <Alert
            showIcon
            type="info"
            style={{ marginBottom: 16 }}
            message={t('botAgents.notificationDeliveryTitle', '通知投递前提')}
            description={t('botAgents.notificationDeliveryHelp', '需要先完成社交账号身份绑定，并在该通道产生可验证的私聊会话。自动通知只发送已勾选事件；手机端任务查询、追问和取消不受勾选项限制。')}
          />

          <Collapse
            bordered={false}
            size="small"
            style={{ background: 'transparent', marginBottom: 8 }}
            items={[{
              key: 'advanced',
              label: (
                <Space>
                  {t('botAgents.advancedConfigOptional', '高级配置（可选）')}
                  <Button
                    type="text"
                    size="small"
                    shape="circle"
                    icon={<QuestionCircleOutlined />}
                    aria-label={t('botAgents.advancedConfigGuide', '查看高级配置说明')}
                    onClick={(event) => {
                      event.stopPropagation();
                      setAdvancedHelpOpen(true);
                    }}
                  />
                </Space>
              ),
              children: (
                <Form.Item
                  name="config_json"
                  label={t('botAgents.advancedConfig', '高级配置 JSON')}
                  extra={t('botAgents.advancedConfigHelp', '可选。仅用于少数平台扩展参数，例如自定义请求头、消息格式、mention 策略或 payload 模板；普通配置请优先使用上方表单。')}
                >
                  <Input.TextArea
                    rows={5}
                    placeholder={t('botAgents.advancedConfigPlaceholder', '{\n  "messageType": "markdown",\n  "mentionAll": false,\n  "headers": {}\n}')}
                  />
                </Form.Item>
              ),
            }]}
          />
        </Form>
      </Modal>

      <Modal
        open={advancedHelpOpen}
        title={t('botAgents.advancedConfigGuide', '高级配置说明')}
        footer={null}
        width={720}
        onCancel={() => setAdvancedHelpOpen(false)}
      >
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          <Alert
            showIcon
            type="warning"
            message={t('botAgents.advancedConfigSecretWarning', '不要在高级 JSON 中填写 Token、Secret 或密码；凭证只使用上方专用密码字段。')}
          />
          <Paragraph style={{ marginBottom: 0 }}>
            {t(
              `botAgents.advancedConfigPlatformHelp.${selectedChannelPlatform || 'generic_webhook'}`,
              t('botAgents.advancedConfigPlatformHelp.generic_webhook'),
            )}
          </Paragraph>
          <Text strong>{t('botAgents.advancedConfigExample', '当前平台示例')}</Text>
          <pre style={{ margin: 0, padding: 12, overflow: 'auto', maxHeight: 360, background: 'var(--surface-subtle)', border: '1px solid var(--border-color)', borderRadius: 6 }}>
            {JSON.stringify(
              ADVANCED_CONFIG_EXAMPLES[selectedChannelPlatform || 'generic_webhook']
                ?? ADVANCED_CONFIG_EXAMPLES.generic_webhook,
              null,
              2,
            )}
          </pre>
          <Text type="secondary">
            {t('botAgents.advancedConfigOverrideHelp', '这里的字段仅覆盖适配器高级默认值；页面已有的字段始终优先使用表单配置。')}
          </Text>
        </Space>
      </Modal>

      <Drawer open={!!detailAgent} onClose={() => setDetailAgent(null)} width={560} title={detailAgent?.name}>
        {detailAgent && (
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            <Descriptions column={1} bordered size="small">
              <Descriptions.Item label={t('common.status')}>{detailAgent.enabled ? t('common.enabled') : t('common.disabled')}</Descriptions.Item>
              <Descriptions.Item label={t('botAgents.boundCapabilities', '绑定能力')}>
                <Space wrap>
                  {detailAgent.capabilities.map((cap) => {
                    const prefixes = stringArrayFromConfig(cap.config_json, 'trigger_prefixes');
                    return (
                      <Tag key={cap.capability_key}>
                        {capabilityLabel(cap.capability_key)}
                        {prefixes.length ? ` · ${prefixes.join('/')}` : ''}
                      </Tag>
                    );
                  })}
                </Space>
              </Descriptions.Item>
              <Descriptions.Item label={t('botAgents.channels', '通道')}>{detailAgent.channels_count}</Descriptions.Item>
            </Descriptions>
            {detailAgent.persona_prompt && <Paragraph style={{ whiteSpace: 'pre-wrap' }}>{detailAgent.persona_prompt}</Paragraph>}
            <Button icon={<ApiOutlined />} disabled={!canWrite} onClick={() => openChannelModal()}>{t('botAgents.newChannel', '新建通道')}</Button>
          </Space>
        )}
      </Drawer>
    </div>
  );
}
