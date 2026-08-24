import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Card,
  Table,
  Tag,
  Typography,
  Button,
  Modal,
  Form,
  Input,
  Select,
  Space,
  Switch,
  Popconfirm,
  Tooltip,
  message,
  Drawer,
  Descriptions,
  Divider,
  Badge,
  Row,
  Col,
  Statistic,
  Progress,
  Spin,
  Alert,
  AutoComplete,
  Checkbox,
  InputNumber,
} from 'antd';
import {
  PlusOutlined,
  DeleteOutlined,
  KeyOutlined,
  GlobalOutlined,
  EditOutlined,
  ReloadOutlined,
  CheckCircleOutlined,
  StopOutlined,
  EyeOutlined,
  EyeInvisibleOutlined,
  BarChartOutlined,
  ExperimentOutlined,
  ThunderboltOutlined,
  SyncOutlined,
  SafetyCertificateOutlined,
  LinkOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { apiKeysApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { MOCK_API_KEYS } from '@/api/mock';
import { PageSkeleton } from '@/components/Skeleton';
import { usePermissions } from '@/store/permissions';
import type { ApiKeyRecord } from '@/types';

const { Title, Text, Paragraph } = Typography;

const PROVIDER_COLOR: Record<string, string> = {
  anthropic: 'orange',
  openai: 'green',
  deepseek: 'cyan',
  kimi: 'magenta',
  glm: 'gold',
  gemini: 'geekblue',
  xai: 'volcano',
  custom: 'blue',
};

const PROVIDER_BASE_URL_DEFAULTS: Record<string, string> = {
  deepseek: 'https://api.deepseek.com/v1',
  kimi: 'https://api.moonshot.cn/v1',
  glm: 'https://open.bigmodel.cn/api/paas/v4',
  gemini: 'https://generativelanguage.googleapis.com/v1beta/openai',
  xai: 'https://api.x.ai/v1',
  custom: '',
};

function isStructuredProbeFormatWarning(value: string): boolean {
  const normalized = value.toLowerCase();
  return normalized.includes('probe returned invalid structured content')
    || (normalized.includes('probe failed transiently') && normalized.includes('structured'));
}

const OPENAI_COMPATIBLE_PRESET_PROVIDERS = new Set(['deepseek', 'kimi', 'glm', 'gemini']);
const EDITABLE_BASE_URL_PRESET_PROVIDERS = new Set([
  ...OPENAI_COMPATIBLE_PRESET_PROVIDERS,
  'xai',
]);

const AUDIO_ENGINE_OPTIONS = [{ value: 'suno', label: 'Suno' }];

function stripAudioEnginePrefix(raw: string, engine: string): string {
  let current = raw.trim();
  const normalizedEngine = engine.trim().toLowerCase();
  if (!current || !normalizedEngine) return current;
  while (current) {
    const lowered = current.toLowerCase();
    if (lowered === normalizedEngine) return '';
    let consumed = false;
    for (const sep of ['/', ':', '|']) {
      const prefix = `${normalizedEngine}${sep}`;
      if (lowered.startsWith(prefix)) {
        current = current.slice(prefix.length).trim();
        consumed = true;
        break;
      }
    }
    if (!consumed) break;
  }
  return current;
}

function parseAudioModelDescriptor(raw?: string): { audioEngine: string; modelVersion: string } {
  const fallback = { audioEngine: 'suno', modelVersion: '' };
  if (!raw || !raw.trim()) return fallback;
  const trimmed = raw.trim();
  const lowered = trimmed.toLowerCase();
  if (lowered === 'suno' || lowered.startsWith('suno/') || lowered.startsWith('suno:') || lowered.startsWith('suno|')) {
    return { audioEngine: 'suno', modelVersion: stripAudioEnginePrefix(trimmed, 'suno') };
  }
  // Legacy/plain value: treat as Suno version.
  return { audioEngine: 'suno', modelVersion: trimmed };
}

function composeAudioModelDescriptor(audioEngine?: string, modelVersion?: string): string {
  const normalizedEngine = (audioEngine || 'suno').trim().toLowerCase();
  const normalizedVersion = stripAudioEnginePrefix((modelVersion || '').trim(), normalizedEngine);
  if (normalizedEngine !== 'suno') {
    return normalizedVersion || normalizedEngine;
  }
  if (!normalizedVersion) return 'suno';
  return `suno/${normalizedVersion}`;
}

const MODEL_TYPE_COLOR: Record<string, string> = {
  chat: 'blue',
  embedding: 'purple',
  image: 'magenta',
  video: 'cyan',
  audio: 'gold',
};

interface BaseUrlInputProps {
  visible: boolean;
  label: string;
  extra?: string;
  placeholder?: string;
}

function BaseUrlInput({ visible, label, extra, placeholder }: BaseUrlInputProps) {
  const { t } = useTranslation();

  if (!visible) return null;

  return (
    <Form.Item name="base_url" label={label} extra={extra} rules={[{ required: true, message: t('common.required') + ' ' + t('apikeys.baseUrl') }]}>
      <Input
        placeholder={placeholder || t('apikeys.baseUrlPlaceholder')}
        prefix={<GlobalOutlined />}
      />
    </Form.Item>
  );
}

interface ApiKeyFormValues {
  name: string;
  provider: string;
  base_url?: string;
  model?: string;
  dimensions?: number;
  audio_engine?: string;
  audio_generate_path?: string;
  audio_query_path?: string;
  model_type?: string;
  key_value: string;
  daily_limit?: number;
  monthly_limit?: number;
  enabled?: boolean;
  priority?: number;
  input_price_per_million?: number;
  output_price_per_million?: number;
  scenarios?: string[];
  supports_reasoning_effort?: boolean;
  reasoning_effort_default?: string;
  reasoning_transport?: string;
  reasoning_effort_values?: string[];
  reasoning_policy?: 'auto' | 'fast' | 'standard' | 'deep' | 'maximum';
  include_reasoning?: boolean;
  use_max_completion_tokens?: boolean;
  native_web_search_enabled?: boolean;
  native_web_search_extra_body?: string;
  native_web_search_tool_template?: string;
  context_window_tokens?: number | null;
  max_output_tokens?: number | null;
}

interface EditFormValues {
  name: string;
  provider?: string;
  base_url?: string;
  model?: string;
  dimensions?: number;
  audio_engine?: string;
  audio_generate_path?: string;
  audio_query_path?: string;
  model_type?: string;
  key_value?: string;
  daily_limit?: number | null;
  monthly_limit?: number | null;
  enabled: boolean;
  priority?: number | null;
  input_price_per_million?: number | null;
  output_price_per_million?: number | null;
  scenarios?: string[];
  supports_reasoning_effort?: boolean;
  reasoning_effort_default?: string;
  reasoning_transport?: string;
  reasoning_effort_values?: string[];
  reasoning_policy?: 'auto' | 'fast' | 'standard' | 'deep' | 'maximum';
  include_reasoning?: boolean;
  use_max_completion_tokens?: boolean;
  native_web_search_enabled?: boolean;
  native_web_search_extra_body?: string;
  native_web_search_tool_template?: string;
  context_window_tokens?: number | null;
  max_output_tokens?: number | null;
  expires_at?: string;
}

type ScenarioFilter = 'all' | 'chat' | 'nl2sql' | 'rd' | 'pm';

const SCENARIO_TAG_COLOR: Record<string, string> = {
  chat: 'geekblue',
  nl2sql: 'purple',
  rd: 'blue',
  agent: 'blue',
  pm: 'gold',
};

const ALL_SCENARIOS = ['chat', 'nl2sql', 'rd', 'pm'] as const;
const ALL_SCENARIO_VALUES = [...ALL_SCENARIOS];

function buildCapabilities(values: {
  supports_reasoning_effort?: boolean;
  reasoning_effort_default?: string;
  reasoning_transport?: string;
  reasoning_effort_values?: string[];
  reasoning_policy?: 'auto' | 'fast' | 'standard' | 'deep' | 'maximum';
  include_reasoning?: boolean;
  use_max_completion_tokens?: boolean;
  native_web_search_enabled?: boolean;
  native_web_search_extra_body?: string;
  native_web_search_tool_template?: string;
  context_window_tokens?: number | null;
  max_output_tokens?: number | null;
}) {
  const capabilities: NonNullable<ApiKeyRecord['capabilities_json']> = {};
  if (values.reasoning_policy && values.reasoning_policy !== 'auto') {
    capabilities.reasoningPolicy = values.reasoning_policy;
  }
  if (values.supports_reasoning_effort) {
    const supportedValues = values.reasoning_effort_values?.filter(Boolean) ?? [];
    const fallbackValues = supportedValues.length > 0 ? supportedValues : ['low', 'medium', 'high'];
    const middleIndex = Math.floor((fallbackValues.length - 1) / 2);
    capabilities.reasoningEffort = true;
    capabilities.reasoningTransport = values.reasoning_transport || 'reasoning_effort';
    capabilities.reasoningEffortValues = fallbackValues;
    capabilities.reasoningEffortDefault =
      values.reasoning_effort_default || fallbackValues[middleIndex] || 'high';
    capabilities.reasoningBudgetMap = {
      fast: fallbackValues[0],
      standard: fallbackValues[middleIndex],
      deep: fallbackValues[fallbackValues.length - 1],
    };
  }
  if (values.include_reasoning) {
    capabilities.includeReasoning = true;
  }
  if (values.use_max_completion_tokens) {
    capabilities.useMaxCompletionTokens = true;
  }
  if (values.native_web_search_enabled) {
    const nativeWebSearch: {
      enabled: boolean;
      extraBody?: Record<string, unknown>;
      toolTemplate?: Record<string, unknown>;
    } = {
      enabled: true,
    };
    const extraBody = parseJsonObjectField(values.native_web_search_extra_body, 'nativeWebSearch.extraBody');
    if (extraBody) {
      nativeWebSearch.extraBody = extraBody;
    }
    const toolTemplate = parseJsonObjectField(values.native_web_search_tool_template, 'nativeWebSearch.toolTemplate');
    if (toolTemplate) {
      nativeWebSearch.toolTemplate = toolTemplate;
    }
    capabilities.nativeWebSearch = nativeWebSearch;
  }
  if (values.context_window_tokens != null && Number(values.context_window_tokens) > 0) {
    capabilities.contextWindowTokens = Number(values.context_window_tokens);
  }
  if (values.max_output_tokens != null && Number(values.max_output_tokens) > 0) {
    capabilities.maxOutputTokens = Number(values.max_output_tokens);
  }
  return capabilities;
}

function parseJsonObjectField(raw: string | undefined, label: string): Record<string, unknown> | undefined {
  const trimmed = raw?.trim();
  if (!trimmed) return undefined;
  const parsed = JSON.parse(trimmed);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error(`${label} must be a JSON object`);
  }
  return parsed as Record<string, unknown>;
}

function stringifyJsonObject(value: unknown): string | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  return JSON.stringify(value, null, 2);
}

type CapabilityResolution = Awaited<ReturnType<typeof apiKeysApi.resolveModel>>;
type DiscoveredModel = Awaited<ReturnType<typeof apiKeysApi.discoverModels>>['models'][number];
type ModelOption = { value: string; label: string; model: DiscoveredModel };

function CapabilitySummary({
  resolution,
  loading,
  probing,
  onProbe,
}: {
  resolution?: CapabilityResolution;
  loading: boolean;
  probing: boolean;
  onProbe: () => void;
}) {
  const { t } = useTranslation();
  if (loading) {
    return <Spin size="small" />;
  }
  if (!resolution) {
    return (
      <Alert
        type="info"
        showIcon
        message={t('apikeys.capabilityAwaitingModel')}
      />
    );
  }
  const profile = resolution.profile;
  const reasoningValues = profile.reasoningEffortValues ?? [];
  const features = profile.features ?? {};
  const confidenceLabel = (() => {
    switch (resolution.confidence) {
      case 'high': return t('apikeys.capabilityConfidence.high');
      case 'medium': return t('apikeys.capabilityConfidence.medium');
      case 'low': return t('apikeys.capabilityConfidence.low');
      default: return resolution.confidence;
    }
  })();
  const sourceLabel = (() => {
    switch (resolution.source) {
      case 'built_in_registry': return t('apikeys.capabilitySource.built_in_registry');
      case 'provider_metadata': return t('apikeys.capabilitySource.provider_metadata');
      case 'probe': return t('apikeys.capabilitySource.probe');
      case 'conservative_fallback': return t('apikeys.capabilitySource.conservative_fallback');
      default: return resolution.source;
    }
  })();
  const capabilityItems = [
    [t('apikeys.contextWindowTokens'), profile.contextWindowTokens?.toLocaleString() ?? t('apikeys.capabilityUnknown')],
    [t('apikeys.maxOutputTokens'), profile.maxOutputTokens?.toLocaleString() ?? t('apikeys.capabilityUnknown')],
    [
      t('apikeys.reasoningLevels'),
      reasoningValues.length > 0
        ? <Space size={[4, 4]} wrap>{reasoningValues.map((value) => <Tag key={value}>{value}</Tag>)}</Space>
        : t('apikeys.capabilityNotDetected'),
    ],
    [t('apikeys.outputTokenParameter'), profile.outputTokenParameter ?? 'max_tokens'],
    [
      t('apikeys.toolCalling'),
      features.tools == null
        ? t('apikeys.capabilityUnknown')
        : features.tools ? t('apikeys.capabilitySupported') : t('apikeys.capabilityUnsupported'),
    ],
    [
      t('apikeys.structuredOutput'),
      features.structuredOutput == null
        ? t('apikeys.capabilityUnknown')
        : features.structuredOutput ? t('apikeys.capabilitySupported') : t('apikeys.capabilityUnsupported'),
    ],
    [
      t('apikeys.jsonObjectOutput'),
      features.jsonObject == null
        ? t('apikeys.capabilityUnknown')
        : features.jsonObject ? t('apikeys.capabilitySupported') : t('apikeys.capabilityUnsupported'),
    ],
    [
      t('apikeys.strictJsonSchemaOutput'),
      features.strictJsonSchema == null
        ? t('apikeys.capabilityUnknown')
        : features.strictJsonSchema ? t('apikeys.capabilitySupported') : t('apikeys.capabilityUnsupported'),
    ],
  ] as const;
  return (
    <div style={{ border: '1px solid #30363d', padding: 12, borderRadius: 6 }}>
      <Space wrap style={{ marginBottom: 10 }}>
        <Tag color={resolution.confidence === 'high' ? 'green' : 'gold'}>
          {confidenceLabel}
        </Tag>
        <Tag>{sourceLabel}</Tag>
        {resolution.requiresProbe && (
          <Tag color="gold">{t('apikeys.capabilityNeedsVerification')}</Tag>
        )}
        {profile.protocol && <Tag>{profile.protocol}</Tag>}
      </Space>
      <div style={{
        display: 'grid',
        gridTemplateColumns: 'repeat(auto-fit, minmax(min(240px, 100%), 1fr))',
        gap: 12,
        marginBottom: 12,
      }}>
        {capabilityItems.map(([label, value]) => (
          <div key={label} style={{ minWidth: 0 }}>
            <div style={{ color: 'var(--text-muted)', fontSize: 12, marginBottom: 4, overflowWrap: 'anywhere' }}>
              {label}
            </div>
            <div style={{ color: 'var(--text-primary)', minHeight: 22, overflowWrap: 'anywhere', wordBreak: 'normal' }}>
              {value}
            </div>
          </div>
        ))}
      </div>
      <Button
        size="small"
        icon={<ExperimentOutlined />}
        loading={probing}
        onClick={onProbe}
      >
        {t('apikeys.verifyCapabilities')}
      </Button>
    </div>
  );
}

export default function ApiKeys() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { hasPermission } = usePermissions();
  const canWrite = hasPermission('apikeys:write');
  const canDelete = hasPermission('apikeys:delete');

  // ── Scenario filter state ───────────────────────────────────────────────
  const [scenarioFilter, setScenarioFilter] = useState<ScenarioFilter>('all');

  // Drawer states
  const [createOpen, setCreateOpen] = useState(false);
  const [editOpen, setEditOpen] = useState(false);
  const [editingKey, setEditingKey] = useState<ApiKeyRecord | null>(null);
  const [statsKeyId, setStatsKeyId] = useState<string | null>(null);
  const [rotateOpen, setRotateOpen] = useState(false);
  const [rotatingKey, setRotatingKey] = useState<ApiKeyRecord | null>(null);
  const [rotateForm] = Form.useForm();
  const [selectedProvider, setSelectedProvider] = useState('anthropic');
  const [editProvider, setEditProvider] = useState('anthropic');

  // Health test states
  const [healthStates, setHealthStates] = useState<Record<string, 'testing' | 'ok' | 'error'>>({});

  // Forms
  const [createForm] = Form.useForm<ApiKeyFormValues>();
  const [editForm] = Form.useForm<EditFormValues>();
  const createModelType = Form.useWatch('model_type', createForm);
  const editModelType = Form.useWatch('model_type', editForm);
  const createModel = Form.useWatch('model', createForm);
  const createBaseUrl = Form.useWatch('base_url', createForm);
  const createApiKey = Form.useWatch('key_value', createForm);
  const editModel = Form.useWatch('model', editForm);
  const editBaseUrl = Form.useWatch('base_url', editForm);
  const [createResolution, setCreateResolution] = useState<CapabilityResolution>();
  const [editResolution, setEditResolution] = useState<CapabilityResolution>();
  const [createModelOptions, setCreateModelOptions] = useState<ModelOption[]>([]);
  const [editModelOptions, setEditModelOptions] = useState<ModelOption[]>([]);
  const [resolvingCreate, setResolvingCreate] = useState(false);
  const [resolvingEdit, setResolvingEdit] = useState(false);
  const [discoveringCreate, setDiscoveringCreate] = useState(false);
  const [discoveringEdit, setDiscoveringEdit] = useState(false);
  const [probingCreate, setProbingCreate] = useState(false);
  const [probingEdit, setProbingEdit] = useState(false);

  const openCreateDrawer = () => {
    createForm.resetFields();
    createForm.setFieldsValue({
      scenarios: ALL_SCENARIO_VALUES,
      model_type: 'chat',
      provider: 'anthropic',
      audio_engine: 'suno',
      reasoning_policy: 'auto',
    });
    setSelectedProvider('anthropic');
    setCreateResolution(undefined);
    setCreateModelOptions([]);
    setCreateOpen(true);
  };

  useEffect(() => {
    if (createModelType === 'embedding') {
      createForm.setFieldValue('scenarios', ALL_SCENARIO_VALUES);
    }
  }, [createForm, createModelType]);

  useEffect(() => {
    if (editModelType === 'embedding') {
      editForm.setFieldValue('scenarios', ALL_SCENARIO_VALUES);
    }
  }, [editForm, editModelType]);

  useEffect(() => {
    if (!createOpen || createModelType !== 'chat' || !createModel?.trim()) {
      setCreateResolution(undefined);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(async () => {
      setResolvingCreate(true);
      try {
        const result = await apiKeysApi.resolveModel({
          provider: selectedProvider,
          baseUrl: createBaseUrl,
          model: createModel.trim(),
          modelType: createModelType,
        });
        if (!cancelled) setCreateResolution(result);
      } catch {
        if (!cancelled) setCreateResolution(undefined);
      } finally {
        if (!cancelled) setResolvingCreate(false);
      }
    }, 300);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [createBaseUrl, createModel, createModelType, createOpen, selectedProvider]);

  useEffect(() => {
    if (!editOpen || editModelType !== 'chat' || !editModel?.trim()) {
      setEditResolution(undefined);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(async () => {
      setResolvingEdit(true);
      try {
        const result = await apiKeysApi.resolveModel({
          provider: editProvider,
          baseUrl: editBaseUrl,
          model: editModel.trim(),
          modelType: editModelType,
        });
        if (!cancelled) setEditResolution(result);
      } catch {
        if (!cancelled) setEditResolution(undefined);
      } finally {
        if (!cancelled) setResolvingEdit(false);
      }
    }, 300);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [editBaseUrl, editModel, editModelType, editOpen, editProvider]);

  // Query: list API keys
  const {
    data: rawData,
    isLoading,
    isError,
    refetch,
    isRefetching,
  } = useQuery({
    queryKey: queryKeys.apiKeys.list(),
    queryFn: () => apiKeysApi.list(),
    retry: false,
    throwOnError: false,
  });

  // Dev fallback
  const allKeys: ApiKeyRecord[] = (import.meta.env.DEV && isError) ? MOCK_API_KEYS : (rawData?.keys ?? []);

  // ── Scenario filtering ──────────────────────────────────────────────────
  const filteredKeys = scenarioFilter === 'all'
    ? allKeys
    : allKeys.filter(k => {
        const scenarios = k.scenarios ?? null;
        // Null / empty array means "all scenarios"
        if (!scenarios || scenarios.length === 0) return true;
        if (scenarioFilter === 'rd') return scenarios.includes('rd') || scenarios.includes('agent');
        return scenarios.includes(scenarioFilter);
      });

  const total = rawData?.total ?? filteredKeys.length;

  // Scenario tab counts
  const scenarioCounts: Record<ScenarioFilter, number> = {
    all: allKeys.length,
    nl2sql: allKeys.filter(k => {
      const s = k.scenarios ?? null;
      return !s || s.length === 0 || s.includes('nl2sql');
    }).length,
    chat: allKeys.filter(k => {
      const s = k.scenarios ?? null;
      return !s || s.length === 0 || s.includes('chat');
    }).length,
    rd: allKeys.filter(k => {
      const s = k.scenarios ?? null;
      return !s || s.length === 0 || s.includes('rd') || s.includes('agent');
    }).length,
    pm: allKeys.filter(k => {
      const s = k.scenarios ?? null;
      return !s || s.length === 0 || s.includes('pm');
    }).length,
  };

  // Query: per-key usage stats
  const { data: statsData } = useQuery({
    queryKey: statsKeyId ? queryKeys.apiKeys.stats(statsKeyId) : ['apiKeys', 'stats', 'none'],
    queryFn: () => apiKeysApi.stats(statsKeyId!),
    enabled: !!statsKeyId,
  });

  // Mutation: create
  const createMut = useMutation({
    mutationFn: (values: ApiKeyFormValues) =>
      apiKeysApi.create({
        name: values.name,
        // Keep OpenAI-compatible services as first-class UI presets while
        // preserving the backend's validated custom-provider transport shape.
        provider: OPENAI_COMPATIBLE_PRESET_PROVIDERS.has(values.provider) ? 'custom' : values.provider,
        base_url: OPENAI_COMPATIBLE_PRESET_PROVIDERS.has(values.provider)
          ? values.base_url?.trim() || PROVIDER_BASE_URL_DEFAULTS[values.provider]
          : values.base_url,
        model:
          values.model_type === 'audio'
            ? composeAudioModelDescriptor(values.audio_engine, values.model)
            : values.model,
        dimensions: values.model_type === 'embedding' ? values.dimensions : undefined,
        audio_generate_path:
          values.model_type === 'audio' ? values.audio_generate_path?.trim() : undefined,
        audio_query_path:
          values.model_type === 'audio' ? values.audio_query_path?.trim() : undefined,
        model_type: values.model_type || 'chat',
        key_value: values.key_value,
      daily_limit: values.daily_limit != null ? Number(values.daily_limit) : undefined,
      monthly_limit: values.monthly_limit != null ? Number(values.monthly_limit) : undefined,
        priority: values.priority != null ? Number(values.priority) : undefined,
        input_price_per_million: values.input_price_per_million != null ? Number(values.input_price_per_million) : undefined,
        output_price_per_million: values.output_price_per_million != null ? Number(values.output_price_per_million) : undefined,
        scenarios: values.model_type === 'embedding' ? ALL_SCENARIO_VALUES : values.scenarios,
        capabilities_json: values.model_type === 'chat' ? buildCapabilities(values) : null,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.embeddingConfig() });
      message.success(t('apikeys.createSuccess'));
      setCreateOpen(false);
      createForm.resetFields();
      setSelectedProvider('anthropic');
    },
    onError: (err: Error) => {
      message.error(err.message || t('common.operateFailed'));
    },
  });

  // Mutation: update
  const updateMut = useMutation({
    mutationFn: ({
      id,
      values,
    }: {
      id: string;
      values: Partial<EditFormValues>;
    }) =>
      apiKeysApi.update(id, {
        name: values.name,
        base_url: values.base_url,
        model:
          values.model_type === 'audio'
            ? composeAudioModelDescriptor(values.audio_engine, values.model)
            : values.model,
        dimensions: values.model_type === 'embedding' ? values.dimensions : undefined,
        audio_generate_path:
          values.model_type === 'audio' ? values.audio_generate_path?.trim() : undefined,
        audio_query_path:
          values.model_type === 'audio' ? values.audio_query_path?.trim() : undefined,
        model_type: values.model_type,
        key_value: values.key_value,
        daily_limit: values.daily_limit != null ? Number(values.daily_limit) : undefined,
        monthly_limit: values.monthly_limit != null ? Number(values.monthly_limit) : undefined,
        enabled: values.enabled,
        priority: values.priority != null ? Number(values.priority) : undefined,
        input_price_per_million: values.input_price_per_million != null ? Number(values.input_price_per_million) : undefined,
        output_price_per_million: values.output_price_per_million != null ? Number(values.output_price_per_million) : undefined,
        scenarios: values.model_type === 'embedding' ? ALL_SCENARIO_VALUES : values.scenarios,
        capabilities_json: values.model_type === 'chat' ? buildCapabilities(values) : null,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.embeddingConfig() });
      message.success(t('apikeys.updateSuccess'));
      setEditOpen(false);
      setEditingKey(null);
      editForm.resetFields();
    },
    onError: (err: Error) => {
      message.error(err.message || t('common.operateFailed'));
    },
  });

  // Mutation: delete
  const deleteMut = useMutation({
    mutationFn: (id: string) => apiKeysApi.delete(id),
    onMutate: async (id) => {
      await qc.cancelQueries({ queryKey: queryKeys.apiKeys.all });
      const prev = qc.getQueryData<{ keys: ApiKeyRecord[] }>(queryKeys.apiKeys.list());
      qc.setQueryData<{ keys: ApiKeyRecord[] }>(
        queryKeys.apiKeys.list(),
        (old) => {
          if (!old) return old;
          return { ...old, keys: old.keys.filter((k) => k.id !== id) };
        }
      );
      return { prev };
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.embeddingConfig() });
      message.success(t('apikeys.deleteSuccess'));
    },
    onError: (_err, _vars, ctx) => {
      if (ctx?.prev) {
        qc.setQueryData(queryKeys.apiKeys.list(), ctx.prev);
      }
      message.error(t('common.operateFailed'));
    },
  });

  // Mutation: toggle enabled
  const toggleMut = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      apiKeysApi.update(id, { enabled }),
    onMutate: async ({ id, enabled }) => {
      await qc.cancelQueries({ queryKey: queryKeys.apiKeys.all });
      const prev = qc.getQueryData<{ keys: ApiKeyRecord[] }>(
        queryKeys.apiKeys.list()
      );
      qc.setQueryData<{ keys: ApiKeyRecord[] }>(
        queryKeys.apiKeys.list(),
        (old) => {
          if (!old) return old;
          return {
            ...old,
            keys: old.keys.map((k) =>
              k.id === id ? { ...k, enabled } : k
            ),
          };
        }
      );
      return { prev };
    },
    onError: (_err, _vars, ctx) => {
      if (ctx?.prev) {
        qc.setQueryData(queryKeys.apiKeys.list(), ctx.prev);
      }
      message.error(t('common.operateFailed'));
    },
    onSettled: () => {
      qc.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
    },
  });

  // Mutation: test health
  const testHealthMut = useMutation({
    mutationFn: (id: string) => apiKeysApi.testHealth(id),
    onSuccess: (res, id) => {
      setHealthStates((prev) => ({ ...prev, [id]: res.ok ? 'ok' : 'error' }));
      if (!res.ok) {
        message.error(`${t('apikeys.healthFailed')}: ${res.error ?? 'unknown error'}`);
      }
    },
    onError: (err: Error, id) => {
      setHealthStates((prev) => ({ ...prev, [id]: 'error' }));
      message.error(err.message ?? t('common.operateFailed'));
    },
  });

  const discoverCreateModels = async () => {
    if (!createApiKey?.trim()) {
      message.warning(t('apikeys.discoveryNeedsKey'));
      return;
    }
    setDiscoveringCreate(true);
    try {
      const result = await apiKeysApi.discoverModels({
        provider: selectedProvider,
        baseUrl: createBaseUrl,
        apiKey: createApiKey.trim(),
        modelType: createModelType,
      });
      setCreateModelOptions(result.models.map((model) => ({
        value: model.id,
        label: model.displayName ? model.displayName + ' (' + model.id + ')' : model.id,
        model,
      })));
      message.success(t('apikeys.discoverySuccess', { count: result.models.length }));
    } catch (error) {
      message.error(error instanceof Error ? error.message : t('apikeys.discoveryFailed'));
    } finally {
      setDiscoveringCreate(false);
    }
  };

  const discoverEditModels = async () => {
    if (!editingKey) return;
    setDiscoveringEdit(true);
    try {
      const result = await apiKeysApi.discoverModels({
        provider: editProvider,
        baseUrl: editBaseUrl,
        existingKeyId: editingKey.id,
        modelType: editModelType,
      });
      setEditModelOptions(result.models.map((model) => ({
        value: model.id,
        label: model.displayName ? model.displayName + ' (' + model.id + ')' : model.id,
        model,
      })));
      message.success(t('apikeys.discoverySuccess', { count: result.models.length }));
    } catch (error) {
      message.error(error instanceof Error ? error.message : t('apikeys.discoveryFailed'));
    } finally {
      setDiscoveringEdit(false);
    }
  };

  const selectDiscoveredCreateModel = async (_value: string, option: ModelOption) => {
    if (createModelType !== 'chat') return;
    const detected = {
      profile: option.model.profile,
      source: option.model.source,
      confidence: option.model.confidence,
      requiresProbe: option.model.confidence !== 'high',
    };
    setCreateResolution(detected);
    try {
      const accepted = await apiKeysApi.acceptModelProfile({
        provider: selectedProvider,
        baseUrl: createBaseUrl,
        model: option.model.id,
        modelType: createModelType,
        profile: option.model.profile,
        source: option.model.source,
        confidence: option.model.confidence,
      });
      setCreateResolution(accepted);
    } catch (error) {
      message.error(error instanceof Error ? error.message : t('apikeys.capabilitySaveFailed'));
    }
  };

  const selectDiscoveredEditModel = async (_value: string, option: ModelOption) => {
    if (!editingKey || editModelType !== 'chat') return;
    const detected = {
      profile: option.model.profile,
      source: option.model.source,
      confidence: option.model.confidence,
      requiresProbe: option.model.confidence !== 'high',
    };
    setEditResolution(detected);
    try {
      const accepted = await apiKeysApi.acceptModelProfile({
        provider: editProvider,
        baseUrl: editBaseUrl,
        model: option.model.id,
        modelType: editModelType,
        profile: option.model.profile,
        source: option.model.source,
        confidence: option.model.confidence,
      });
      setEditResolution(accepted);
      qc.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
    } catch (error) {
      message.error(error instanceof Error ? error.message : t('apikeys.capabilitySaveFailed'));
    }
  };

  const probeCreateModel = async () => {
    if (!createModel?.trim() || !createApiKey?.trim()) {
      message.warning(t('apikeys.probeNeedsModelAndKey'));
      return;
    }
    setProbingCreate(true);
    try {
      const result = await apiKeysApi.probeModel({
        provider: selectedProvider,
        baseUrl: createBaseUrl,
        apiKey: createApiKey.trim(),
        model: createModel.trim(),
        modelType: createModelType,
        full: true,
      });
      setCreateResolution({
        profile: result.profile,
        source: result.source,
        confidence: result.confidence,
        requiresProbe: result.confidence !== 'high',
      });
      if (result.warning) {
        message.warning(
          isStructuredProbeFormatWarning(result.warning)
            ? t('apikeys.probeStructuredOutputInvalid')
            : result.warning,
        );
      }
      else message.success(t('apikeys.probeSuccess'));
    } catch (error) {
      const detail = error instanceof Error ? error.message : '';
      message.error(
        isStructuredProbeFormatWarning(detail)
          ? t('apikeys.probeStructuredOutputInvalid')
          : detail || t('apikeys.probeFailed'),
      );
    } finally {
      setProbingCreate(false);
    }
  };

  const probeEditModel = async () => {
    if (!editingKey || !editModel?.trim()) {
      message.warning(t('apikeys.probeNeedsModelAndKey'));
      return;
    }
    setProbingEdit(true);
    try {
      const result = await apiKeysApi.probeModel({
        provider: editProvider,
        baseUrl: editBaseUrl,
        existingKeyId: editingKey.id,
        model: editModel.trim(),
        modelType: editModelType,
        full: true,
      });
      setEditResolution({
        profile: result.profile,
        source: result.source,
        confidence: result.confidence,
        requiresProbe: result.confidence !== 'high',
      });
      qc.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
      if (result.warning) {
        message.warning(
          isStructuredProbeFormatWarning(result.warning)
            ? t('apikeys.probeStructuredOutputInvalid')
            : result.warning,
        );
      }
      else message.success(t('apikeys.probeSuccess'));
    } catch (error) {
      const detail = error instanceof Error ? error.message : '';
      message.error(
        isStructuredProbeFormatWarning(detail)
          ? t('apikeys.probeStructuredOutputInvalid')
          : detail || t('apikeys.probeFailed'),
      );
    } finally {
      setProbingEdit(false);
    }
  };

  // Helpers
  const openEdit = (record: ApiKeyRecord) => {
    setEditingKey(record);
    const presentation = providerPresentation(record);
    setEditProvider(presentation.value);
    if (record.resolved_capabilities) {
      setEditResolution({
        profile: record.resolved_capabilities,
        source: record.model_profile?.source ?? 'built_in_registry',
        confidence: record.model_profile?.confidence ?? 'high',
        requiresProbe: record.model_profile?.confidence === 'low',
      });
    }
    const parsedAudioModel =
      record.model_type === 'audio'
        ? parseAudioModelDescriptor(record.model)
        : { audioEngine: 'suno', modelVersion: record.model ?? '' };
    editForm.setFieldsValue({
      name: record.name,
      provider: record.provider,
      base_url: record.base_url,
      model: parsedAudioModel.modelVersion,
      dimensions: record.dimensions ?? undefined,
      audio_engine: parsedAudioModel.audioEngine,
      audio_generate_path: record.audio_generate_path,
      audio_query_path: record.audio_query_path,
      model_type: record.model_type,
      daily_limit: record.daily_limit,
      monthly_limit: record.monthly_limit,
      enabled: record.enabled,
      priority: record.priority,
      input_price_per_million: record.input_price_per_million,
      output_price_per_million: record.output_price_per_million,
      scenarios: record.model_type === 'embedding' ? ALL_SCENARIO_VALUES : record.scenarios ?? [],
      supports_reasoning_effort: record.capabilities_json?.reasoningEffort ?? false,
      reasoning_effort_default: record.capabilities_json?.reasoningEffortDefault ?? 'high',
      reasoning_transport: record.capabilities_json?.reasoningTransport ?? 'reasoning_effort',
      reasoning_effort_values: record.capabilities_json?.reasoningEffortValues ?? [],
      reasoning_policy: record.capabilities_json?.reasoningPolicy ?? 'auto',
      include_reasoning: record.capabilities_json?.includeReasoning ?? false,
      use_max_completion_tokens: record.capabilities_json?.useMaxCompletionTokens ?? false,
      native_web_search_enabled: !!record.capabilities_json?.nativeWebSearch?.enabled,
      native_web_search_extra_body: stringifyJsonObject(record.capabilities_json?.nativeWebSearch?.extraBody),
      native_web_search_tool_template: stringifyJsonObject(record.capabilities_json?.nativeWebSearch?.toolTemplate),
      context_window_tokens: record.capabilities_json?.contextWindowTokens ?? undefined,
      max_output_tokens: record.capabilities_json?.maxOutputTokens ?? undefined,
    });
    setEditOpen(true);
  };

  const providerOptions = [
    { value: 'anthropic', label: t('apikeys.providerAnthropic') },
    { value: 'openai', label: t('apikeys.providerOpenAI') },
    { value: 'deepseek', label: t('apikeys.providerDeepSeek') },
    { value: 'kimi', label: t('apikeys.providerKimi') },
    { value: 'glm', label: t('apikeys.providerGLM') },
    { value: 'gemini', label: t('apikeys.providerGemini') },
    { value: 'xai', label: t('apikeys.providerXAI') },
    { value: 'custom', label: t('apikeys.providerCustom') },
  ];

  const providerPresentation = (record: Pick<ApiKeyRecord, 'provider' | 'base_url'>) => {
    const baseUrl = record.base_url?.toLowerCase() ?? '';
    let inferred = record.provider;
    if (record.provider === 'custom') {
      if (baseUrl.includes('api.deepseek.com')) inferred = 'deepseek';
      else if (baseUrl.includes('api.moonshot.cn')) inferred = 'kimi';
      else if (baseUrl.includes('open.bigmodel.cn')) inferred = 'glm';
      else if (baseUrl.includes('generativelanguage.googleapis.com')) inferred = 'gemini';
      else if (baseUrl.includes('api.x.ai')) inferred = 'xai';
    }
    const label = providerOptions.find((option) => option.value === inferred)?.label ?? inferred;
    return { value: inferred, label };
  };

  const handleCreateProviderChange = (provider: string) => {
    setSelectedProvider(provider);
    if (EDITABLE_BASE_URL_PRESET_PROVIDERS.has(provider)) {
      createForm.setFieldValue('base_url', PROVIDER_BASE_URL_DEFAULTS[provider]);
    } else {
      createForm.setFieldValue('base_url', undefined);
    }
  };

  const modelTypeOptions = [
    { value: 'chat', label: t('apikeys.modelTypeChat') },
    { value: 'embedding', label: t('apikeys.modelTypeEmbedding') },
    { value: 'image', label: t('apikeys.modelTypeImage') },
    { value: 'video', label: t('apikeys.modelTypeVideo') },
    { value: 'audio', label: t('apikeys.modelTypeAudio') },
  ];

  const modelTypeLabel = (modelType?: string) => {
    switch (modelType) {
      case 'embedding':
        return t('apikeys.modelTypeEmbedding');
      case 'image':
        return t('apikeys.modelTypeImage');
      case 'video':
        return t('apikeys.modelTypeVideo');
      case 'audio':
        return t('apikeys.modelTypeAudio');
      case 'chat':
      default:
        return t('apikeys.modelTypeChat');
    }
  };

  const scenarioTagColor = (s: string) => SCENARIO_TAG_COLOR[s] ?? 'default';

  const scenarioLabel = (s: string) => {
    switch (s) {
      case 'chat': return t('apikeys.scenarioChat');
      case 'nl2sql': return t('apikeys.scenarioNl2sql');
      case 'rd':
        return t('apikeys.scenarioRd');
      case 'agent': return t('apikeys.scenarioAgent');
      case 'pm': return t('apikeys.scenarioPm');
      default: return s;
    }
  };

  const reasoningEffortOptions = (resolution?: CapabilityResolution) => {
    const values = resolution?.profile.reasoningEffortValues;
    const effectiveValues = values && values.length > 0
      ? values
      : ['minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
    return effectiveValues.map((value) => ({
      value,
      label: (() => {
        switch (value) {
          case 'minimal': return t('apikeys.reasoningEffortValue.minimal');
          case 'low': return t('apikeys.reasoningEffortValue.low');
          case 'medium': return t('apikeys.reasoningEffortValue.medium');
          case 'high': return t('apikeys.reasoningEffortValue.high');
          case 'xhigh': return t('apikeys.reasoningEffortValue.xhigh');
          case 'max': return t('apikeys.reasoningEffortValue.max');
          case 'enabled': return t('apikeys.reasoningEffortValue.enabled');
          case 'disabled': return t('apikeys.reasoningEffortValue.disabled');
          default: return value;
        }
      })(),
    }));
  };

  const scenarioTabItems = [
    { key: 'all' as ScenarioFilter, label: `${t('apikeys.scenariosAll')} (${scenarioCounts.all})` },
    { key: 'chat' as ScenarioFilter, label: `${t('apikeys.scenarioChat')} (${scenarioCounts.chat})` },
    { key: 'nl2sql' as ScenarioFilter, label: `${t('apikeys.scenarioNl2sql')} (${scenarioCounts.nl2sql})` },
    { key: 'rd' as ScenarioFilter, label: `${t('apikeys.scenarioRd')} (${scenarioCounts.rd})` },
    { key: 'pm' as ScenarioFilter, label: `${t('apikeys.scenarioPm')} (${scenarioCounts.pm})` },
  ];

  const columns: ColumnsType<ApiKeyRecord> = [
    {
      title: t('apikeys.columns.name'),
      dataIndex: 'name',
      key: 'name',
      width: 160,
      render: (v) => <Text strong>{v}</Text>,
    },
    {
      title: t('apikeys.columns.provider'),
      dataIndex: 'provider',
      key: 'provider',
      width: 100,
      render: (_v, record) => {
        const provider = providerPresentation(record);
        return <Tag color={PROVIDER_COLOR[provider.value] ?? 'default'}>{provider.label || '—'}</Tag>;
      },
    },
    {
      title: t('apikeys.columns.modelType'),
      dataIndex: 'model_type',
      key: 'model_type',
      width: 100,
      render: (v) => (
        <Tag color={MODEL_TYPE_COLOR[v] ?? 'blue'}>{modelTypeLabel(v)}</Tag>
      ),
    },
    {
      title: t('apikeys.columns.model'),
      dataIndex: 'model',
      key: 'model',
      width: 150,
      render: (v, r) => {
        const runtimeWarning =
          r.runtime_available === false ? (
            <Tooltip title={r.runtime_error || t('apikeys.runtimeUnavailable')}>
              <Tag color="red" style={{ marginTop: 4 }}>
                {t('apikeys.runtimeUnavailable')}
              </Tag>
            </Tooltip>
          ) : null;
        if (r.model_type === 'audio') {
          const parsed = parseAudioModelDescriptor(typeof v === 'string' ? v : undefined);
          return (
            <Space direction="vertical" size={0}>
              <Tag>{parsed.modelVersion || 'suno'}</Tag>
              {runtimeWarning}
            </Space>
          );
        }
        return (
          <Space direction="vertical" size={0}>
            {v ? (
              <Tag>{v}</Tag>
            ) : (
              <Text type="secondary" style={{ fontSize: 12 }}>
                {t('common.noData')}
              </Text>
            )}
            {runtimeWarning}
          </Space>
        );
      },
    },
    {
      title: t('apikeys.scenarios'),
      key: 'scenarios',
      width: 200,
      render: (_, r) => {
        const scenarios = r.scenarios;
        if (!scenarios || scenarios.length === 0) {
          return (
            <Tag color="default" style={{ fontSize: 11 }}>
              {t('apikeys.scenariosAll')}
            </Tag>
          );
        }
        return (
          <Space size={4} wrap>
            {scenarios.map((s) => (
              <Tag key={s} color={scenarioTagColor(s)} style={{ fontSize: 11 }}>
                {scenarioLabel(s)}
              </Tag>
            ))}
          </Space>
        );
      },
    },
    {
      title: t('apikeys.columns.keyHint'),
      dataIndex: 'key_hint',
      key: 'key_hint',
      width: 130,
      render: (v) => (
        <code
          style={{
            fontSize: 12,
            background: 'var(--bg-surface)',
            padding: '2px 6px',
            borderRadius: 4,
          }}
        >
          **__{v}
        </code>
      ),
    },
    {
      title: t('apikeys.columns.dailyLimit'),
      dataIndex: 'daily_limit',
      key: 'daily_limit',
      width: 120,
      align: 'right',
      render: (v, r) => {
        const used = r.usage_today ?? 0;
        const pct = v && v > 0 ? Math.min((used / v) * 100, 100) : 0;
        return (
          <Tooltip title={`${used.toLocaleString()} / ${v?.toLocaleString() ?? t('apikeys.noLimit')}`}>
            <div style={{ minWidth: 80 }}>
              <Progress
                percent={pct}
                size="small"
                showInfo={false}
                strokeColor={pct > 80 ? '#ff4d4f' : pct > 60 ? '#faad14' : '#52c41a'}
                style={{ marginBottom: 2 }}
              />
              <Text style={{ fontSize: 11 }}>
                {v ? `${used.toLocaleString()} / ${v.toLocaleString()}` : t('apikeys.noLimit')}
              </Text>
            </div>
          </Tooltip>
        );
      },
    },
    {
      title: t('apikeys.columns.enabled'),
      dataIndex: 'enabled',
      key: 'enabled',
      width: 90,
      render: (v, r) => {
        const health = healthStates[r.id];
        return (
          <Space direction="vertical" size={2}>
            <Tooltip title={!canWrite ? t('common.noPermission') : undefined}>
              <span>
                <Switch
                  checked={v}
                  checkedChildren={<CheckCircleOutlined />}
                  unCheckedChildren={<StopOutlined />}
                  size="small"
                  disabled={!canWrite}
                  loading={toggleMut.isPending}
                  onChange={(checked) =>
                    toggleMut.mutate({ id: r.id, enabled: checked })
                  }
                />
              </span>
            </Tooltip>
            {health === 'testing' && <Spin size="small" />}
            {health === 'ok' && <Text type="success" style={{ fontSize: 10 }}>{t('apikeys.healthOk')}</Text>}
            {health === 'error' && <Text type="danger" style={{ fontSize: 10 }}>{t('apikeys.healthError')}</Text>}
          </Space>
        );
      },
    },
    {
      title: t('apikeys.columns.createdAt'),
      dataIndex: 'created_at',
      key: 'created_at',
      width: 110,
      render: (v) =>
        v ? (
          <Text type="secondary" style={{ fontSize: 12 }}>
            {new Date(v).toLocaleDateString()}
          </Text>
        ) : (
          '—'
        ),
    },
    {
      title: t('common.actions'),
      key: 'action',
      width: 160,
      fixed: 'right',
      render: (_, r) => (
        <Space size={4}>
          <Tooltip title={t('apikeys.testHealth')}>
            <Button
              size="small"
              icon={<ExperimentOutlined />}
              onClick={() => {
                setHealthStates((prev) => ({ ...prev, [r.id]: 'testing' }));
                testHealthMut.mutate(r.id);
              }}
              loading={healthStates[r.id] === 'testing'}
            />
          </Tooltip>
          <Tooltip title={t('apikeys.viewStats')}>
            <Button
              size="small"
              icon={<BarChartOutlined />}
              onClick={() => setStatsKeyId(r.id)}
            />
          </Tooltip>
          {canWrite && (
            <Tooltip title={t('common.edit')}>
              <Button
                size="small"
                icon={<EditOutlined />}
                onClick={() => openEdit(r)}
              />
            </Tooltip>
          )}
          {canDelete && (
            <Popconfirm
              title={t('apikeys.deleteConfirm')}
              onConfirm={() => deleteMut.mutate(r.id)}
              okText={t('common.confirm')}
              cancelText={t('common.cancel')}
              okButtonProps={{ danger: true }}
            >
              <Tooltip title={t('common.delete')}>
                <Button size="small" danger icon={<DeleteOutlined />} />
              </Tooltip>
            </Popconfirm>
          )}
        </Space>
      ),
    },
  ];

  if (isLoading) return <PageSkeleton rows={5} />;

  const enabledCount = filteredKeys.filter((k) => k.enabled).length;
  const hasFailover = enabledCount > 1;
  const isDevFallback = import.meta.env.DEV && isError;

  return (
    <div style={{ padding: '24px 24px 0' }}>
      {isDevFallback && (
        <Alert
          type="warning"
          message={t('errors.devFallback')}
          showIcon
          style={{ marginBottom: 16 }}
        />
      )}

      {/* Header */}
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'flex-start',
          marginBottom: 20,
          gap: 16,
        }}
      >
        <div>
          <Title level={3} style={{ margin: 0 }}>
            <KeyOutlined style={{ marginRight: 8 }} />
            {t('apikeys.title')} <span style={{ fontWeight: 400, color: '#999', fontSize: 14 }}>({total})</span>
          </Title>
          <Paragraph type="secondary" style={{ margin: '4px 0 0', fontSize: 13 }}>
            {t('apikeys.subtitle')}
          </Paragraph>
        </div>
        <Space size={12}>
          {hasFailover && (
            <Tag icon={<Badge status="success" text="" />} color="green" style={{ alignSelf: 'center' }}>
              {t('apikeys.failover')}: {enabledCount} {t('common.enabled')}
            </Tag>
          )}
          <Button icon={<ReloadOutlined spin={isRefetching} />} onClick={() => refetch()}>
            {t('common.refresh')}
          </Button>
          {canWrite && (
            <Button type="primary" icon={<PlusOutlined />} onClick={openCreateDrawer}>
              {t('apikeys.add')}
            </Button>
          )}
        </Space>
      </div>

      {/* Stats row */}
      {filteredKeys.length > 0 && (
        <div
          style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(3, 1fr)',
            gap: 12,
            marginBottom: 16,
          }}
        >
          <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
            <Text type="secondary" style={{ fontSize: 12 }}>{t('common.total', { count: filteredKeys.length })}</Text>
            <div style={{ fontSize: 24, fontWeight: 600 }}>{filteredKeys.length}</div>
          </Card>
          <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
            <Text type="secondary" style={{ fontSize: 12 }}>{t('common.enabled')}</Text>
            <div style={{ fontSize: 24, fontWeight: 600, color: '#3fb950' }}>{enabledCount}</div>
          </Card>
          <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
            <Text type="secondary" style={{ fontSize: 12 }}>{t('apikeys.failover')}</Text>
            <div style={{ fontSize: 24, fontWeight: 600, color: hasFailover ? '#3fb950' : '#8b949e' }}>
              {hasFailover ? t('common.enabled') : t('common.disabled')}
            </div>
          </Card>
        </div>
      )}

      {/* Scenario filter tabs */}
      <div
        style={{
          display: 'flex',
          gap: 8,
          marginBottom: 12,
          flexWrap: 'wrap',
        }}
      >
        {scenarioTabItems.map((tab) => (
          <Button
            key={tab.key}
            type={scenarioFilter === tab.key ? 'primary' : 'default'}
            onClick={() => setScenarioFilter(tab.key)}
          >
            {tab.label}
          </Button>
        ))}
      </div>

      <Card styles={{ body: { padding: 0 } }}>
        <Table
          columns={columns}
          dataSource={filteredKeys}
          rowKey="id"
          pagination={{ pageSize: 20, size: 'small' }}
          scroll={{ x: 1300 }}
          locale={{
            emptyText: (
              <div style={{ padding: '48px 0', textAlign: 'center' }}>
                <KeyOutlined style={{ fontSize: 48, color: '#484f58', marginBottom: 16, display: 'block' }} />
                <Text strong style={{ fontSize: 16 }}>
                  {t('apikeys.empty.title')}
                </Text>
                <br />
                <Text type="secondary" style={{ fontSize: 13 }}>
                  {t('apikeys.empty.description')}
                </Text>
                <br />
                {canWrite && (
                  <Button
                    type="primary"
                    icon={<PlusOutlined />}
                    onClick={openCreateDrawer}
                    style={{ marginTop: 16 }}
                  >
                    {t('apikeys.empty.action')}
                  </Button>
                )}
              </div>
            ),
          }}
        />
      </Card>

      {/* ── Create Drawer ────────────────────────────────────────────────── */}
      <Drawer
        title={t('apikeys.add')}
        open={createOpen}
        onClose={() => {
          setCreateOpen(false);
          createForm.resetFields();
          setSelectedProvider('anthropic');
        }}
        width={520}
        footer={
          <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
            <Button
              onClick={() => {
                setCreateOpen(false);
                createForm.resetFields();
                setSelectedProvider('anthropic');
              }}
            >
              {t('common.cancel')}
            </Button>
            <Button
              type="primary"
              loading={createMut.isPending}
              onClick={() => createForm.submit()}
            >
              {t('common.create')}
            </Button>
          </div>
        }
      >
        <Form
          form={createForm}
          layout="vertical"
          onFinish={(values) => createMut.mutate(values)}
          initialValues={{
            scenarios: ALL_SCENARIO_VALUES,
            model_type: 'chat',
            provider: 'anthropic',
            audio_engine: 'suno',
            reasoning_policy: 'auto',
          }}
        >
          <Form.Item
            name="name"
            label={t('apikeys.columns.name')}
            rules={[{ required: true, message: t('common.required') + ' ' + t('common.name') }]}
          >
            <Input placeholder={t('apikeys.namePlaceholder')} />
          </Form.Item>

          <Form.Item
            name="provider"
            label={t('apikeys.provider')}
            rules={[{ required: true, message: t('common.required') + ' ' + t('apikeys.provider') }]}
          >
            <Select options={providerOptions} onChange={handleCreateProviderChange} />
          </Form.Item>

          <BaseUrlInput
            visible={selectedProvider === 'custom' || EDITABLE_BASE_URL_PRESET_PROVIDERS.has(selectedProvider)}
            label={t('apikeys.baseUrl')}
            extra={t('apikeys.baseUrlExtra')}
            placeholder={t('apikeys.baseUrlPlaceholder')}
          />

          <Form.Item
            name="key_value"
            label={t('apikeys.keyValue')}
            rules={[{ required: true, message: t('common.required') + ' ' + t('apikeys.keyValue') }]}
            extra={t('apikeys.encryptionNote')}
          >
            <Input.Password
              placeholder={t('apikeys.keyPlaceholder')}
              iconRender={(visible) =>
                visible ? <EyeOutlined /> : <EyeInvisibleOutlined />
              }
            />
          </Form.Item>

          <Form.Item
            name="model_type"
            label={t('apikeys.modelType')}
            extra={t('apikeys.modelTypeExtra')}
          >
            <Select
              options={modelTypeOptions}
              onChange={(v) => {
                if (v === 'audio') {
                  if (!createForm.getFieldValue('audio_engine')) {
                    createForm.setFieldValue('audio_engine', 'suno');
                  }
                  if (createForm.getFieldValue('provider') !== 'custom') {
                    createForm.setFieldValue('provider', 'custom');
                    setSelectedProvider('custom');
                  }
                } else if (v === 'embedding') {
                  createForm.setFieldValue('scenarios', ALL_SCENARIO_VALUES);
                }
              }}
            />
          </Form.Item>

          {createModelType === 'audio' && (
            <Form.Item
              name="audio_engine"
              label={t('apikeys.audioEngine')}
              rules={[{ required: true, message: t('common.required') + ' ' + t('apikeys.audioEngine') }]}
            >
              <Select
                options={AUDIO_ENGINE_OPTIONS}
                placeholder={t('apikeys.audioEnginePlaceholder')}
                allowClear={false}
              />
            </Form.Item>
          )}

          <Form.Item label={t('apikeys.model')} extra={t('apikeys.modelExtensibilityHint')}>
            <Space.Compact block>
              <Form.Item name="model" noStyle>
                <AutoComplete
                  style={{ width: '100%' }}
                  options={createModelOptions}
                  onSelect={selectDiscoveredCreateModel}
                  filterOption={(input, option) =>
                    String(option?.value ?? '').toLowerCase().includes(input.toLowerCase())}
                  placeholder={
                    createModelType === 'audio'
                      ? t('apikeys.audioVersionPlaceholder')
                      : t('apikeys.modelPlaceholder')
                  }
                />
              </Form.Item>
              <Tooltip title={t('apikeys.discoverModels')}>
                <Button
                  icon={<SyncOutlined />}
                  loading={discoveringCreate}
                  onClick={discoverCreateModels}
                  disabled={createModelType === 'audio'}
                />
              </Tooltip>
            </Space.Compact>
          </Form.Item>

          {createModelType === 'embedding' && (
            <Form.Item
              name="dimensions"
              label={t('apikeys.embeddingDimensions')}
              extra={t('apikeys.embeddingDimensionsExtra')}
              rules={[{ required: true, message: t('common.required') }]}
            >
              <InputNumber min={1} max={32768} precision={0} style={{ width: '100%' }} />
            </Form.Item>
          )}

          {createModelType === 'audio' && (
            <>
              <Form.Item
                name="audio_generate_path"
                label={t('apikeys.audioGeneratePath')}
                extra={t('apikeys.audioEndpointHint')}
                rules={[
                  {
                    required: true,
                    message: t('common.required') + ' ' + t('apikeys.audioGeneratePath'),
                  },
                ]}
              >
                <Input placeholder={t('apikeys.audioGeneratePathPlaceholder')} />
              </Form.Item>
              <Form.Item
                name="audio_query_path"
                label={t('apikeys.audioQueryPath')}
                extra={t('apikeys.audioQueryEndpointHint')}
                rules={[
                  {
                    required: true,
                    message: t('common.required') + ' ' + t('apikeys.audioQueryPath'),
                  },
                ]}
              >
                <Input placeholder={t('apikeys.audioQueryPathPlaceholder')} />
              </Form.Item>
            </>
          )}

          {createModelType === 'chat' && (
            <>
              <Divider style={{ margin: '12px 0' }} />
              <Text strong style={{ display: 'block', marginBottom: 8 }}>
                {t('apikeys.modelCapabilities')}
              </Text>
              <Text type="secondary" style={{ display: 'block', fontSize: 12, marginBottom: 12 }}>
                {t('apikeys.modelCapabilitiesHint')}
              </Text>
              <CapabilitySummary
                resolution={createResolution}
                loading={resolvingCreate}
                probing={probingCreate}
                onProbe={probeCreateModel}
              />
              {createResolution?.profile.reasoningEffort && (
                <Form.Item
                  name="reasoning_policy"
                  label={t('apikeys.reasoningPolicy')}
                  extra={t('apikeys.reasoningPolicyHint')}
                  style={{ marginTop: 12 }}
                >
                  <Select
                    style={{ width: '100%' }}
                    options={[
                      { value: 'auto', label: t('apikeys.reasoningPolicyValue.auto') },
                      { value: 'fast', label: t('apikeys.reasoningPolicyValue.fast') },
                      { value: 'standard', label: t('apikeys.reasoningPolicyValue.standard') },
                      { value: 'deep', label: t('apikeys.reasoningPolicyValue.deep') },
                      { value: 'maximum', label: t('apikeys.reasoningPolicyValue.maximum') },
                    ]}
                  />
                </Form.Item>
              )}
              <Divider titlePlacement="start" plain>
                {t('apikeys.manualOverrides')}
              </Divider>
              <Form.Item name="supports_reasoning_effort" valuePropName="checked" style={{ marginBottom: 8 }}>
                <Checkbox>{t('apikeys.supportsReasoningEffort')}</Checkbox>
              </Form.Item>
              <Form.Item
                noStyle
                shouldUpdate={(prev, cur) => prev.supports_reasoning_effort !== cur.supports_reasoning_effort}
              >
                {({ getFieldValue }) =>
                  getFieldValue('supports_reasoning_effort') ? (
                    <>
                      <Row gutter={12}>
                        <Col span={12}>
                          <Form.Item
                            name="reasoning_transport"
                            label={t('apikeys.reasoningTransport')}
                            initialValue="reasoning_effort"
                          >
                            <Select options={[
                              { value: 'reasoning_effort', label: 'reasoning_effort' },
                              { value: 'anthropic_thinking', label: 'Anthropic thinking' },
                              { value: 'thinking_level', label: 'thinking_level' },
                              { value: 'enable_thinking', label: 'enable_thinking' },
                            ]} />
                          </Form.Item>
                        </Col>
                        <Col span={12}>
                          <Form.Item
                            name="reasoning_effort_values"
                            label={t('apikeys.reasoningSupportedValues')}
                          >
                            <Select mode="tags" options={reasoningEffortOptions(createResolution)} />
                          </Form.Item>
                        </Col>
                      </Row>
                      <Form.Item
                        name="reasoning_effort_default"
                        label={t('apikeys.reasoningEffortDefault')}
                        style={{ marginBottom: 8 }}
                      >
                        <Select options={reasoningEffortOptions(createResolution)} />
                      </Form.Item>
                    </>
                  ) : null
                }
              </Form.Item>
              <Form.Item name="include_reasoning" valuePropName="checked" style={{ marginBottom: 8 }}>
                <Checkbox>{t('apikeys.includeReasoning')}</Checkbox>
              </Form.Item>
              <Form.Item name="use_max_completion_tokens" valuePropName="checked" style={{ marginBottom: 0 }}>
                <Checkbox>{t('apikeys.useMaxCompletionTokens')}</Checkbox>
              </Form.Item>
              <Form.Item name="native_web_search_enabled" valuePropName="checked" style={{ marginTop: 8, marginBottom: 8 }}>
                <Checkbox>{t('apikeys.nativeWebSearchEnabled')}</Checkbox>
              </Form.Item>
              <Form.Item
                noStyle
                shouldUpdate={(prev, cur) => prev.native_web_search_enabled !== cur.native_web_search_enabled}
              >
                {({ getFieldValue }) =>
                  getFieldValue('native_web_search_enabled') ? (
                    <Row gutter={12}>
                      <Col span={12}>
                        <Form.Item
                          name="native_web_search_extra_body"
                          label={t('apikeys.nativeWebSearchExtraBody')}
                          extra={t('apikeys.nativeWebSearchExtraBodyHint')}
                        >
                          <Input.TextArea
                            rows={4}
                            placeholder={'{}'}
                          />
                        </Form.Item>
                      </Col>
                      <Col span={12}>
                        <Form.Item
                          name="native_web_search_tool_template"
                          label={t('apikeys.nativeWebSearchToolTemplate')}
                          extra={t('apikeys.nativeWebSearchToolTemplateHint')}
                        >
                          <Input.TextArea
                            rows={4}
                            placeholder={'{"type":"web_search_preview"}'}
                          />
                        </Form.Item>
                      </Col>
                    </Row>
                  ) : null
                }
              </Form.Item>
              <Row gutter={12} style={{ marginTop: 12 }}>
                <Col span={12}>
                  <Form.Item
                    name="context_window_tokens"
                    label={t('apikeys.contextWindowTokens')}
                    extra={t('apikeys.tokenLimitAutoHint')}
                  >
                    <Input type="number" min={1024} placeholder={t('apikeys.autoDetect')} />
                  </Form.Item>
                </Col>
                <Col span={12}>
                  <Form.Item
                    name="max_output_tokens"
                    label={t('apikeys.maxOutputTokens')}
                    extra={t('apikeys.tokenLimitAutoHint')}
                  >
                    <Input type="number" min={1024} placeholder={t('apikeys.autoDetect')} />
                  </Form.Item>
                </Col>
              </Row>
            </>
          )}

          <Divider style={{ margin: '12px 0' }} />

          {/* Scenario selector */}
          <Form.Item
            name="scenarios"
            label={t('apikeys.scenarios')}
            extra={
              <Text type="secondary" style={{ fontSize: 12 }}>
                {createModelType === 'embedding'
                  ? t('apikeys.embeddingScenariosLocked')
                  : t('apikeys.scenariosExtra')}
              </Text>
            }
            rules={[
              {
                validator: (_, value) =>
                  Array.isArray(value) && value.length > 0
                    ? Promise.resolve()
                    : Promise.reject(new Error(t('apikeys.scenariosRequired'))),
              },
            ]}
          >
            <Checkbox.Group disabled={createModelType === 'embedding'}>
              <Space direction="vertical">
                {ALL_SCENARIOS.map((s) => (
                  <Checkbox key={s} value={s}>
                    <Tag color={scenarioTagColor(s)} style={{ marginLeft: 4 }}>{scenarioLabel(s)}</Tag>
                  </Checkbox>
                ))}
              </Space>
            </Checkbox.Group>
          </Form.Item>

          <Divider style={{ margin: '12px 0' }} />

          <Row gutter={12}>
            <Col span={8}>
              <Form.Item name="daily_limit" label={t('apikeys.dailyLimit')}>
                <Input type="number" placeholder={t('apikeys.noLimit')} min={0} />
              </Form.Item>
            </Col>
            <Col span={8}>
              <Form.Item name="monthly_limit" label={t('apikeys.monthlyLimit')}>
                <Input type="number" placeholder={t('apikeys.noLimit')} min={0} />
              </Form.Item>
            </Col>
            <Col span={8}>
              <Form.Item name="priority" label={t('apikeys.priority')}>
                <Input type="number" placeholder="0" min={0} />
              </Form.Item>
            </Col>
          </Row>

          <Divider style={{ margin: '12px 0' }} />

          <Text type="secondary" style={{ fontSize: 12, display: 'block', marginBottom: 12 }}>
            {t('apikeys.customPricingHint')}
          </Text>
          <Row gutter={12}>
            <Col span={12}>
              <Form.Item name="input_price_per_million" label={t('apikeys.inputPriceLabel')}>
                <Input
                  type="number"
                  placeholder={t('apikeys.pricePlaceholder')}
                  min={0}
                  step={0.01}
                  prefix="$"
                  suffix="/1M"
                />
              </Form.Item>
            </Col>
            <Col span={12}>
              <Form.Item name="output_price_per_million" label={t('apikeys.outputPriceLabel')}>
                <Input
                  type="number"
                  placeholder={t('apikeys.pricePlaceholder')}
                  min={0}
                  step={0.01}
                  prefix="$"
                  suffix="/1M"
                />
              </Form.Item>
            </Col>
          </Row>

          {hasFailover && (
            <Descriptions size="small" column={1} bordered>
              <Descriptions.Item
                label={
                  <Space>
                    <Badge status="success" />
                    {t('apikeys.failover')}
                  </Space>
                }
              >
                <Text type="secondary" style={{ fontSize: 12 }}>{t('apikeys.failoverDesc')}</Text>
              </Descriptions.Item>
            </Descriptions>
          )}
        </Form>
      </Drawer>

      {/* ── Edit Drawer ───────────────────────────────────────────────────── */}
      <Drawer
        title={t('apikeys.edit')}
        open={editOpen}
        onClose={() => {
          setEditOpen(false);
          setEditingKey(null);
          editForm.resetFields();
        }}
        width={520}
        footer={
          <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
            <Button
              onClick={() => {
                setEditOpen(false);
                setEditingKey(null);
                editForm.resetFields();
              }}
            >
              {t('common.cancel')}
            </Button>
            <Button
              type="primary"
              loading={updateMut.isPending}
              onClick={() => editForm.submit()}
            >
              {t('common.save')}
            </Button>
          </div>
        }
      >
        {editingKey && (
          <Descriptions size="small" column={2} style={{ marginBottom: 24 }}>
            <Descriptions.Item label={t('apikeys.columns.provider')}>
              {(() => {
                const provider = providerPresentation(editingKey);
                return <Tag color={PROVIDER_COLOR[provider.value] ?? 'default'}>{provider.label}</Tag>;
              })()}
            </Descriptions.Item>
            <Descriptions.Item label={t('common.createdAt')}>
              {new Date(editingKey.created_at).toLocaleDateString()}
            </Descriptions.Item>
          </Descriptions>
        )}
        <Form
          form={editForm}
          layout="vertical"
          onFinish={(values) => {
            if (!editingKey) return;
            const cleanValues = { ...values };
            if (!cleanValues.key_value?.trim()) {
              delete cleanValues.key_value;
            }
            updateMut.mutate({
              id: editingKey.id,
              values: {
                ...cleanValues,
                model:
                  cleanValues.model_type === 'audio'
                    ? composeAudioModelDescriptor(cleanValues.audio_engine, cleanValues.model)
                    : cleanValues.model,
                daily_limit: cleanValues.daily_limit != null
                  ? Number(cleanValues.daily_limit) : undefined,
                monthly_limit: cleanValues.monthly_limit != null
                  ? Number(cleanValues.monthly_limit) : undefined,
                priority: cleanValues.priority != null
                  ? Number(cleanValues.priority) : undefined,
                input_price_per_million: cleanValues.input_price_per_million != null
                  ? Number(cleanValues.input_price_per_million) : undefined,
                output_price_per_million: cleanValues.output_price_per_million != null
                  ? Number(cleanValues.output_price_per_million) : undefined,
                scenarios: cleanValues.model_type === 'embedding'
                  ? ALL_SCENARIO_VALUES
                  : cleanValues.scenarios,
              },
            });
          }}
        >
          <Form.Item
            name="name"
            label={t('apikeys.columns.name')}
            rules={[{ required: true }]}
          >
            <Input />
          </Form.Item>

          <BaseUrlInput
            visible={editProvider === 'custom' || editProvider === 'deepseek'}
            label={t('apikeys.baseUrl')}
          />

          <Form.Item
            name="model_type"
            label={t('apikeys.modelType')}
            extra={t('apikeys.modelTypeExtra')}
          >
            <Select
              options={modelTypeOptions}
              onChange={(v) => {
                if (v === 'audio') {
                  if (!editForm.getFieldValue('audio_engine')) {
                    editForm.setFieldValue('audio_engine', 'suno');
                  }
                  if (editingKey?.provider === 'custom') {
                    setEditProvider('custom');
                  }
                } else if (v === 'embedding') {
                  editForm.setFieldValue('scenarios', ALL_SCENARIO_VALUES);
                }
              }}
            />
          </Form.Item>

          {editModelType === 'audio' && (
            <Form.Item
              name="audio_engine"
              label={t('apikeys.audioEngine')}
              rules={[{ required: true, message: t('common.required') + ' ' + t('apikeys.audioEngine') }]}
            >
              <Select
                options={AUDIO_ENGINE_OPTIONS}
                placeholder={t('apikeys.audioEnginePlaceholder')}
                allowClear={false}
              />
            </Form.Item>
          )}

          <Form.Item label={t('apikeys.model')} extra={t('apikeys.modelExtensibilityHint')}>
            <Space.Compact block>
              <Form.Item name="model" noStyle>
                <AutoComplete
                  style={{ width: '100%' }}
                  options={editModelOptions}
                  onSelect={selectDiscoveredEditModel}
                  filterOption={(input, option) =>
                    String(option?.value ?? '').toLowerCase().includes(input.toLowerCase())}
                  placeholder={
                    editModelType === 'audio'
                      ? t('apikeys.audioVersionPlaceholder')
                      : t('apikeys.modelPlaceholder')
                  }
                />
              </Form.Item>
              <Tooltip title={t('apikeys.discoverModels')}>
                <Button
                  icon={<SyncOutlined />}
                  loading={discoveringEdit}
                  onClick={discoverEditModels}
                  disabled={editModelType === 'audio'}
                />
              </Tooltip>
            </Space.Compact>
          </Form.Item>

          {editModelType === 'embedding' && (
            <Form.Item
              name="dimensions"
              label={t('apikeys.embeddingDimensions')}
              extra={t('apikeys.embeddingDimensionsExtra')}
              rules={[{ required: true, message: t('common.required') }]}
            >
              <InputNumber min={1} max={32768} precision={0} style={{ width: '100%' }} />
            </Form.Item>
          )}

          {editModelType === 'audio' && (
            <>
              <Form.Item
                name="audio_generate_path"
                label={t('apikeys.audioGeneratePath')}
                extra={t('apikeys.audioEndpointHint')}
                rules={[
                  {
                    required: true,
                    message: t('common.required') + ' ' + t('apikeys.audioGeneratePath'),
                  },
                ]}
              >
                <Input placeholder={t('apikeys.audioGeneratePathPlaceholder')} />
              </Form.Item>
              <Form.Item
                name="audio_query_path"
                label={t('apikeys.audioQueryPath')}
                extra={t('apikeys.audioQueryEndpointHint')}
                rules={[
                  {
                    required: true,
                    message: t('common.required') + ' ' + t('apikeys.audioQueryPath'),
                  },
                ]}
              >
                <Input placeholder={t('apikeys.audioQueryPathPlaceholder')} />
              </Form.Item>
            </>
          )}

          {editModelType === 'chat' && (
            <>
              <Divider style={{ margin: '12px 0' }} />
              <Text strong style={{ display: 'block', marginBottom: 8 }}>
                {t('apikeys.modelCapabilities')}
              </Text>
              <Text type="secondary" style={{ display: 'block', fontSize: 12, marginBottom: 12 }}>
                {t('apikeys.modelCapabilitiesHint')}
              </Text>
              <CapabilitySummary
                resolution={editResolution}
                loading={resolvingEdit}
                probing={probingEdit}
                onProbe={probeEditModel}
              />
              {editResolution?.profile.reasoningEffort && (
                <Form.Item
                  name="reasoning_policy"
                  label={t('apikeys.reasoningPolicy')}
                  extra={t('apikeys.reasoningPolicyHint')}
                  style={{ marginTop: 12 }}
                >
                  <Select
                    style={{ width: '100%' }}
                    options={[
                      { value: 'auto', label: t('apikeys.reasoningPolicyValue.auto') },
                      { value: 'fast', label: t('apikeys.reasoningPolicyValue.fast') },
                      { value: 'standard', label: t('apikeys.reasoningPolicyValue.standard') },
                      { value: 'deep', label: t('apikeys.reasoningPolicyValue.deep') },
                      { value: 'maximum', label: t('apikeys.reasoningPolicyValue.maximum') },
                    ]}
                  />
                </Form.Item>
              )}
              <Divider titlePlacement="start" plain>
                {t('apikeys.manualOverrides')}
              </Divider>
              <Form.Item name="supports_reasoning_effort" valuePropName="checked" style={{ marginBottom: 8 }}>
                <Checkbox>{t('apikeys.supportsReasoningEffort')}</Checkbox>
              </Form.Item>
              <Form.Item
                noStyle
                shouldUpdate={(prev, cur) => prev.supports_reasoning_effort !== cur.supports_reasoning_effort}
              >
                {({ getFieldValue }) =>
                  getFieldValue('supports_reasoning_effort') ? (
                    <>
                      <Row gutter={12}>
                        <Col span={12}>
                          <Form.Item
                            name="reasoning_transport"
                            label={t('apikeys.reasoningTransport')}
                            initialValue="reasoning_effort"
                          >
                            <Select options={[
                              { value: 'reasoning_effort', label: 'reasoning_effort' },
                              { value: 'anthropic_thinking', label: 'Anthropic thinking' },
                              { value: 'thinking_level', label: 'thinking_level' },
                              { value: 'enable_thinking', label: 'enable_thinking' },
                            ]} />
                          </Form.Item>
                        </Col>
                        <Col span={12}>
                          <Form.Item
                            name="reasoning_effort_values"
                            label={t('apikeys.reasoningSupportedValues')}
                          >
                            <Select mode="tags" options={reasoningEffortOptions(editResolution)} />
                          </Form.Item>
                        </Col>
                      </Row>
                      <Form.Item
                        name="reasoning_effort_default"
                        label={t('apikeys.reasoningEffortDefault')}
                        style={{ marginBottom: 8 }}
                      >
                        <Select options={reasoningEffortOptions(editResolution)} />
                      </Form.Item>
                    </>
                  ) : null
                }
              </Form.Item>
              <Form.Item name="include_reasoning" valuePropName="checked" style={{ marginBottom: 8 }}>
                <Checkbox>{t('apikeys.includeReasoning')}</Checkbox>
              </Form.Item>
              <Form.Item name="use_max_completion_tokens" valuePropName="checked" style={{ marginBottom: 0 }}>
                <Checkbox>{t('apikeys.useMaxCompletionTokens')}</Checkbox>
              </Form.Item>
              <Form.Item name="native_web_search_enabled" valuePropName="checked" style={{ marginTop: 8, marginBottom: 8 }}>
                <Checkbox>{t('apikeys.nativeWebSearchEnabled')}</Checkbox>
              </Form.Item>
              <Form.Item
                noStyle
                shouldUpdate={(prev, cur) => prev.native_web_search_enabled !== cur.native_web_search_enabled}
              >
                {({ getFieldValue }) =>
                  getFieldValue('native_web_search_enabled') ? (
                    <Row gutter={12}>
                      <Col span={12}>
                        <Form.Item
                          name="native_web_search_extra_body"
                          label={t('apikeys.nativeWebSearchExtraBody')}
                          extra={t('apikeys.nativeWebSearchExtraBodyHint')}
                        >
                          <Input.TextArea
                            rows={4}
                            placeholder={'{}'}
                          />
                        </Form.Item>
                      </Col>
                      <Col span={12}>
                        <Form.Item
                          name="native_web_search_tool_template"
                          label={t('apikeys.nativeWebSearchToolTemplate')}
                          extra={t('apikeys.nativeWebSearchToolTemplateHint')}
                        >
                          <Input.TextArea
                            rows={4}
                            placeholder={'{"type":"web_search_preview"}'}
                          />
                        </Form.Item>
                      </Col>
                    </Row>
                  ) : null
                }
              </Form.Item>
              <Row gutter={12} style={{ marginTop: 12 }}>
                <Col span={12}>
                  <Form.Item
                    name="context_window_tokens"
                    label={t('apikeys.contextWindowTokens')}
                    extra={t('apikeys.tokenLimitAutoHint')}
                  >
                    <Input type="number" min={1024} placeholder={t('apikeys.autoDetect')} />
                  </Form.Item>
                </Col>
                <Col span={12}>
                  <Form.Item
                    name="max_output_tokens"
                    label={t('apikeys.maxOutputTokens')}
                    extra={t('apikeys.tokenLimitAutoHint')}
                  >
                    <Input type="number" min={1024} placeholder={t('apikeys.autoDetect')} />
                  </Form.Item>
                </Col>
              </Row>
            </>
          )}

          {/* Scenario selector in edit drawer */}
          <Form.Item
            name="scenarios"
            label={t('apikeys.scenarios')}
            extra={
              <Text type="secondary" style={{ fontSize: 12 }}>
                {editModelType === 'embedding'
                  ? t('apikeys.embeddingScenariosLocked')
                  : t('apikeys.scenariosExtra')}
              </Text>
            }
            rules={[
              {
                validator: (_, value) =>
                  Array.isArray(value) && value.length > 0
                    ? Promise.resolve()
                    : Promise.reject(new Error(t('apikeys.scenariosRequired'))),
              },
            ]}
          >
            <Checkbox.Group disabled={editModelType === 'embedding'}>
              <Space direction="vertical">
                {ALL_SCENARIOS.map((s) => (
                  <Checkbox key={s} value={s}>
                    <Tag color={scenarioTagColor(s)} style={{ marginLeft: 4 }}>{scenarioLabel(s)}</Tag>
                  </Checkbox>
                ))}
              </Space>
            </Checkbox.Group>
          </Form.Item>

          <Form.Item
            name="key_value"
            label={t('apikeys.keyValue')}
            extra={t('apikeys.newKeyHint')}
          >
            <Input.Password
              placeholder={t('apikeys.newKeyHint')}
              iconRender={(visible) =>
                visible ? <EyeOutlined /> : <EyeInvisibleOutlined />
              }
            />
          </Form.Item>

          <Row gutter={12}>
            <Col span={8}>
              <Form.Item name="daily_limit" label={t('apikeys.dailyLimit')}>
                <Input type="number" placeholder={t('apikeys.noLimit')} min={0} />
              </Form.Item>
            </Col>
            <Col span={8}>
              <Form.Item name="monthly_limit" label={t('apikeys.monthlyLimit')}>
                <Input type="number" placeholder={t('apikeys.noLimit')} min={0} />
              </Form.Item>
            </Col>
            <Col span={8}>
              <Form.Item name="priority" label={t('apikeys.priority')}>
                <Input type="number" placeholder="0" min={0} />
              </Form.Item>
            </Col>
          </Row>

          <Divider style={{ margin: '12px 0' }} />

          <Row gutter={12}>
            <Col span={12}>
              <Form.Item name="input_price_per_million" label={t('apikeys.inputPriceLabel')}>
                <Input
                  type="number"
                  placeholder={t('apikeys.pricePlaceholder')}
                  min={0}
                  step={0.01}
                  prefix="$"
                  suffix="/1M"
                />
              </Form.Item>
            </Col>
            <Col span={12}>
              <Form.Item name="output_price_per_million" label={t('apikeys.outputPriceLabel')}>
                <Input
                  type="number"
                  placeholder={t('apikeys.pricePlaceholder')}
                  min={0}
                  step={0.01}
                  prefix="$"
                  suffix="/1M"
                />
              </Form.Item>
            </Col>
          </Row>

          <Divider style={{ margin: '12px 0' }} />

          <Form.Item
            name="enabled"
            label={t('apikeys.status')}
            valuePropName="checked"
          >
            <Switch
              checkedChildren={t('apikeys.enabled')}
              unCheckedChildren={t('apikeys.disabled')}
            />
          </Form.Item>
        </Form>
      </Drawer>

      {/* ── Stats Drawer ──────────────────────────────────────────────────── */}
      <Drawer
        title={
          <Space>
            <BarChartOutlined />
            <span>{t('apikeys.usageStats')}</span>
            <Text type="secondary" style={{ fontWeight: 400, fontSize: 13 }}>
              {statsData?.key_id ? filteredKeys.find((k) => k.id === statsData.key_id)?.name : ''}
            </Text>
          </Space>
        }
        open={!!statsKeyId}
        onClose={() => setStatsKeyId(null)}
        width={560}
      >
        {statsData ? (
          <div>
            <Row gutter={16} style={{ marginBottom: 20 }}>
              <Col span={8}>
                <Statistic
                  title={t('apikeys.stats.totalCalls')}
                  value={statsData.total_calls}
                  valueStyle={{ fontSize: 24 }}
                />
              </Col>
              <Col span={8}>
                <Statistic
                  title={t('apikeys.stats.totalTokens')}
                  value={statsData.total_tokens}
                  valueStyle={{ fontSize: 24 }}
                />
              </Col>
              <Col span={8}>
                <Statistic
                  title={t('apikeys.stats.totalCost')}
                  value={statsData.total_cost_usd}
                  prefix="$"
                  valueStyle={{ fontSize: 24, color: '#3fb950' }}
                />
              </Col>
            </Row>

            <Title level={5} style={{ margin: '0 0 12px' }}>{t('apikeys.stats.dailyUsage')}</Title>
            {statsData.daily_usage && statsData.daily_usage.length > 0 ? (
              <Table
                size="small"
                rowKey="date"
                pagination={false}
                columns={[
                  { title: t('apikeys.stats.date'), dataIndex: 'date', key: 'date' },
                  { title: t('apikeys.stats.calls'), dataIndex: 'calls', key: 'calls', align: 'right' as const },
                  {
                    title: t('apikeys.stats.tokens'),
                    dataIndex: 'tokens_used',
                    key: 'tokens_used',
                    align: 'right' as const,
                    render: (v: number) => v?.toLocaleString(),
                  },
                  {
                    title: t('apikeys.stats.cost'),
                    dataIndex: 'cost_usd',
                    key: 'cost_usd',
                    align: 'right' as const,
                    render: (v: number) => `$${v?.toFixed(4)}`,
                  },
                ]}
                dataSource={statsData.daily_usage}
              />
            ) : (
              <Text type="secondary">{t('common.noData')}</Text>
            )}
          </div>
        ) : (
          <div style={{ textAlign: 'center', padding: 40 }}>
            <Text type="secondary">{t('common.loading')}</Text>
          </div>
        )}
      </Drawer>

      {/* ── Key Rotation Modal ─────────────────────────────────────────────── */}
      <Modal
        title={
          <Space>
            <SafetyCertificateOutlined />
            {t('apikeys.rotateTitle')}
          </Space>
        }
        open={rotateOpen}
        onCancel={() => { setRotateOpen(false); setRotatingKey(null); rotateForm.resetFields(); }}
        footer={null}
        width={480}
      >
        <Alert
          type="info"
          message={t('apikeys.rotateHint')}
          showIcon
          style={{ marginBottom: 16 }}
        />
        <Form
          form={rotateForm}
          layout="vertical"
          onFinish={(values) => {
            updateMut.mutate({
              id: rotatingKey!.id,
              values: {
                name: values.name,
                key_value: values.key_value,
              },
            });
            setRotateOpen(false);
            setRotatingKey(null);
          }}
        >
          <Form.Item name="name" label={t('apikeys.keyName')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item
            name="key_value"
            label={t('apikeys.newKeyValue')}
            rules={[{ required: true, message: t('common.required') }]}
            extra={t('apikeys.newKeyValueExtra')}
          >
            <Input.Password placeholder={t('apikeys.newKeyPlaceholder')} />
          </Form.Item>
          <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
            <Button type="primary" htmlType="submit" loading={updateMut.isPending}>
              {t('apikeys.rotateConfirm')}
            </Button>
          </Space>
        </Form>
      </Modal>

      {/* Expiry Warning Banner */}
      {(() => {
        const soon = filteredKeys.filter((k) => {
          if (!k.expires_at) return false;
          const days = Math.ceil((new Date(k.expires_at!).getTime() - Date.now()) / 86400000);
          return days <= 30 && days >= 0;
        });
        const expired = filteredKeys.filter((k) => {
          if (!k.expires_at) return false;
          return new Date(k.expires_at!) < new Date();
        });
        if (expired.length === 0 && soon.length === 0) return null;
        return (
          <Alert
            type={expired.length > 0 ? 'error' : 'warning'}
            message={
              expired.length > 0
                ? t('apikeys.expiredBanner', { count: expired.length })
                : t('apikeys.expiringBanner', { count: soon.length })
            }
            description={
              <Space wrap>
                {[...expired, ...soon].slice(0, 5).map((k) => (
                  <Tag key={k.id} color={expired.includes(k) ? 'red' : 'orange'}>
                    {k.name}
                  </Tag>
                ))}
                {expired.length + soon.length > 5 && (
                  <Text type="secondary">+{expired.length + soon.length - 5} more</Text>
                )}
              </Space>
            }
            showIcon
            style={{ marginTop: 16 }}
          />
        );
      })()}
    </div>
  );
}
